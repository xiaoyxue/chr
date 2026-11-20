//! chr-server's GStreamer core (#154): the live program path of every tracked
//! program (`--tracked-scene`, `--description`).
//!
//! One GStreamer pipeline carries the program from the capture process to the
//! output, timed by the pipeline clock (the system clock in simulation, the
//! DeckLink output card's with `--output decklink`):
//!
//! ```text
//! INPUT (chr-sdi-in) ─unixfd─► capture bin: unixfdsrc ! RGBA ! tee
//!   ├─ CG branch:    appsink ─► tracking hop ─► camera ─► render hop ─┐  (driver)
//!   └─ clean branch: videoconvertscale ! UYVY ! appsink ─────────────┤
//!                                                    selector (per slot, deadline)
//! program: appsrc ! livesync ! tee ─┬─ queue ! appsink sync=true | decklinkvideosink (OUTPUT)
//!                                   ├─ queue leaky ! appsink        (program tap)
//!                                   └─ queue leaky ! appsink        (recording, with --record)
//! audio bin: unixfdsrc ! appsink ─► [+D] ─► appsrc ! audioconvert ! appsink | decklinkaudiosink
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
//!   v4 with the scene snapshot and each frame's time base) and their supervisors; their sockets are bins
//!   of this pipeline ([`Host::Bin`]), added for each worker session and
//!   removed with it, so a restarted worker is relinked into the running
//!   pipeline. A dead or stalled worker only costs CG: its frames' slots get
//!   clean feed.
//! - **Capture** is a bin too, rebuilt when the capture process goes away.
//! - **Rates and formats** come from the capture's caps and may change at any
//!   time: deadlines do not depend on the rate, the program follows the input's
//!   rate and scan ([`OutputRate::Follow`]) or converts it to a fixed one
//!   ([`OutputRate::Fixed`]), and a new format starts a new tracking stream
//!   (`server/docs/gst-core.md`). Interlaced frames are deinterlaced for
//!   tracking and rendering only ([`capture`]).
//! - **Input loss** fills the program's slots on its cadence ([`InputLoss`]).
//! - **Audio, timecode and captions** leave with their frame's delay D
//!   ([`audio`], [`ancillary`]).
//! - **Recording** (`--record`): leaky branches of the capture's and the
//!   program's `tee` hand their frames to the recording worker's taps
//!   ([`crate::worker::recording`]); they are only built when recording.
//!
//! `fallbackswitch` is not used: it switches streams after a timeout, so a
//! single late CG frame costs output slots and never shows that slot's clean
//! frame (`server/docs/gst-core.md`).

pub mod ancillary;
pub mod audio;
pub mod capture;
pub mod decklink;
pub mod driver;
pub mod input;
pub mod output;
pub mod select;

pub use select::InputLoss;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use chr_common::rate::Rate;

use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSrc};

