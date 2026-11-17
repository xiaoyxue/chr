//! The program's audio: the input's embedded audio, delayed by exactly the
//! program delay D, like its video (#154 phase 4).
//!
//! ```text
//! INPUT ─unixfd (<capture socket>.audio)─► audio-<n>: unixfdsrc ! appsink   (per session)
//!            ─► [capture time + D] ─► program audio: appsrc ! audioconvert ! audio sink
//!                                     (appsink sync=true | decklinkaudiosink)
//! ```
//!
//! - **Same delay, same clock.** Every audio buffer's capture time, a time on
//!   the same-host system clock like a frame's (the input stamps it in chr's
//!   capture meta), becomes running time of the core's clock exactly as a
//!   frame's does ([`Timeline::of_system`]); it is due
//!   D later, and the sink renders it on the pipeline clock when due. A frame
//!   and the audio captured with it therefore leave together: lip sync is the
//!   input's, within a sample.
//! - **Independent of the video.** Audio never passes the selector, the
//!   workers or the program's `livesync`: a worker that dies or stalls, a format
//!   change or a slow render cannot drop, delay or block it. The program
//!   `appsrc` holds D of audio (its queue is D plus a second, leaky).
//! - **Its own socket.** The capture hop's audio is a second unixfd socket
//!   beside the frames' (`<capture socket>.audio`): audio buffers are small,
//!   so `unixfdsink` copies them into shared memory; the receiver maps them
//!   read-only, as frames. A session is connected, lost and reconnected on its
//!   own, as the capture is.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use chr_common::methods::sdi::AudioStatus;
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSinkCallbacks, AppSrc};

use super::{Error, Timeline, ancillary::Ancillary, output::Summary};
use crate::sdi::SdiRecorder;

/// How long a lost audio session waits before it connects again.
const RETRY: Duration = Duration::from_millis(500);

/// The audio sink: simulated (an `appsink` on the clock) or a card's.
pub enum Sink {
    Simulated,
    Element(gst::Element),
}

/// Delivery statistics: capture-to-output delay and samples.
#[derive(Default)]
struct Stats {
    window: Summary,
    samples: u64,
    rate: u32,
    channels: u32,
}

/// The program's audio chain, added before the pipeline plays: elements
/// added to a playing pipeline would not get its clock and base time.
pub struct Chain {
    program: AppSrc,
    delay: gst::ClockTime,
    stats: Arc<Mutex<Stats>>,
    timeline: Arc<OnceLock<Timeline>>,
}

pub struct Audio {
    pipeline: gst::Pipeline,
    program: AppSrc,
    socket: PathBuf,
    timeline: Timeline,
    delay: gst::ClockTime,
    stats: Arc<Mutex<Stats>>,
    session: Mutex<Session>,
    recorder: SdiRecorder,
}

struct Session {
    bin: Option<(gst::Bin, Arc<AtomicBool>)>,
    retry_at: Instant,
}

impl Chain {
    /// The program's audio chain in `pipeline`, which does not play yet.
    pub fn add(pipeline: &gst::Pipeline, delay: Duration, sink: Sink) -> Result<Self, Error> {
        let delay = gst::ClockTime::from_nseconds(delay.as_nanos() as u64);
        let program = AppSrc::builder()
            .name("program-audio")
            .format(gst::Format::Time)
            .is_live(true)
            .min_latency(0)
            .block(false)
            .max_buffers(0)
            .max_bytes(0)
            .max_time(delay + gst::ClockTime::SECOND)
            .build();
        program.set_property_from_str("leaky-type", "downstream");
        let convert = gst::ElementFactory::make("audioconvert").build()?;
        let stats = Arc::new(Mutex::new(Stats::default()));
        let timeline = Arc::new(OnceLock::<Timeline>::new());
        let sink = match sink {
            Sink::Element(element) => element,
            Sink::Simulated => {
                let sink = AppSink::builder()
                    .name("audio-output")
                    .sync(true)
                    .async_(false)
                    .enable_last_sample(false)
                    .max_buffers(4)
                    .drop(true)
                    .build();
                sink.set_property("processing-deadline", 0_u64);
                let (stats, timeline) = (Arc::clone(&stats), Arc::clone(&timeline));
                sink.set_callbacks(
                    AppSinkCallbacks::builder()
                        .new_sample(move |sink| {
                            let sample =
                                sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                            if let Some(timeline) = timeline.get() {
                                rendered(&stats, timeline, delay, &sample);
                            }
                            Ok(gst::FlowSuccess::Ok)
                        })
                        .build(),
                );
                sink.upcast()
            }
        };
        pipeline.add_many([program.upcast_ref(), &convert, &sink])?;
        gst::Element::link_many([program.upcast_ref(), &convert, &sink])?;
        Ok(Self {
            program,
            delay,
            stats,
            timeline,
        })
    }

    /// The running chain, taking audio from `socket` once it is there.
    pub fn start(
        self,
        pipeline: &gst::Pipeline,
        timeline: Timeline,
        socket: PathBuf,
        recorder: SdiRecorder,
    ) -> Audio {
        let _ = self.timeline.set(timeline.clone());
        Audio {
            pipeline: pipeline.clone(),
            program: self.program,
            socket,
            timeline,
            delay: self.delay,
            stats: self.stats,
            session: Mutex::new(Session {
                bin: None,
                retry_at: Instant::now(),
            }),
            recorder,
        }
    }
}

