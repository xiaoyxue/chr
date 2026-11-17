//! The core's CG branch: every captured frame goes to the tracking worker (or,
//! without one, waits for its camera from `tracking.frame`); once its camera is
//! in, before its slot's deadline, the render worker composites the frame with
//! the program scene's current snapshot, and the rendered frame is offered to
//! the selector for that frame's slot. A frame whose camera or render misses
//! the deadline simply goes out as its clean feed. Both workers are supervised,
//! their sockets being bins of the core's pipeline.
//!
//! The driver also publishes the core's telemetry in `sdi.get_status` (#131):
//! the program delay, what the slots got, their slack and the render budget.
//!
//! The captured frames' format may change. A frame in a new format starts a new
//! tracking stream (the tracking worker is restarted with the new caps; the
//! frames it still held go out as clean feed), and every render request
//! carries its frame's extent and time base. Frames whose extent is not the
//! program camera's go out as clean feed: the program's camera and lens are
//! defined at its native extent.

use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use chr_common::{
    methods::{
        sdi::{CoreStatus, CoreWindow, Spread},
        tracking::TrackingFrame,
    },
    rate::Rate,
    scene_frame as w,
};
use chr_tracker::{
    join::{CapturedFrame, DeadlineJoin, TrackingFeed},
    lens::LensSelection,
    pose::Tracked,
};
use gstreamer as gst;
use serde_json::json;

use super::{Core, DECISION_MARGIN, Error, capture::Captured, output::Summary, select::Frame};
use crate::{
    program::synthetic::{Synthetic, id},
    retained::SceneControl,
    sdi::VideoMode,
    worker::{
        supervisor::{Miss, Supervisor},
        tracking::supervisor::{Tracker, in_flight_limit},
    },
};

/// How often the driver advances supervision when no frame arrives.
const TICK: Duration = Duration::from_millis(2);
/// How often the driver logs the core's statistics: the telemetry's window.
const REPORT: Duration = Duration::from_secs(10);
/// How often the driver refreshes the telemetry's counters.
const STATUS: Duration = Duration::from_secs(1);

/// Render requests time their frames in nanoseconds of capture time, so the
/// media time runs on across changes of rate.
fn nanoseconds() -> w::TimeBase {
    w::TimeBase::new(1, 1_000_000_000).expect("a reduced time base")
}

/// What a render request needs of the installed program.
pub struct Program {
    control: SceneControl,
    target: serde_json::Value,
    stream_id: w::Id,
    timeline_id: w::Id,
    image_ref: w::Id,
    tracked: Tracked,
    image: w::ImageDescription,
    /// The rate the program's scene animations step at.
    rate: Rate,
}

impl Program {
    /// The tracked program `synthetic`, whose scene `control` owns.
    pub fn new(synthetic: &Synthetic, control: SceneControl) -> Result<Self, Error> {
        let runtime = &synthetic.configuration.runtime_id;
        Ok(Self {
            control,
            target: json!({"runtime_id": runtime, "scene_id": synthetic.scene_id}),
            stream_id: synthetic.stream_id.clone(),
            timeline_id: id(format!("{}-timeline", runtime.as_str()))?,
            image_ref: id(format!("{}-capture", runtime.as_str()))?,
            tracked: synthetic
                .tracking
                .clone()
                .ok_or("the GStreamer core needs a tracked program")?,
            image: w::ImageDescription {
                extent: synthetic.configuration.native_extent,
                format: w::ImageFormat::Rgba8Unorm,
                transfer: w::Transfer::Bt709,
                alpha: w::Alpha::Straight,
            },
            rate: Rate::from_time_base(synthetic.configuration.output_rate)?,
        })
    }

    /// Whether frames of `format` can be rendered: they have the program
    /// camera's extent.
    fn renders(&self, format: VideoMode) -> bool {
        (format.width, format.height) == (self.image.extent.width, self.image.extent.height)
    }

    /// The scene as it is now, advanced to the program frame at running time
    /// `at` ns: animations step at the program's rate, whatever the input's.
    async fn scene(&self, at: u64) -> Result<w::SceneSnapshot, Error> {
        self.control
            .advance_rotation(self.rate.frame_at(at))
            .await
            .map_err(|error| error.message)?;
        let snapshot = self
            .control
            .call(w::SCENE_GET, Some(self.target.clone()))
            .await
            .map_err(|error| error.message)?;
        Ok(serde_json::from_value(snapshot)?)
    }

