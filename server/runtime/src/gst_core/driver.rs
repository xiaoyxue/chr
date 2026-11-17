//! The core's CG branch: every captured frame goes to the tracking worker; once
//! its camera is back, before its slot's deadline, the render worker
//! composites the frame with the program scene's current snapshot, and the
//! rendered frame is offered to the selector for that frame's slot. A frame
//! whose camera or render misses the deadline simply goes out as its clean
//! feed. Both workers are supervised as in the timer-paced loop, their sockets
//! being bins of the core's pipeline.

use std::{collections::HashMap, time::Duration, time::Instant};

use chr_common::{methods::tracking::TrackingFrame, scene_frame as w};
use chr_tracker::{
    join::{CapturedFrame, DeadlineJoin, TrackingFeed},
    lens::LensSelection,
    pose::Tracked,
};
use gstreamer as gst;
use serde_json::json;

use super::{Core, Error, capture::Captured, output::Summary};
use crate::{
    program::synthetic::{Synthetic, id},
    retained::SceneControl,
    worker::{
        supervisor::{Miss, Supervisor},
        tracking::supervisor::Tracker,
    },
};

/// How often the driver advances supervision when no frame arrives.
const TICK: Duration = Duration::from_millis(2);
/// How often the driver logs the core's statistics.
const REPORT: Duration = Duration::from_secs(10);

/// What a render request needs of the installed program.
pub struct Program {
    control: SceneControl,
    target: serde_json::Value,
    stream_id: w::Id,
    timeline_id: w::Id,
    image_ref: w::Id,
    tracked: Tracked,
    image: w::ImageDescription,
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
        })
    }

    /// The scene as it is now, advanced to frame `sequence`.
    async fn scene(&self, sequence: u64) -> Result<w::SceneSnapshot, Error> {
        self.control
            .advance_rotation(sequence)
            .await
            .map_err(|error| error.message)?;
        let snapshot = self
            .control
            .call(w::SCENE_GET, Some(self.target.clone()))
            .await
            .map_err(|error| error.message)?;
        Ok(serde_json::from_value(snapshot)?)
    }

    /// Frame `sequence` with its camera, as the render worker takes it.
    fn frame(&self, sequence: u64, camera: &TrackingFrame) -> w::FrameInput {
        w::FrameInput {
            frame: w::FrameKey {
                stream_id: self.stream_id.clone(),
                sequence: sequence.into(),
            },
            image_ref: self.image_ref.clone(),
            position: w::Position {
                timeline_id: self.timeline_id.clone(),
                ticks: sequence.into(),
            },
            geometry: self.tracked.geometry(camera),
        }
    }
}

/// Frames that got a camera but no render request.
#[derive(Debug, Default)]
struct Unrendered {
    not_ready: u64,
    no_credit: u64,
    failed: u64,
    scene: u64,
}

pub struct Driver<'a> {
    core: &'a Core,
    tracker: Tracker,
    render: Supervisor,
    feed: TrackingFeed,
    join: DeadlineJoin<(gst::Memory, gst::ClockTime)>,
    lenses: LensSelection,
    program: Program,
    /// Render requests in flight: their frame's sequence and capture time.
    rendering: HashMap<u64, (u64, gst::ClockTime)>,
    unrendered: Unrendered,
    /// Time left until each rendered frame's slot is decided, when it was
    /// submitted: the render's budget, since the last report.
    budget: Summary,
}

impl<'a> Driver<'a> {
    pub fn new(
        core: &'a Core,
        tracker: Tracker,
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
            feed,
            lenses,
            program,
            rendering: HashMap::new(),
            unrendered: Unrendered::default(),
            budget: Summary::default(),
        }
    }

    /// Runs until the capture channel closes or the core fails.
    pub async fn run(
        mut self,
        mut frames: tokio::sync::mpsc::Receiver<Captured>,
    ) -> Result<(), Error> {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut report = tokio::time::interval_at(tokio::time::Instant::now() + REPORT, REPORT);
        loop {
            tokio::select! {
                frame = frames.recv() => self.captured(frame.ok_or("the capture channel closed")?),
                _ = tick.tick() => {}
                _ = report.tick() => {
                    let budget = std::mem::take(&mut self.budget);
                    let tracking = self.feed.status();
                    log::info!(
                        "{}; tracking: {} joined, {} without a camera; render budget at submit {} over {} frames; not rendered: {:?}",
                        self.core.report(),
                        tracking.frames_joined,
                        tracking.frames_clean_feed,
                        budget.ms(),
                        budget.count,
                        self.unrendered
                    );
                }
            }
            self.step().await?;
        }
    }

    fn captured(&mut self, frame: Captured) {
        let sent = self
            .tracker
            .track_memory(frame.sequence, frame.memory.clone());
        let deadline = self
            .core
            .timeline()
            .instant(self.core.deadline(frame.capture));
        self.join.frame(
            CapturedFrame {
                sequence: frame.sequence,
                rgba: (frame.memory, frame.capture),
                arrived: Instant::now(),
            },
            deadline,
        );
        if !sent {
            // No answer will come: it must not hold back the frames after it.
            self.join.without_camera(frame.sequence);
        }
    }

    async fn step(&mut self) -> Result<(), Error> {
        if self.core.maintain()? {
            self.join.reset();
            self.tracker.forget();
        }
        self.join.accept(self.tracker.poll());
        for (frame, camera) in self.join.joined(Instant::now()) {
            self.render(frame, camera).await;
        }
        for completion in self.render.completions() {
            let Some((sequence, capture)) = self.rendering.remove(&completion.id) else {
                continue;
            };
            match completion.result {
                Ok(frame) => self.core.offer_cg(sequence, capture, frame),
                Err(miss) => {
                    log::debug!("frame {sequence} not rendered: {miss:?}");
                    self.unrendered.failed += 1;
                }
            }
        }
        Ok(())
    }

    async fn render(
        &mut self,
        frame: CapturedFrame<(gst::Memory, gst::ClockTime)>,
        camera: TrackingFrame,
    ) {
        let (camera, model) = self.lenses.apply(camera);
        self.feed.record_model(model);
        let sequence = frame.sequence;
        let (memory, capture) = frame.rgba;
        let scene = match self.program.scene(sequence).await {
            Ok(scene) => scene,
            Err(error) => {
                log::warn!("frame {sequence} not rendered: no scene snapshot: {error}");
                self.unrendered.scene += 1;
                return;
            }
        };
        let input = self.program.frame(sequence, &camera);
        match self
            .render
            .submit(&self.program.image, memory, &input, &scene)
        {
            Ok(id) => {
                self.rendering.insert(id, (sequence, capture));
                let left = self.core.deadline(capture).nseconds() as i64
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