impl Audio {
    /// Keeps an audio session connected while the input offers one.
    pub fn maintain(&self) {
        let mut session = lock(&self.session);
        if let Some((_, failed)) = &session.bin {
            if !failed.load(Ordering::Acquire) {
                return;
            }
            log::warn!("audio from {} lost; reconnecting", self.socket.display());
            if let Some((bin, _)) = session.bin.take() {
                let _ = bin.set_state(gst::State::Null);
                let _ = self.pipeline.remove(&bin);
            }
            session.retry_at = Instant::now() + RETRY;
        }
        if Instant::now() < session.retry_at || !self.socket.exists() {
            return;
        }
        session.retry_at = Instant::now() + RETRY;
        match self.open() {
            Ok(opened) => {
                log::info!("audio session connected to {}", self.socket.display());
                session.bin = Some(opened);
            }
            Err(error) => log::debug!("audio not connected: {error}"),
        }
    }

    /// One session: `unixfdsrc ! appsink`, its buffers retimed to their slot
    /// and handed to the program's audio.
    fn open(&self) -> Result<(gst::Bin, Arc<AtomicBool>), Error> {
        static SESSIONS: AtomicU64 = AtomicU64::new(0);
        let bin = gst::Bin::with_name(&format!(
            "audio-{}",
            SESSIONS.fetch_add(1, Ordering::Relaxed)
        ));
        let source = gst::ElementFactory::make("unixfdsrc")
            .property(
                "socket-path",
                self.socket
                    .to_str()
                    .ok_or("the audio socket path is not UTF-8")?,
            )
            .build()?;
        let received = AppSink::builder()
            .sync(false)
            .async_(false)
            .enable_last_sample(false)
            .max_buffers(16)
            .drop(true)
            .caps(&gst::Caps::new_empty_simple("audio/x-raw"))
            .build();
        bin.add_many([&source, received.upcast_ref()])?;
        source.link(&received)?;
        let failed = Arc::new(AtomicBool::new(false));
        let ended = Arc::clone(&failed);
        let (program, timeline, delay) = (self.program.clone(), self.timeline.clone(), self.delay);
        received.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                    if let Err(error) = forward(&program, &timeline, delay, &sample) {
                        log::debug!("audio buffer dropped: {error}");
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .eos(move |_| ended.store(true, Ordering::Release))
                .build(),
        );
        self.pipeline.add(&bin)?;
        if let Err(error) = bin.sync_state_with_parent() {
            let _ = bin.set_state(gst::State::Null);
            let _ = self.pipeline.remove(&bin);
            return Err(error.into());
        }
        Ok((bin, failed))
    }

    /// The audio since the last report, given the video's mean
    /// capture-to-output delay over the same time: the lip-sync offset.
    pub fn report(&self, video_delay_ns: Option<f64>) -> String {
        let (window, samples, rate, channels) = {
            let mut stats = lock(&self.stats);
            (
                std::mem::take(&mut stats.window),
                stats.samples,
                stats.rate,
                stats.channels,
            )
        };
        if window.count == 0 {
            return "audio: none".to_owned();
        }
        let offset = video_delay_ns.map(|video| window.mean() - video);
        self.recorder.audio(AudioStatus {
            sample_rate: rate,
            channels,
            samples_output: samples,
            delay_us: (window.mean() / 1e3) as i64,
            offset_us: offset.map_or(0, |offset| (offset / 1e3) as i64),
        });
        format!(
            "audio: {} buffers ({samples} samples at {rate} Hz, {channels} channels), delay {}, \
             audio - video {}",
            window.count,
            window.ms(),
            offset.map_or_else(
                || "-".to_owned(),
                |offset| format!("{:.3} ms", offset / 1e6)
            )
        )
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        if let Some((bin, _)) = lock(&self.session).bin.take() {
            let _ = bin.set_state(gst::State::Null);
            let _ = self.pipeline.remove(&bin);
        }
    }
}

/// Retimes a received audio buffer to its capture time plus the program
/// delay and hands it to the program's audio.
fn forward(
    program: &AppSrc,
    timeline: &Timeline,
    delay: gst::ClockTime,
    sample: &gst::Sample,
) -> Result<(), Error> {
    let caps = sample.caps_owned().ok_or("audio without caps")?;
    if program.caps().as_ref() != Some(&caps) {
        program.set_caps(Some(&caps));
    }
    let mut buffer = sample.buffer_owned().ok_or("a sample without a buffer")?;
    let captured = Ancillary::read(&buffer)?
        .and_then(|ancillary| ancillary.capture_ns)
        .ok_or("audio without its capture time")?;
    let writable = buffer.make_mut();
    writable.set_pts(timeline.of_system(captured) + delay);
    // A sink syncs on the DTS if there is one (a parser's, from the input).
    writable.set_dts(None);
    program.push_buffer(buffer)?;
    Ok(())
}

/// Records one audio buffer the simulated sink rendered.
fn rendered(
    stats: &Mutex<Stats>,
    timeline: &Timeline,
    delay: gst::ClockTime,
    sample: &gst::Sample,
) {
    let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
        return;
    };
    let Some(pts) = buffer.pts() else {
        return;
    };
    let structure = caps.structure(0);
    let field = |name: &str| {
        structure
            .and_then(|structure| structure.get::<i32>(name).ok())
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_default()
    };
    let (rate, channels) = (field("rate"), field("channels"));
    let at = timeline.clock().time();
    let captured = timeline.absolute(pts.saturating_sub(delay));
    let mut stats = lock(stats);
    stats
        .window
        .add(at.nseconds() as i64 - captured.nseconds() as i64);
    let frame_bytes = (channels * 4).max(1) as usize;
    stats.samples += (buffer.size() / frame_bytes) as u64;
    (stats.rate, stats.channels) = (rate, channels);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