    /// Frame `key` with its camera, as the render worker takes it; its
    /// position counts [`nanoseconds`] of running time.
    fn frame(&self, key: Frame, camera: &TrackingFrame) -> w::FrameInput {
        w::FrameInput {
            frame: w::FrameKey {
                stream_id: self.stream_id.clone(),
                sequence: key.sequence.into(),
            },
            image_ref: self.image_ref.clone(),
            position: w::Position {
                timeline_id: self.timeline_id.clone(),
                ticks: key.capture.nseconds().into(),
            },
            geometry: self.tracked.geometry(camera),
        }
    }
}

/// Frames that got a camera but no render request, and frames that were not
/// tracked because the program cannot render their extent.
#[derive(Debug, Default)]
struct Unrendered {
    not_ready: u64,
    no_credit: u64,
    /// Joined, but its slot was decided before the render worker had a credit.
    expired: u64,
    failed: u64,
    scene: u64,
    other_extent: u64,
}

pub struct Driver<'a> {
    core: &'a Core,
    /// The tracking worker; without one, cameras come from `tracking.frame`.
    tracker: Option<Tracker>,
    render: Supervisor,
    feed: TrackingFeed,
    join: DeadlineJoin<(gst::Memory, Frame)>,
    /// Joined frames waiting for a render credit, in capture order: a tracking
    /// worker answers in batches (vchao's `--batch-frames`), more at once than
    /// the render worker takes.
    joined: VecDeque<(CapturedFrame<(gst::Memory, Frame)>, TrackingFrame)>,
    lenses: LensSelection,
    program: Program,
    /// The format of the frames the tracking worker gets now.
    tracking: Option<VideoMode>,
    /// Render requests in flight: their frame.
    rendering: HashMap<u64, Frame>,
    unrendered: Unrendered,
    /// Time left until each rendered frame's slot is decided, when it was
    /// submitted: the render's budget, since the last report.
    budget: Summary,
    /// The last complete report window, for the telemetry.
    window: Option<CoreWindow>,
}

impl<'a> Driver<'a> {
    pub fn new(
        core: &'a Core,
        tracker: Option<Tracker>,
        render: Supervisor,
        feed: TrackingFeed,
        lenses: LensSelection,
        program: Program,
        capacity: usize,
    ) -> Self {
        Self {
            core,
            tracker,
            render,
            join: DeadlineJoin::new(feed.clone(), capacity),
            joined: VecDeque::new(),
            feed,
            lenses,
            program,
            tracking: None,
            rendering: HashMap::new(),
            unrendered: Unrendered::default(),
            budget: Summary::default(),
            window: None,
        }
    }

    /// Runs until the capture channel closes or the core fails.
    pub async fn run(
        mut self,
        mut frames: tokio::sync::mpsc::Receiver<Captured>,
    ) -> Result<(), Error> {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let start = tokio::time::Instant::now();
        let mut report = tokio::time::interval_at(start + REPORT, REPORT);
        let mut status = tokio::time::interval_at(start + STATUS, STATUS);
        let mut window_start = start;
        loop {
            tokio::select! {
                frame = frames.recv() => self.captured(frame.ok_or("the capture channel closed")?),
                _ = tick.tick() => {}
                _ = status.tick() => self.publish(),
                _ = report.tick() => {
                    let budget = std::mem::take(&mut self.budget);
                    let tracking = self.feed.status();
                    let report = self.core.report();
                    log::info!(
                        "{report}; tracking: {} joined, {} without a camera; render budget at submit {} over {} frames; not rendered: {:?}",
                        tracking.frames_joined,
                        tracking.frames_clean_feed,
                        budget.ms(),
                        budget.count,
                        self.unrendered
                    );
                    let now = tokio::time::Instant::now();
                    self.window = Some(CoreWindow {
                        seconds: (now - window_start).as_secs_f64(),
                        cg_slots: report.window.cg,
                        clean_slots: report.window.clean,
                        cg_ratio: ratio(report.window.cg, report.window.clean),
                        slack_ms: spread(&report.slack),
                        render_budget_ms: spread(&budget),
                        delay_ms: spread(&report.window.delay),
                    });
                    window_start = now;
                    self.publish();
                }
            }
            self.step().await?;
        }
    }

    /// Publishes the core's telemetry in `sdi.get_status`.
    fn publish(&self) {
        let counters = self.core.selector_counters();
        let ms = |duration: Duration| duration.as_secs_f64() * 1e3;
        self.core.config().recorder.core(CoreStatus {
            program_delay_ms: ms(self.core.config().program_delay),
            decision_margin_ms: ms(DECISION_MARGIN),
            cg_slots: counters.cg,
            clean_slots: counters.clean,
            empty_slots: counters.empty,
            late_cg: counters.late_cg,
            late_decisions: counters.late_decisions,
            cg_ratio: ratio(counters.cg, counters.clean),
            window: self.window.clone(),
        });
    }

