//! chr-server's GStreamer core (#154, phases 1 and 2, experimental): `--pipeline gst`.
//!
//! One GStreamer pipeline carries the program from the capture process to the
//! output, timed by the pipeline clock (the system clock in simulation, the
//! output card's in phase 3) instead of the timer-paced render loop:
//!
//! ```text
//! INPUT (chr-sdi-in) ─unixfd─► capture bin: unixfdsrc ! RGBA ! tee
//!   ├─ CG branch:    appsink ─► tracking hop ─► camera ─► render hop ─┐  (driver)
//!   └─ clean branch: videoconvertscale ! UYVY ! appsink ─────────────┤
//!                                                    selector (per slot, deadline)
//! program: appsrc ! livesync ! tee ─┬─ queue ! appsink sync=true   (OUTPUT, cadence)
//!                                   └─ queue leaky ! appsink        (program tap)
//! hop bins: appsrc ! unixfdsink / unixfdsrc ! appsink per worker session
//! ```
//!
//! - **Timing.** A frame captured at running time `t` is due on the output at
//!   `t + D`, the program delay. The [`select`]or decides each slot at
//!   `t + D - DECISION_MARGIN`: the rendered frame if it is there, else that
//!   frame's own clean feed. `livesync` emits every slot at its time, repeating
//!   the previous frame only when no frame came (input loss), and the output
//!   sink renders it on the clock.
//! - **Frames** stay in the capture process's memfds end to end: the same
//!   memory goes to the tracking and render workers.
//! - **Workers** keep their protocols (VchaoFrame for tracking, render protocol
//!   v3 with the scene snapshot) and their supervisors; their sockets are bins
//!   of this pipeline ([`Host::Bin`]), added for each worker session and
//!   removed with it, so a restarted worker is relinked into the running
//!   pipeline. A dead or stalled worker only costs CG: its frames' slots get
//!   clean feed.
//! - **Capture** is a bin too, rebuilt when the capture process goes away.
//! - **Rates and formats** come from the capture's caps and may change at any
//!   time: deadlines do not depend on the rate, the program follows the input's
//!   rate ([`OutputRate::Follow`]) or converts it to a fixed one
//!   ([`OutputRate::Fixed`]), and a new format starts a new tracking stream
//!   (`server/docs/gst-core.md`).
//!
//! `fallbackswitch` is not used: it switches streams after a timeout, so a
//! single late CG frame costs output slots and never shows that slot's clean
//! frame (`server/docs/gst-core.md`).

pub mod capture;
pub mod driver;
pub mod input;
pub mod output;
pub mod select;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use chr_common::rate::Rate;

use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSrc};

use crate::{
    sdi::{SdiRecorder, VideoMode},
    worker::transport::{self, Host},
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// Name of the custom meta on every program frame: `cg` (bool), the
/// frame's capture `sequence` and its capture time `source-ns` (u64).
pub const PROGRAM_META: &str = "chr-program-frame";
/// The selector decides a slot this long before it is due, leaving the time to
/// hand the frame through `livesync` to the output.
pub const DECISION_MARGIN: Duration = Duration::from_millis(4);

/// Registers the metas the core reads and writes; idempotent.
pub fn init() -> Result<(), Error> {
    static INIT: OnceLock<()> = OnceLock::new();
    transport::init()?;
    INIT.get_or_init(|| gst::meta::CustomMeta::register(PROGRAM_META, &[]));
    Ok(())
}

/// The pipeline clock seen as running time, the time base of every timestamp
/// in the core.
#[derive(Clone, Debug)]
pub struct Timeline {
    clock: gst::Clock,
    base: gst::ClockTime,
}

impl Timeline {
    pub fn of(pipeline: &gst::Pipeline) -> Result<Self, Error> {
        Ok(Self {
            clock: pipeline.clock().ok_or("the pipeline has no clock")?,
            base: pipeline
                .base_time()
                .ok_or("the pipeline has no base time")?,
        })
    }

    /// The current running time.
    pub fn now(&self) -> gst::ClockTime {
        self.clock.time().saturating_sub(self.base)
    }

    /// The clock time of running time `running`: for a captured frame, its
    /// capture time in the same-host clock domain the capture process stamped.
    pub fn absolute(&self, running: gst::ClockTime) -> gst::ClockTime {
        self.base + running
    }

    /// The instant at which the clock reaches running time `running`.
    pub fn instant(&self, running: gst::ClockTime) -> Instant {
        let now = Instant::now();
        let current = self.now();
        if running >= current {
            now + Duration::from_nanos((running - current).nseconds())
        } else {
            now.checked_sub(Duration::from_nanos((current - running).nseconds()))
                .unwrap_or(now)
        }
    }
}

/// The program output's rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputRate {
    /// The input's rate, changing with it.
    Follow,
    /// This rate whatever the input's: frames are repeated or dropped.
    Fixed(Rate),
}