use crate::{
    sdi::{Format, Scan, SdiRecorder, VideoMode},
    worker::{
        recording::Tap,
        transport::{self, Host},
    },
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// Name of the custom meta on every program frame: `cg` (bool), the
/// frame's capture `sequence` and its capture time `source-ns` (u64).
pub const PROGRAM_META: &str = "chr-program-frame";
/// The selector decides a slot this long before it is due, leaving the time to
/// hand the frame through `livesync` to the output.
pub const DECISION_MARGIN: Duration = Duration::from_millis(4);
/// How long before it goes on air a card gets its frame. `livesync` hands a
/// slot over at its time, but `decklinkvideosink` schedules each frame about
/// 10 ms before air and calls a frame handed over later late (the card may drop
/// it). With a card, program frames therefore carry `due - CARD_LEAD` and the
/// sink's `ts-offset` puts them back on air at `due`: capture + D as before,
/// with D − `CARD_LEAD` left for the CG.
pub const CARD_LEAD: Duration = Duration::from_millis(20);

/// Registers the metas the core reads and writes; idempotent.
pub fn init() -> Result<(), Error> {
    static INIT: OnceLock<()> = OnceLock::new();
    transport::init()?;
    ancillary::init();
    INIT.get_or_init(|| gst::meta::CustomMeta::register(PROGRAM_META, &[]));
    Ok(())
}

/// The pipeline clock seen as running time, the time base of every timestamp
/// in the core.
#[derive(Clone, Debug)]
pub struct Timeline {
    clock: gst::Clock,
    base: gst::ClockTime,
    /// The pipeline clock is the same-host system clock the capture process
    /// stamps with (simulation), not a card's.
    system: bool,
}

impl Timeline {
    pub fn of(pipeline: &gst::Pipeline) -> Result<Self, Error> {
        let clock = pipeline.clock().ok_or("the pipeline has no clock")?;
        Ok(Self {
            system: clock == gst::SystemClock::obtain(),
            clock,
            base: pipeline
                .base_time()
                .ok_or("the pipeline has no base time")?,
        })
    }

    pub fn clock(&self) -> &gst::Clock {
        &self.clock
    }

    /// The current running time.
    pub fn now(&self) -> gst::ClockTime {
        self.clock.time().saturating_sub(self.base)
    }

    /// The running time of `system_ns`, a time on the same-host system clock:
    /// a capture time the capture process stamped (`source_ns`). On a card's
    /// clock it is mapped through the two clocks' offset now; they drift apart
    /// by parts per million, far below a frame.
    pub fn of_system(&self, system_ns: u64) -> gst::ClockTime {
        let offset = if self.system {
            0
        } else {
            self.clock.time().nseconds() as i128
                - gst::SystemClock::obtain().time().nseconds() as i128
        };
        let running = system_ns as i128 + offset - self.base.nseconds() as i128;
        gst::ClockTime::from_nseconds(running.max(0) as u64)
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
    /// The input's rate and scan, changing with it.
    Follow,
    /// This rate and scan whatever the input's: frames are repeated or
    /// dropped; a progressive output deinterlaces an interlaced input.
    Fixed(Rate, Scan),
}

impl std::str::FromStr for OutputRate {
    type Err = String;

    /// `follow`, `fixed:<rate>` or `fixed:<field rate>i` (`fixed:50i`: 25
    /// interlaced frames per second).
    fn from_str(text: &str) -> Result<Self, String> {
        match text.split_once(':') {
            None if text == "follow" => Ok(Self::Follow),
            Some(("fixed", rate)) => {
                let (rate, scan) = crate::sdi::scanned_rate(rate)?;
                Ok(Self::Fixed(rate, scan))
            }
            _ => Err(format!(
                "the output rate is follow or fixed:<rate> (fixed:50, fixed:59.94, fixed:50i), \
                 not {text:?}"
            )),
        }
    }
}

/// Where the program goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputSink {
    /// An `appsink` on the system clock that measures the cadence.
    #[default]
    Simulated,
    /// A DeckLink card's output, whose clock becomes the pipeline's. It
    /// needs a fixed output rate: a card does not change its mode while it
    /// plays. chr-server opens it in the input's format ([`capture::probe`])
    /// unless told another.
    DeckLink { device: u32 },
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
    pub sink: OutputSink,
    /// What a lost input's slots show.
    pub input_loss: InputLoss,
    /// Pass the input's audio through.
    pub audio: bool,
    /// Audio channels of a DeckLink output (2, 8 or 16), whatever the input
    /// sends.
    pub audio_channels: u32,
    /// Where every captured frame is recorded, after numbering.
    pub record_input: Option<Arc<Tap>>,
    /// Where every program slot is recorded, before the output.
    pub record_program: Option<Arc<Tap>>,
}

/// The running pipeline: program output, selector and capture.
pub struct Core {
    pipeline: gst::Pipeline,
    timeline: Timeline,
    selector: select::Selector,
    output: Arc<output::Output>,
    capture: Mutex<CaptureState>,
    audio: Option<audio::Audio>,
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
        let output_format = Format {
            mode: config.output,
            scan: match config.output_rate {
                OutputRate::Follow => Scan::Progressive,
                OutputRate::Fixed(_, scan) => scan,
            },
        };
        if matches!(config.sink, OutputSink::DeckLink { .. })
            && config.output_rate == OutputRate::Follow
        {
            return Err("a DeckLink output opens in one format: probe the input's \
                        (capture::probe) and give it as OutputRate::Fixed; \
                        decklinkvideosink does not change its mode while it plays"
                .into());
        }
        let card = matches!(config.sink, OutputSink::DeckLink { .. });
        let lead = if card { CARD_LEAD } else { Duration::ZERO };
        if card && config.program_delay <= lead + DECISION_MARGIN {
            return Err(format!(
                "a DeckLink output needs a program delay above {:?}: the card gets each \
                 frame {lead:?} before air",
                lead + DECISION_MARGIN
            )
            .into());
        }
        let program = AppSrc::builder()
            .name("program")
            .caps(&uyvy_caps(output_format))
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
        let output_sink: gst::Element = match config.sink {
            OutputSink::Simulated => {
                let sink = AppSink::builder()
                    .name("output")
                    .sync(true)
                    .async_(false)
                    .enable_last_sample(false)
                    .max_buffers(2)
                    .drop(true)
                    .build();
                // livesync hands over each slot when it is due: no processing
                // time to add to the pipeline latency (basesink's default is 20 ms).
                sink.set_property("processing-deadline", 0_u64);
                sink.upcast()
            }
            OutputSink::DeckLink { device } => {
                let sink = decklink::video_sink(device, output_format)?;
                sink.set_property("ts-offset", CARD_LEAD.as_nanos() as i64);
                // The card's clock, locked to its reference input, times the program.
                let clock = sink
                    .provide_clock()
                    .ok_or("decklinkvideosink provides no clock")?;
                pipeline.use_clock(Some(&clock));
                log::info!(
                    "output: DeckLink device {device} in {}; the card's clock is the pipeline's",
                    decklink::Mode::of(output_format)?.nick
                );
                sink
            }
        };
        if config.sink == OutputSink::Simulated {
            pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        }
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
            &output_sink,
            &tap_queue,
            tap_sink.upcast_ref(),
        ])?;
        gst::Element::link_many([program.upcast_ref(), &livesync, &tee])?;
        gst::Element::link_many([&tee, &output_queue, &output_sink])?;
        gst::Element::link_many([&tee, &tap_queue, tap_sink.upcast_ref()])?;
        if let Some(record) = &config.record_program {
            record.branch(pipeline.upcast_ref(), &tee)?;
        }
        let tap = match &config.tap {
            Some(path) => Some(chr_shm::Writer::create(
                path,
                chr_shm::tap::layout(config.output.width, config.output.height),
            )?),
            None => None,
        };
        let loss_fill = fill::frame(&config.input_loss, config.output)?;
        let audio = if config.audio {
            Some(audio::Chain::add(
                &pipeline,
                config.program_delay,
                match config.sink {
                    OutputSink::Simulated => audio::Sink::Simulated,
                    OutputSink::DeckLink { device } => {
                        audio::Sink::Element(decklink::audio_sink(device)?)
                    }
                },
                card.then_some(config.audio_channels),
            )?)
        } else {
            None
        };
        pipeline.set_state(gst::State::Playing)?;
        let timeline = Timeline::of(&pipeline)?;
        let output = Arc::new(output::Output::new(
            config.output,
            timeline.clone(),
            tap,
            config.recorder.clone(),
            lead,
        ));
        output::attach(&output, &output_sink, &tap_sink)?;
        if card {
            // A card's clock stands still until it plays, and it plays once
            // both sinks have their caps: give them a frame (and silence) at
            // running time 0, due now, before anything waits on the clock.
            select::prime(
                &program,
                &fill::black(config.output),
                output_format,
                &timeline,
            )?;
        }
        let selector = select::Selector::start(
            select::Program {
                appsrc: program,
                output: output_format,
                policy: config.output_rate,
                recorder: config.recorder.clone(),
                loss_fill,
            },
            timeline.clone(),
            select::Timing::new(config.program_delay - lead),
        )?;
        let audio = audio.map(|chain| {
            chain.start(
                &pipeline,
                timeline.clone(),
                input::audio_socket(&config.capture_socket),
                config.recorder.clone(),
            )
        });
        let streams = Arc::new(capture::Streams::new(on_format(
            selector.clone(),
            config.recorder.clone(),
            card.then_some(output_format),
        )));
        let (frames, captured) = tokio::sync::mpsc::channel(CAPTURED_QUEUE);
        log::info!(
            "GStreamer core: capture from {} (program camera {}), program {}x{} at {} delayed \
             {:?}, each slot decided {:?} before it is due; input loss: {:?}; audio {}",
            config.capture_socket.display(),
            config.capture.name(),
            config.output.width,
            config.output.height,
            match config.output_rate {
                OutputRate::Follow => "the input's rate and scan".to_owned(),
                OutputRate::Fixed(..) => format!("a fixed {}", output_format.name()),
            },
            config.program_delay,
            DECISION_MARGIN,
            config.input_loss,
            if config.audio {
                "passed through"
            } else {
                "off"
            },
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
                audio,
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
        if let Some(audio) = &self.audio {
            audio.maintain();
        }
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
            self.timeline.clone(),
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
            let recoverable = source.contains(":hop-")
                || source.contains(":capture-")
                || source.contains(":audio-");
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

    /// The output, the audio and the selector since the last report.
    pub fn report(&self) -> Report {
        let (output, window) = self.output.report();
        let slack = self.selector.take_slack();
        let video_delay = (window.delay.count > 0).then(|| window.delay.mean());
        let audio = self.audio.as_ref().map_or_else(
            || "audio: off".to_owned(),
            |audio| audio.report(video_delay),
        );
        let recording = [
            ("input", &self.config.record_input),
            ("program", &self.config.record_program),
        ]
        .into_iter()
        .filter_map(|(name, tap)| {
            Some(format!(
                "; recording {name}: {}",
                tap.as_ref()?.counters().report()
            ))
        })
        .collect::<String>();
        Report {
            text: format!(
                "{output}; per-slot slack of CG slots {}; {audio}; {}{recording}",
                slack.ms(),
                self.selector.counters().report()
            ),
            window,
            slack,
        }
    }

    /// The program sequence of the newest input stream's first frame
    /// (`frame_id` 0).
    pub fn stream_base(&self) -> u64 {
        self.streams.base()
    }

    pub fn selector_counters(&self) -> select::Counters {
        self.selector.counters()
    }

    pub fn output_totals(&self) -> output::Totals {
        self.output.totals()
    }
}