    fn captured(&mut self, frame: Captured) {
        let key = frame.key;
        if !self.program.renders(frame.format) {
            // Its slot gets its clean feed.
            self.unrendered.other_extent += 1;
            return;
        }
        if self.tracking != Some(frame.format) {
            self.new_format(frame.format);
        }
        let sent = match &mut self.tracker {
            Some(tracker) => tracker.track_memory(key.sequence, frame.memory.clone()),
            None => {
                // tracking.frame counts in frame_ids of the input's newest stream.
                self.feed.rebase(self.core.stream_base());
                true
            }
        };
        let deadline = self
            .core
            .timeline()
            .instant(self.core.deadline(key.capture));
        self.join.frame(
            CapturedFrame {
                sequence: key.sequence,
                rgba: (frame.memory, key),
                arrived: Instant::now(),
            },
            deadline,
        );
        if !sent {
            // No answer will come: it must not hold back the frames after it.
            self.join.without_camera(key.sequence);
        }
    }

    /// Frames in `format` from now on: a new tracking stream. The frames the
    /// old one holds get no camera, so their slots get their clean feed.
    fn new_format(&mut self, format: VideoMode) {
        if self.tracking.is_some() {
            self.join.reset();
            self.joined.clear();
        }
        if let Some(tracker) = &mut self.tracker {
            let in_flight = in_flight_limit(self.core.config().program_delay, format.fps);
            tracker.reconfigure(format.width, format.height, format.fps, in_flight);
        }
        self.tracking = Some(format);
    }

    async fn step(&mut self) -> Result<(), Error> {
        if self.core.maintain()? {
            self.join.reset();
            self.joined.clear();
            if let Some(tracker) = &mut self.tracker {
                tracker.forget();
            }
        }
        if let Some(tracker) = &mut self.tracker {
            self.join.accept(tracker.poll());
        }
        let completions = self.render.completions();
        for completion in completions {
            let Some(key) = self.rendering.remove(&completion.id) else {
                continue;
            };
            match completion.result {
                Ok(frame) => self.core.offer_cg(key, frame),
                Err(miss) => {
                    log::debug!("frame {} not rendered: {miss:?}", key.sequence);
                    self.unrendered.failed += 1;
                }
            }
        }
        self.joined.extend(self.join.joined(Instant::now()));
        while let Some((frame, _)) = self.joined.front() {
            let key = frame.rgba.1;
            if self.core.timeline().now() >= self.core.deadline(key.capture) {
                self.joined.pop_front();
                self.unrendered.expired += 1;
                continue;
            }
            if self.render.is_ready() && !self.render.has_credit() {
                // A credit comes back with the next completed render.
                break;
            }
            let (frame, camera) = self.joined.pop_front().expect("front exists");
            self.render(frame, camera).await;
        }
        Ok(())
    }

    async fn render(&mut self, frame: CapturedFrame<(gst::Memory, Frame)>, camera: TrackingFrame) {
        let (camera, model) = self.lenses.apply(camera);
        self.feed.record_model(model);
        let sequence = frame.sequence;
        let (memory, key) = frame.rgba;
        let scene = match self.program.scene(key.capture.nseconds()).await {
            Ok(scene) => scene,
            Err(error) => {
                log::warn!("frame {sequence} not rendered: no scene snapshot: {error}");
                self.unrendered.scene += 1;
                return;
            }
        };
        let input = self.program.frame(key, &camera);
        match self
            .render
            .submit(&self.program.image, memory, &input, &scene, nanoseconds())
        {
            Ok(id) => {
                self.rendering.insert(id, key);
                let left = self.core.deadline(key.capture).nseconds() as i64
                    - self.core.timeline().now().nseconds() as i64;
                self.budget.add(left);
            }
            Err(Miss::NotReady) => self.unrendered.not_ready += 1,
            Err(Miss::NoCredit) => self.unrendered.no_credit += 1,
            Err(miss) => {
                log::debug!("frame {sequence} not rendered: {miss:?}");
                self.unrendered.failed += 1;
            }
        }
    }
}

/// `cg / (cg + clean)`; `None` without slots.
fn ratio(cg: u64, clean: u64) -> Option<f64> {
    (cg + clean > 0).then(|| cg as f64 / (cg + clean) as f64)
}

/// A summary of nanosecond values in ms; `None` without values.
fn spread(summary: &Summary) -> Option<Spread> {
    (summary.count > 0).then(|| Spread {
        min: summary.min as f64 / 1e6,
        mean: summary.mean() / 1e6,
        max: summary.max as f64 / 1e6,
        count: summary.count,
    })
}