impl std::str::FromStr for OutputRate {
    type Err = String;

    /// `follow` or `fixed:<rate>`.
    fn from_str(text: &str) -> Result<Self, String> {
        match text.split_once(':') {
            None if text == "follow" => Ok(Self::Follow),
            Some(("fixed", rate)) => Ok(Self::Fixed(rate.parse().map_err(|e| format!("{e}"))?)),
            _ => Err(format!(
                "the output rate is follow or fixed:<rate> (fixed:50, fixed:59.94), not {text:?}"
            )),
        }
    }
}

pub struct CoreConfig {
    /// The program camera's extent and nominal rate: frames of another extent
    /// go out as their clean feed.
    pub capture: VideoMode,
    /// Extent of the program output and its rate at start (the fixed rate,
    /// with [`OutputRate::Fixed`]).
    pub output: VideoMode,
    pub output_rate: OutputRate,
    /// The capture process's unixfd socket.
    pub capture_socket: PathBuf,
    /// Capture to output: every frame's slot is due this long after its capture.
    pub program_delay: Duration,
    /// The shared-memory program tap for chr-monitor.
    pub tap: Option<PathBuf>,
    /// Counters behind `sdi.get_status`.
    pub recorder: SdiRecorder,
}

/// The running pipeline: program output, selector and capture.
pub struct Core {
    pipeline: gst::Pipeline,
    timeline: Timeline,
    selector: select::Selector,
    output: Arc<output::Output>,
    capture: Mutex<CaptureState>,
    config: CoreConfig,
    frames: tokio::sync::mpsc::Sender<capture::Captured>,
    /// The numbering and formats of every capture session's streams.
    streams: Arc<capture::Streams>,
}

struct CaptureState {
    current: Option<capture::Capture>,
    retry_at: Instant,
    sessions: u64,
}

/// How long the core waits before it connects to the capture process again.
const CAPTURE_RETRY: Duration = Duration::from_millis(500);
/// Captured frames waiting for the driver.
const CAPTURED_QUEUE: usize = 16;