/// One report of the core: its text for the log, and the window it covers.
pub struct Report {
    pub text: String,
    /// The output's slots since the previous report.
    pub window: output::Totals,
    /// How long before its slot's deadline each CG slot's frame arrived, ns.
    pub slack: output::Summary,
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current = None;
        self.audio = None;
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
/// selector, counted in the status. The status's mode changes where the
/// output's does, in the selector. A card keeps the mode it was opened in
/// (`card`): an input in another format is converted, which is worth a warning.
fn on_format(
    selector: select::Selector,
    recorder: crate::sdi::SdiRecorder,
    card: Option<Format>,
) -> capture::OnFormat {
    Box::new(move |first, format: Format, changed| {
        if changed {
            log::info!("capture format now {} from frame {first}", format.name());
            selector.begin_transition(first);
            recorder.format_changed();
        } else {
            log::info!("capture format {} from frame {first}", format.name());
        }
        if let Some(card) = card
            && card != format
        {
            log::warn!(
                "the input is {} but the DeckLink output stays {}: its frames are converted \
                 (repeated or dropped); restart chr-server to open the card in the input's format",
                format.name(),
                card.name()
            );
        }
    })
}

/// Packed 8-bit UYVY, BT.709 limited range, in `format`.
pub fn uyvy_caps(format: Format) -> gst::Caps {
    let (interlace, order) = format.scan.caps_fields();
    let mut caps = gst::Caps::builder("video/x-raw")
        .field("format", "UYVY")
        .field("width", format.mode.width as i32)
        .field("height", format.mode.height as i32)
        .field(
            "framerate",
            crate::worker::tracking::wire::fraction(format.mode.fps),
        )
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .field("interlace-mode", interlace)
        .field("colorimetry", "bt709")
        // SDI's 4:2:2 siting. `decklinkvideosink` names it in every mode, and its
        // accept-caps needs the program's caps to be a subset of a mode's.
        .field("chroma-site", "mpeg2")
        .build();
    if let Some(order) = order {
        caps.make_mut()
            .structure_mut(0)
            .expect("one structure")
            .set("field-order", order);
    }
    caps
}

/// The still frames a lost input's slots show.
mod fill {
    use super::*;