impl Core {
    /// Builds and plays the pipeline; captured frames for the CG branch come
    /// out of the returned receiver.
    pub fn start(
        config: CoreConfig,
    ) -> Result<(Self, tokio::sync::mpsc::Receiver<capture::Captured>), Error> {
        init()?;
        let pipeline = gst::Pipeline::with_name("chr-core");
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        let program = AppSrc::builder()
            .name("program")
            .caps(&uyvy_caps(config.output))
            .format(gst::Format::Time)
            .is_live(true)
            // Program frames carry their due time and arrive before it.
            .min_latency(0)
            .block(false)
            .max_buffers(0)
            .max_bytes(0)
            .max_time(gst::ClockTime::ZERO)
            .build();
        let livesync = gst::ElementFactory::make("livesync")
            .name("ratelock")
            .build()
            .map_err(|_| {
                "GStreamer has no livesync element; the core needs gst-plugins-rs' livesync \
                 on GST_PLUGIN_SYSTEM_PATH_1_0 (the Nix shells provide it)"
            })?;
        let tee = gst::ElementFactory::make("tee").name("out").build()?;
        let output_queue = queue(2, false)?;
        let output_sink = AppSink::builder()
            .name("output")
            .sync(true)
            .async_(false)
            .enable_last_sample(false)
            .max_buffers(2)
            .drop(true)
            .build();
        // livesync hands over each slot when it is due: no processing time to
        // add to the pipeline latency (basesink's default is 20 ms).
        output_sink.set_property("processing-deadline", 0_u64);
        let tap_queue = queue(2, true)?;
        let tap_sink = AppSink::builder()
            .name("tap")
            .sync(false)
            .async_(false)
            .enable_last_sample(false)
            .max_buffers(2)
            .drop(true)
            .build();
        pipeline.add_many([
            program.upcast_ref(),
            &livesync,
            &tee,
            &output_queue,
            output_sink.upcast_ref(),
            &tap_queue,
            tap_sink.upcast_ref(),
        ])?;
        gst::Element::link_many([program.upcast_ref(), &livesync, &tee])?;
        gst::Element::link_many([&tee, &output_queue, output_sink.upcast_ref()])?;
        gst::Element::link_many([&tee, &tap_queue, tap_sink.upcast_ref()])?;
        let tap = match &config.tap {
            Some(path) => Some(chr_shm::Writer::create(
                path,
                chr_shm::tap::layout(config.output.width, config.output.height),
            )?),
            None => None,
        };
        pipeline.set_state(gst::State::Playing)?;
        let timeline = Timeline::of(&pipeline)?;
        let output = Arc::new(output::Output::new(
            config.output,
            timeline.clone(),
            tap,
            config.recorder.clone(),
        ));
        output::attach(&output, &output_sink, &tap_sink);
        let selector = select::Selector::start(
            select::Program {
                appsrc: program,
                output: config.output,
                policy: config.output_rate,
                recorder: config.recorder.clone(),
            },
            timeline.clone(),
            select::Timing::new(config.program_delay),
        )?;
        let streams = Arc::new(capture::Streams::new(on_format(
            selector.clone(),
            config.recorder.clone(),
            config.output,
            config.output_rate,
        )));
        let (frames, captured) = tokio::sync::mpsc::channel(CAPTURED_QUEUE);
        log::info!(
            "GStreamer core: capture from {} (program camera {}), program {}x{} at {} delayed \
             {:?}, each slot decided {:?} before it is due",
            config.capture_socket.display(),
            config.capture.name(),
            config.output.width,
            config.output.height,
            match config.output_rate {
                OutputRate::Follow => "the input's rate".to_owned(),
                OutputRate::Fixed(rate) => format!("a fixed {} fps", rate.label()),
            },
            config.program_delay,
            DECISION_MARGIN,
        );
        Ok((
            Self {
                pipeline,
                timeline,
                selector,
                output,
                capture: Mutex::new(CaptureState {
                    current: None,
                    retry_at: Instant::now(),
                    sessions: 0,
                }),
                config,
                frames,
                streams,
            },
            captured,
        ))
    }