    /// A black UYVY frame at the output's extent.
    pub fn black(output: VideoMode) -> gst::Buffer {
        let pixels = output.width as usize * output.height as usize;
        gst::Buffer::from_mut_slice([0x80, 0x10, 0x80, 0x10].repeat(pixels / 2))
    }

    /// Black or the slate at the output's extent; `None` to hold the last frame.
    pub fn frame(loss: &InputLoss, output: VideoMode) -> Result<Option<gst::Buffer>, Error> {
        match loss {
            InputLoss::Hold => Ok(None),
            InputLoss::Black => Ok(Some(black(output))),
            InputLoss::Slate(None) => {
                convert("videotestsrc num-buffers=1 pattern=smpte", None, output).map(Some)
            }
            InputLoss::Slate(Some(png)) => {
                let image = image::open(png)
                    .map_err(|error| format!("cannot read the slate {}: {error}", png.display()))?
                    .to_rgba8();
                let caps = gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .field("width", image.width() as i32)
                    .field("height", image.height() as i32)
                    .field("framerate", gst::Fraction::new(0, 1))
                    .build();
                convert(
                    "appsrc name=in format=time",
                    Some((caps, gst::Buffer::from_mut_slice(image.into_raw()))),
                    output,
                )
                .map(Some)
            }
        }
    }

    /// One frame of `front` (fed `input`, if any) as UYVY at the output's extent.
    fn convert(
        front: &str,
        input: Option<(gst::Caps, gst::Buffer)>,
        output: VideoMode,
    ) -> Result<gst::Buffer, Error> {
        let caps = uyvy_caps(output.into());
        let structure = caps.structure(0).expect("one structure");
        let pipeline = gst::parse::launch(&format!(
            "{front} ! videoconvertscale add-borders=true ! video/x-raw,format=UYVY,width={},\
             height={},pixel-aspect-ratio=1/1,colorimetry=bt709 ! appsink name=out sync=false",
            structure.get::<i32>("width")?,
            structure.get::<i32>("height")?,
        ))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "not a pipeline")?;
        let sink = pipeline
            .by_name("out")
            .and_then(|sink| sink.downcast::<AppSink>().ok())
            .ok_or("no appsink")?;
        pipeline.set_state(gst::State::Playing)?;
        if let Some((caps, buffer)) = input {
            let source = pipeline
                .by_name("in")
                .and_then(|source| source.downcast::<AppSrc>().ok())
                .ok_or("no appsrc")?;
            source.set_caps(Some(&caps));
            source.push_buffer(buffer)?;
            source.end_of_stream()?;
        }
        let sample = sink.try_pull_sample(gst::ClockTime::from_seconds(5));
        let _ = pipeline.set_state(gst::State::Null);
        Ok(sample
            .and_then(|sample| sample.buffer_owned())
            .ok_or("the slate was not converted")?)
    }
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
    use crate::sdi::SdiStats;