    /// Where workers' socket elements go: bins of this pipeline.
    pub fn host(&self) -> Host {
        Host::Bin(self.pipeline.clone())
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    pub fn config(&self) -> &CoreConfig {
        &self.config
    }

    /// The running time at which the slot of a frame captured at `capture` is decided.
    pub fn deadline(&self, capture: gst::ClockTime) -> gst::ClockTime {
        self.selector.timing().deadline(capture)
    }

    /// Offers a rendered frame for its slot.
    pub fn offer_cg(&self, key: select::Frame, frame: gst::Buffer) {
        self.selector.cg(key, frame);
    }

    /// Handles bus messages and keeps a capture connected. Returns true when a
    /// new capture session replaced a lost one: its frames number on from the
    /// next program sequence, and the old session's frames are done.
    pub fn maintain(&self) -> Result<bool, Error> {
        self.poll_bus()?;
        let mut state = self.capture.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(capture) = &state.current {
            if !capture.failed() {
                return Ok(false);
            }
            log::warn!(
                "capture from {} lost; reconnecting",
                self.config.capture_socket.display()
            );
            state.current = None;
            state.retry_at = Instant::now() + CAPTURE_RETRY;
        }
        if Instant::now() < state.retry_at || !self.config.capture_socket.exists() {
            return Ok(false);
        }
        state.retry_at = Instant::now() + CAPTURE_RETRY;
        match capture::Capture::open(
            &self.pipeline,
            &self.config,
            Arc::clone(&self.streams),
            self.frames.clone(),
            self.selector.clone(),
        ) {
            Ok(capture) => {
                state.sessions += 1;
                log::info!(
                    "capture session {} connected to {}",
                    state.sessions,
                    self.config.capture_socket.display()
                );
                state.current = Some(capture);
                Ok(state.sessions > 1)
            }
            Err(error) => {
                log::debug!("capture not connected: {error}");
                Ok(false)
            }
        }
    }

    /// Logs errors of hops and of the capture, which their owners recover
    /// from; any other error stops the core.
    fn poll_bus(&self) -> Result<(), Error> {
        let Some(bus) = self.pipeline.bus() else {
            return Ok(());
        };
        while let Some(message) =
            bus.pop_filtered(&[gst::MessageType::Error, gst::MessageType::Warning])
        {
            let source = message
                .src()
                .map(|src| src.path_string().to_string())
                .unwrap_or_default();
            // Path segments are `Type:name`; a removed bin's elements no longer
            // have the pipeline in their path.
            let recoverable = source.contains(":hop-") || source.contains(":capture-");
            match message.view() {
                gst::MessageView::Error(error) if recoverable => {
                    log::debug!("{source}: {}", error.error());
                }
                gst::MessageView::Error(error) => {
                    return Err(format!("GStreamer core: {source}: {}", error.error()).into());
                }
                gst::MessageView::Warning(warning) => {
                    log::warn!("{source}: {}", warning.error());
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// A summary of the output and the selector since the last report.
    pub fn report(&self) -> String {
        format!(
            "{}; {}",
            self.output.report(),
            self.selector.counters().report()
        )
    }

    pub fn selector_counters(&self) -> select::Counters {
        self.selector.counters()
    }

    pub fn output_totals(&self) -> output::Totals {
        self.output.totals()
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current = None;
        self.selector.stop();
        let _ = self.pipeline.set_state(gst::State::Null);
        log::info!(
            "GStreamer core totals: {}; {}",
            self.output.totals().report(),
            self.selector.counters().report()
        );
        if let Some(path) = &self.config.tap {
            // A monitor that still maps the tap keeps its pages until it lets go.
            let _ = std::fs::remove_file(path);
        }
    }
}

/// What the core does when the capture's format changes: a transition in the
/// selector, and the output's mode in the status.
fn on_format(
    selector: select::Selector,
    recorder: crate::sdi::SdiRecorder,
    output: VideoMode,
    policy: OutputRate,
) -> capture::OnFormat {
    Box::new(move |first, format, changed| {
        let program = match policy {
            OutputRate::Follow => VideoMode {
                fps: format.fps,
                ..output
            },
            OutputRate::Fixed(_) => output,
        };
        if changed {
            log::info!(
                "capture format now {} from frame {first}: program {}",
                format.name(),
                program.name()
            );
            selector.begin_transition(first);
            recorder.format_changed(program);
        } else {
            log::info!("capture format {} from frame {first}", format.name());
            recorder.set_mode(program);
        }
    })
}

/// Packed 8-bit UYVY, BT.709 limited range, at `mode`.
pub fn uyvy_caps(mode: VideoMode) -> gst::Caps {
    gst::Caps::builder("video/x-raw")
        .field("format", "UYVY")
        .field("width", mode.width as i32)
        .field("height", mode.height as i32)
        .field(
            "framerate",
            crate::worker::tracking::wire::fraction(mode.fps),
        )
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .field("interlace-mode", "progressive")
        .field("colorimetry", "bt709")
        .build()
}

/// A queue of at most `buffers` buffers; a `leaky` one drops its oldest
/// instead of blocking upstream.
fn queue(buffers: u32, leaky: bool) -> Result<gst::Element, Error> {
    let queue = gst::ElementFactory::make("queue")
        .property("max-size-buffers", buffers)
        .property("max-size-bytes", 0_u32)
        .property("max-size-time", 0_u64)
        .build()?;
    if leaky {
        queue.set_property_from_str("leaky", "downstream");
    }
    Ok(queue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_rates_parse() {
        assert_eq!("follow".parse(), Ok(OutputRate::Follow));
        assert_eq!(
            "fixed:59.94".parse(),
            Ok(OutputRate::Fixed(Rate::new(60000, 1001).unwrap()))
        );
        assert_eq!("fixed:50".parse(), Ok(OutputRate::Fixed(Rate::fps(50))));
        for bad in ["fixed", "fixed:", "fixed:0", "follow:50", "50", ""] {
            assert!(bad.parse::<OutputRate>().is_err(), "{bad}");
        }
    }
}