    #[test]
    fn input_loss_policies_parse_and_fill_black_or_a_slate() {
        assert_eq!("hold".parse(), Ok(InputLoss::Hold));
        assert_eq!("black".parse(), Ok(InputLoss::Black));
        assert_eq!("slate".parse(), Ok(InputLoss::Slate(None)));
        assert_eq!(
            "slate:/x/s.png".parse(),
            Ok(InputLoss::Slate(Some("/x/s.png".into())))
        );
        for bad in ["", "freeze", "slate:", "black:x"] {
            assert!(bad.parse::<InputLoss>().is_err(), "{bad}");
        }
        init().unwrap();
        let output = VideoMode {
            width: 64,
            height: 36,
            fps: Rate::fps(50),
        };
        assert!(fill::frame(&InputLoss::Hold, output).unwrap().is_none());
        let black = fill::frame(&InputLoss::Black, output).unwrap().unwrap();
        assert_eq!(black.size(), 64 * 36 * 2);
        assert_eq!(
            &black.map_readable().unwrap()[..4],
            [0x80, 0x10, 0x80, 0x10]
        );
        let bars = fill::frame(&InputLoss::Slate(None), output)
            .unwrap()
            .unwrap();
        assert_eq!(bars.size(), 64 * 36 * 2);
        // A PNG slate is scaled to the output.
        let dir = std::env::temp_dir().join(format!("chr-slate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("slate.png");
        image::RgbaImage::from_pixel(32, 18, image::Rgba([255, 255, 255, 255]))
            .save(&png)
            .unwrap();
        let slate = fill::frame(&InputLoss::Slate(Some(png)), output)
            .unwrap()
            .unwrap();
        let pixels = slate.map_readable().unwrap();
        assert_eq!(pixels.len(), 64 * 36 * 2);
        assert!(pixels[1] > 200, "white luma: {}", pixels[1]);
        drop(pixels);
        assert!(fill::frame(&InputLoss::Slate(Some(dir.join("none.png"))), output).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn interlaced_program_caps_carry_the_field_order() {
        init().unwrap();
        let caps = uyvy_caps(Format {
            mode: VideoMode {
                width: 1920,
                height: 1080,
                fps: Rate::fps(25),
            },
            scan: crate::sdi::Scan::Interlaced(crate::sdi::FieldOrder::TopFirst),
        });
        let format = capture::format_of(&caps).unwrap();
        assert_eq!(format.name(), "1920x1080i50");
        assert_eq!(
            decklink::Mode::of(format).unwrap().nick,
            "1080i50",
            "the DeckLink output's mode"
        );
    }

    /// A CG frame of an interlaced program is one progressive picture sent
    /// with interlaced caps: exactly what `interlace field-pattern=2:2` makes
    /// of it (both fields from the one picture), so the core needs no element.
    #[test]
    fn a_progressive_picture_woven_two_two_is_the_same_frame() {
        init().unwrap();
        let (width, height) = (64_usize, 36_usize);
        let pipeline = gst::parse::launch(
            "appsrc name=in format=time ! interlace field-pattern=2:2 top-field-first=true \
             ! appsink name=out sync=false",
        )
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let source = pipeline
            .by_name("in")
            .unwrap()
            .downcast::<AppSrc>()
            .unwrap();
        let sink = pipeline
            .by_name("out")
            .unwrap()
            .downcast::<AppSink>()
            .unwrap();
        source.set_caps(Some(&uyvy_caps(Format::from(VideoMode {
            width: width as u32,
            height: height as u32,
            fps: Rate::fps(25),
        }))));
        pipeline.set_state(gst::State::Playing).unwrap();
        let frames: Vec<Vec<u8>> = (0..3_u8)
            .map(|n| {
                (0..width * height * 2)
                    .map(|i| (i as u8).wrapping_mul(7) ^ n)
                    .collect()
            })
            .collect();
        for (n, frame) in frames.iter().enumerate() {
            let mut buffer = gst::Buffer::from_slice(frame.clone());
            let writable = buffer.get_mut().unwrap();
            writable.set_pts(gst::ClockTime::from_mseconds(40 * n as u64));
            writable.set_duration(gst::ClockTime::from_mseconds(40));
            source.push_buffer(buffer).unwrap();
        }
        source.end_of_stream().unwrap();
        let mut woven = Vec::new();
        while let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_seconds(2)) {
            let caps = sample.caps().unwrap().structure(0).unwrap().to_owned();
            assert_eq!(caps.get::<&str>("interlace-mode").unwrap(), "interleaved");
            woven.push(sample.buffer().unwrap().map_readable().unwrap().to_vec());
        }
        pipeline.set_state(gst::State::Null).unwrap();
        assert_eq!(woven, frames);
    }

    /// A card's output is primed: a black program frame at running time 0,
    /// with the program's caps, leaves `livesync` at once (a card's sink
    /// starts playing, and its clock running, on those caps), and its audio
    /// sink gets the card's fixed format with silence at 0, whether or not the
    /// input has audio.
    #[test]
    fn a_card_output_is_primed_with_black_and_silence_at_running_time_zero() {
        init().unwrap();
        let i50 = Format {
            mode: VideoMode {
                width: 64,
                height: 36,
                fps: Rate::fps(25),
            },
            scan: crate::sdi::Scan::Interlaced(crate::sdi::FieldOrder::TopFirst),
        };
        let pipeline = gst::parse::launch(
            "appsrc name=program format=time is-live=true ! livesync ! appsink name=output",
        )
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        let program = pipeline
            .by_name("program")
            .unwrap()
            .downcast::<AppSrc>()
            .unwrap();
        program.set_caps(Some(&uyvy_caps(i50)));
        let sink = pipeline
            .by_name("output")
            .unwrap()
            .downcast::<AppSink>()
            .unwrap();
        let chain = audio::Chain::add(
            &pipeline,
            Duration::from_millis(100),
            audio::Sink::Simulated,
            Some(8),
        )
        .unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let timeline = Timeline::of(&pipeline).unwrap();
        let audio_sink = pipeline
            .by_name("audio-output")
            .unwrap()
            .downcast::<AppSink>()
            .unwrap();
        // The audio sink's frames, as the card would take them.
        let heard = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&heard);
        audio_sink.static_pad("sink").unwrap().add_probe(
            gst::PadProbeType::BUFFER,
            move |pad, info| {
                let caps = pad.current_caps().unwrap();
                let channels = caps.structure(0).unwrap().get::<i32>("channels").unwrap();
                let buffer = info.buffer().unwrap();
                record
                    .lock()
                    .unwrap()
                    .push((buffer.pts(), channels, buffer.size()));
                gst::PadProbeReturn::Ok
            },
        );
        select::prime(&program, &fill::black(i50.mode), i50, &timeline).unwrap();
        let audio = chain.start(&pipeline, timeline, "/nonexistent".into(), {
            SdiStats::recorded(i50).1
        });
        let sample = sink
            .try_pull_sample(gst::ClockTime::from_mseconds(500))
            .expect("the primed frame");
        let buffer = sample.buffer().unwrap();
        assert_eq!(buffer.pts(), Some(gst::ClockTime::ZERO));
        assert_eq!(
            capture::format_of(sample.caps().unwrap()).unwrap(),
            i50,
            "the program's caps reach the sink with it"
        );
        let meta = gst::meta::CustomMeta::from_buffer(buffer, PROGRAM_META).unwrap();
        assert!(meta.structure().get::<bool>("fill").unwrap());
        assert_eq!(
            &buffer.map_readable().unwrap()[..4],
            [0x80, 0x10, 0x80, 0x10]
        );
        let deadline = Instant::now() + Duration::from_millis(500);
        while heard.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            heard.lock().unwrap().first().copied(),
            Some((Some(gst::ClockTime::ZERO), 8, 480 * 8 * 4)),
            "10 ms of 8-channel silence at 0"
        );
        drop(audio);
        pipeline.set_state(gst::State::Null).unwrap();
    }

    #[test]
    fn a_system_clock_capture_time_maps_onto_another_clock() {
        init().unwrap();
        let pipeline = gst::Pipeline::new();
        let realtime = gst::glib::Object::builder::<gst::SystemClock>()
            .property("clock-type", gst::ClockType::Realtime)
            .build();
        pipeline.use_clock(Some(&realtime));
        pipeline.set_state(gst::State::Playing).unwrap();
        let timeline = Timeline::of(&pipeline).unwrap();
        assert!(!timeline.system);
        // A frame the capture process stamped now was captured at the running
        // time now, on any clock.
        let system_now = gst::SystemClock::obtain().time().nseconds();
        let captured = timeline.of_system(system_now).nseconds() as i64;
        let now = timeline.now().nseconds() as i64;
        assert!((captured - now).abs() < 5_000_000, "{captured} vs {now}");
        pipeline.set_state(gst::State::Null).unwrap();
    }

    #[test]
    fn output_rates_parse() {
        assert_eq!("follow".parse(), Ok(OutputRate::Follow));
        assert_eq!(
            "fixed:59.94".parse(),
            Ok(OutputRate::Fixed(
                Rate::new(60000, 1001).unwrap(),
                Scan::Progressive
            ))
        );
        assert_eq!(
            "fixed:50".parse(),
            Ok(OutputRate::Fixed(Rate::fps(50), Scan::Progressive))
        );
        assert_eq!(
            "fixed:50i".parse(),
            Ok(OutputRate::Fixed(
                Rate::fps(25),
                Scan::Interlaced(crate::sdi::FieldOrder::TopFirst)
            ))
        );
        for bad in ["fixed", "fixed:", "fixed:0", "follow:50", "50", ""] {
            assert!(bad.parse::<OutputRate>().is_err(), "{bad}");
        }
    }
}
