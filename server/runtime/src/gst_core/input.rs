//! The INPUT process's pipeline (`chr-sdi-in`): the input of the GStreamer
//! core (#154). It plays a source, stamps every frame with its identity and
//! hands it to chr-server over a unixfd socket, and its audio over a second one:
//!
//! ```text
//! file:     ffmpeg (decode, loop, UYVY) | fdsrc ! rawvideoparse ! videoconvert ─┐
//! pattern:  videotestsrc ──────────────────────────────────────────────────────┤
//! DeckLink: decklinkvideosrc mode=auto|<mode> ! videoconvert ──────────────────┤
//!   ! video/x-raw,format=RGBA,<format> ! clocksync ! [stamp]
//!   ! queue leaky=downstream ! unixfdsink <socket>
//! file:     ffmpeg (decode, loop, S32LE) | fdsrc ! rawaudioparse ─┐
//! pattern:  audiotestsrc wave=ticks ──────────────────────────────┤
//! DeckLink: decklinkaudiosrc ! audioconvert ──────────────────────┤
//!   ! audio/x-raw,S32LE,48 kHz ! clocksync ! [gate]
//!   ! queue leaky=downstream ! unixfdsink <socket>.audio
//! ```
//!
//! - **Pace.** A file and the test pattern play in real time on the system
//!   clock (`clocksync`), at their own rates; a DeckLink card delivers at the
//!   signal's rate and timestamps on its own (`clocksync` passes through).
//! - **Memory.** `unixfdsink` offers `GstShmAllocator`, so frames are converted
//!   straight into memfds and cross the socket without a copy; it refuses
//!   anything else. The converter's pool has no upper bound and never stalls
//!   the capture: buffers come back as soon as chr-server releases them.
//! - **Identity.** After `clocksync` releases a frame, it gets `offset` =
//!   `frame_id` (counted from 0, continuous across loops of the file), its
//!   duration, a VchaoFrame meta with `source_ns` = its capture time on the
//!   same-host system clock (`chr_tracker::protocol::CLOCK_DOMAIN`), and chr's
//!   capture meta with its timecode and captions ([`ancillary`](super::ancillary)).
//!   `unixfdsink` turns its PTS into that same clock time for the core.
//! - **Formats.** Any extent and rate, progressive or interlaced (a frame
//!   carries both fields; its rate is the frame rate), from the caps. A new
//!   format is a new stream (a new stream id, `frame_id`s from 0 again), as
//!   vchao requires: the test pattern can switch formats while it plays
//!   ([`Input::switch`]), and a DeckLink signal can change.
//! - **Timecode.** The card's (RP 188), else the file's `timecode` tag counted
//!   on, else the time of day at the stream's start (UTC), counted on.
//! - **Loss.** [`Input::set_lost`] stops delivering frames and audio, as an
//!   input whose signal is gone; frames released more than a period late
//!   (after the process was stopped, say) are dropped, as a card never
//!   delivers a stale frame.
//! - **Never waits for the core:** without a connected core frames are
//!   dropped, and a core that does not read makes the queues drop their oldest.
//!
//! The capture ring of `chr-sdi-sim` is not written.

use std::{
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use chr_common::rate::Rate;
use chr_tracker::protocol::FrameMeta;
use gstreamer::{self as gst, prelude::*};

use super::{
    Error,
    ancillary::{self, Ancillary, Timecode},
    capture::format_of,
    decklink,
};
use crate::{
    sdi::{FieldOrder, Format, Scan, VideoMode},
    worker::{
        tracking::wire::{self, Wire},
        transport,
    },
};

/// What the input plays.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A video file at its own rate, progressive or interlaced, decoded by
    /// FFmpeg and looped, with its first audio stream.
    File(PathBuf),
    /// GStreamer's moving test pattern and a tick each second, without FFmpeg.
    TestPattern(Format),
    /// A DeckLink card's input and its embedded audio (not verified on hardware).
    DeckLink(decklink::Input),
}

/// How the input runs besides its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Frames that may wait for the core before the oldest is dropped.
    pub queue: u32,
    /// Send the source's audio on the audio socket.
    pub audio: bool,
    /// Audio channels.
    pub audio_channels: u32,
    /// Give the test pattern's frames synthetic CEA-608 captions.
    pub captions: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            queue: 4,
            audio: true,
            audio_channels: 2,
            captions: false,
        }
    }
}

/// Audio samples per second on the capture hop: SDI's embedded audio rate.
pub const AUDIO_RATE: u32 = 48_000;
/// Audio that may wait for the core before the oldest is dropped.
const AUDIO_QUEUE: Duration = Duration::from_millis(200);

/// The audio socket beside the capture socket `capture`: `<capture>.audio`.
pub fn audio_socket(capture: &Path) -> PathBuf {
    let mut path = capture.as_os_str().to_owned();
    path.push(".audio");
    path.into()
}

/// The capture hop's audio: 32-bit samples (SDI audio is 24-bit), 48 kHz,
/// interleaved; more than two channels are unpositioned (SDI's embedded
/// channels have no speaker positions).
pub fn audio_caps(channels: u32) -> gst::Caps {
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S32LE")
        .field("rate", AUDIO_RATE as i32)
        .field("channels", channels as i32)
        .field("layout", "interleaved");
    if channels > 2 {
        caps.field("channel-mask", gst::Bitmask::new(0)).build()
    } else {
        caps.build()
    }
}

pub struct Input {
    pipeline: gst::Pipeline,
    decoders: Vec<Decoder>,
    shared: Arc<Shared>,
    pattern: bool,
}

/// What the probes share with the input's owner.
#[derive(Default)]
struct Shared {
    lost: AtomicBool,
    stamped: AtomicU64,
    sent: AtomicU64,
    audio_sent: AtomicU64,
    stale: AtomicU64,
    stream: Mutex<Option<Stamping>>,
}

/// Where a stream's timecode comes from, unless the source's frames carry one.
#[derive(Clone, Debug)]
enum Timecodes {
    /// The file's `timecode` tag, counted on.
    Tagged(String),
    TimeOfDay,
}

impl Input {
    /// Starts playing `source` into a unixfd socket at `socket` (its audio
    /// into [`audio_socket`]`(socket)`).
    pub fn start(source: &Source, socket: &Path, options: &Options) -> Result<Self, Error> {
        transport::check()?;
        ancillary::init();
        let pipeline = gst::Pipeline::with_name("chr-sdi-in");
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        let mut decoders = Vec::new();
        let (front, format, audio, paced, timecodes) = match source {
            Source::File(path) => {
                let probed = probe(path)?;
                let decoder = Decoder::video(path, probed.format)?;
                let (interlaced, tff) = match probed.format.scan {
                    Scan::Progressive => (false, true),
                    Scan::Interlaced(order) => (true, order == FieldOrder::TopFirst),
                };
                let front = bin(&format!(
                    "fdsrc fd={} ! rawvideoparse format=uyvy width={} height={} framerate={}/{} \
                     interlaced={interlaced} top-field-first={tff} colorimetry=bt709 \
                     ! videoconvert n-threads={}",
                    decoder.pipe.as_raw_fd(),
                    probed.format.mode.width,
                    probed.format.mode.height,
                    probed.format.mode.fps.num(),
                    probed.format.mode.fps.den(),
                    threads(probed.format.mode)
                ))?;
                decoders.push(decoder);
                let audio = match (options.audio, probed.audio) {
                    (true, true) => {
                        let decoder = Decoder::audio(path, options.audio_channels)?;
                        let audio = bin(&format!(
                            "fdsrc fd={} ! rawaudioparse use-sink-caps=false format=pcm \
                             pcm-format=s32le sample-rate={AUDIO_RATE} num-channels={}",
                            decoder.pipe.as_raw_fd(),
                            options.audio_channels
                        ))?;
                        decoders.push(decoder);
                        Some(audio)
                    }
                    (true, false) => {
                        log::info!("{} has no audio", path.display());
                        None
                    }
                    (false, _) => None,
                };
                let timecodes = probed
                    .timecode
                    .map_or(Timecodes::TimeOfDay, Timecodes::Tagged);
                (front, Some(probed.format), audio, true, timecodes)
            }
            Source::TestPattern(format) => (
                bin("videotestsrc pattern=smpte horizontal-speed=4 is-live=false")?,
                Some(*format),
                options
                    .audio
                    .then(|| bin("audiotestsrc is-live=false wave=ticks samplesperbuffer=480"))
                    .transpose()?,
                true,
                Timecodes::TimeOfDay,
            ),
            Source::DeckLink(input) => {
                let front = gst::Bin::new();
                let card = decklink::video_source(input)?;
                let convert = gst::ElementFactory::make("videoconvert")
                    .property("n-threads", 4_u32)
                    .build()?;
                front.add_many([&card, &convert])?;
                card.link(&convert)?;
                ghost(&front, &convert)?;
                let audio = if options.audio {
                    let audio = gst::Bin::new();
                    let card = decklink::audio_source(input.device, options.audio_channels)?;
                    let convert = gst::ElementFactory::make("audioconvert").build()?;
                    audio.add_many([&card, &convert])?;
                    card.link(&convert)?;
                    ghost(&audio, &convert)?;
                    Some(audio)
                } else {
                    None
                };
                (
                    front,
                    input.mode.map(|mode| mode.format),
                    audio,
                    false,
                    Timecodes::TimeOfDay,
                )
            }
        };
        let socket_path = socket.to_str().ok_or("the socket path is not UTF-8")?;
        let _ = std::fs::remove_file(socket);
        let caps = gst::ElementFactory::make("capsfilter")
            .name("format")
            .property("caps", capture_caps(format))
            .build()?;
        let pace = gst::ElementFactory::make("clocksync")
            .name("pace")
            .property("sync", paced)
            .build()?;
        let queue = leaky_queue(options.queue, None)?;
        let send = unixfdsink(socket_path, true)?;
        pipeline.add_many([front.upcast_ref(), &caps, &pace, &queue, &send])?;
        gst::Element::link_many([front.upcast_ref(), &caps, &pace, &queue, &send])?;

        let shared = Arc::new(Shared::default());
        stamp(
            &pace,
            Arc::clone(&shared),
            Stamper {
                timecodes,
                captions: options.captions && matches!(source, Source::TestPattern(_)),
                paced,
            },
        )?;
        count(&send, Arc::clone(&shared), |shared| &shared.sent)?;
        if let Some(audio) = audio {
            let audio_path = audio_socket(socket);
            let _ = std::fs::remove_file(&audio_path);
            let audio_caps = gst::ElementFactory::make("capsfilter")
                .property("caps", audio_caps(options.audio_channels))
                .build()?;
            let audio_pace = gst::ElementFactory::make("clocksync")
                .property("sync", paced)
                .build()?;
            let audio_queue = leaky_queue(0, Some(AUDIO_QUEUE))?;
            let audio_send = unixfdsink(
                audio_path.to_str().ok_or("the socket path is not UTF-8")?,
                false,
            )?;
            pipeline.add_many([
                audio.upcast_ref(),
                &audio_caps,
                &audio_pace,
                &audio_queue,
                &audio_send,
            ])?;
            gst::Element::link_many([
                audio.upcast_ref(),
                &audio_caps,
                &audio_pace,
                &audio_queue,
                &audio_send,
            ])?;
            gate(&audio_pace, Arc::clone(&shared), paced)?;
            count(&audio_send, Arc::clone(&shared), |shared| {
                &shared.audio_sent
            })?;
            log::info!("audio into {}", audio_path.display());
        }
        let input = Self {
            pipeline,
            decoders,
            shared,
            pattern: matches!(source, Source::TestPattern(_)),
        };
        input.pipeline.set_state(gst::State::Playing)?;
        Ok(input)
    }

    /// The format of the frames sent now, once the first came.
    pub fn format(&self) -> Option<Format> {
        lock(&self.shared.stream)
            .as_ref()
            .map(|stamping| stamping.format)
    }

    /// The stream sent now, once the first frame came.
    pub fn stream_id(&self) -> Option<String> {
        lock(&self.shared.stream)
            .as_ref()
            .map(|stamping| stamping.wire.stream_id.clone())
    }

    /// Plays the test pattern in `format` from its next frame on: a new stream.
    pub fn switch(&mut self, format: Format) -> Result<(), Error> {
        if !self.pattern {
            return Err("only the test pattern switches its format".into());
        }
        by_name(&self.pipeline, "format")?.set_property("caps", capture_caps(Some(format)));
        Ok(())
    }

    /// Loses the input (no frames, no audio) or brings it back.
    pub fn set_lost(&self, lost: bool) {
        self.shared.lost.store(lost, Ordering::Release);
    }

    /// Frames stamped so far, frames handed to the socket, audio buffers
    /// handed to the audio socket and stale frames dropped.
    pub fn counts(&self) -> (u64, u64, u64, u64) {
        let shared = &self.shared;
        (
            shared.stamped.load(Ordering::Relaxed),
            shared.sent.load(Ordering::Relaxed),
            shared.audio_sent.load(Ordering::Relaxed),
            shared.stale.load(Ordering::Relaxed),
        )
    }

    /// Fails once the pipeline or a decoder stopped.
    pub fn check(&mut self) -> Result<(), Error> {
        if let Some(message) = self
            .pipeline
            .bus()
            .and_then(|bus| bus.pop_filtered(&[gst::MessageType::Error, gst::MessageType::Eos]))
        {
            return Err(match message.view() {
                gst::MessageView::Error(error) => format!("input pipeline: {}", error.error()),
                _ => "the input ended".to_owned(),
            }
            .into());
        }
        for decoder in &mut self.decoders {
            if let Some(status) = decoder.child.try_wait()? {
                return Err(format!("FFmpeg exited ({status})").into());
            }
        }
        Ok(())
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        // Before the decoders and their pipes go.
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A bin of `description` with a ghost src pad.
fn bin(description: &str) -> Result<gst::Bin, Error> {
    Ok(gst::parse::bin_from_description(description, true)?)
}

/// Gives `bin` the src pad of `last`.
fn ghost(bin: &gst::Bin, last: &gst::Element) -> Result<(), Error> {
    let pad = last.static_pad("src").ok_or("no src pad")?;
    bin.add_pad(&gst::GhostPad::with_target(&pad)?)?;
    Ok(())
}

/// A queue that drops its oldest buffer beyond `buffers` buffers or `time`.
fn leaky_queue(buffers: u32, time: Option<Duration>) -> Result<gst::Element, Error> {
    let queue = gst::ElementFactory::make("queue")
        .property("max-size-buffers", buffers)
        .property("max-size-bytes", 0_u32)
        .property(
            "max-size-time",
            time.map_or(0, |time| time.as_nanos() as u64),
        )
        .build()?;
    queue.set_property_from_str("leaky", "downstream");
    Ok(queue)
}

/// The listening side of a capture-hop socket; `fd_only` refuses memory that
/// is not a file descriptor (frames are converted into memfds; audio is
/// small and copied).
fn unixfdsink(path: &str, fd_only: bool) -> Result<gst::Element, Error> {
    let sink = gst::ElementFactory::make("unixfdsink")
        .property("socket-path", path)
        .property("wait-for-connection", false)
        .property("sync", false)
        .property("async", false)
        .property("enable-last-sample", false)
        .build()?;
    if fd_only {
        sink.set_property("min-memory-size", -1_i64);
    }
    Ok(sink)
}

/// Counts the buffers `sink` takes.
fn count(
    sink: &gst::Element,
    shared: Arc<Shared>,
    counter: fn(&Shared) -> &AtomicU64,
) -> Result<(), Error> {
    sink.static_pad("sink")
        .ok_or("unixfdsink has no sink pad")?
        .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            counter(&shared).fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
    Ok(())
}

/// FFmpeg decoding a file in an endless loop to raw video or audio on its
/// stdout, unpaced: `clocksync` sets the pace, and the pipe holds FFmpeg back.
/// Video and audio are two processes, each paced by its own reader, so neither
/// stream can stall the other; both start at the file's start and run on the
/// same running time. A looped file keeps them aligned if its streams last
/// equally long (FFmpeg fills an audio gap with silence).
struct Decoder {
    child: Child,
    pipe: ChildStdout,
}

impl Decoder {
    fn video(path: &Path, format: Format) -> Result<Self, Error> {
        // Interlaced 4:2:0 chroma is per field: convert each field on its own.
        let interlaced = u8::from(format.scan != Scan::Progressive);
        Self::start(
            path,
            &[
                "-threads",
                &threads(format.mode).to_string(),
                "-map",
                "0:v:0",
                "-an",
                "-sn",
                "-dn",
                "-vf",
                &format!(
                    "scale=in_range=tv:out_range=tv:in_color_matrix=bt709:out_color_matrix=bt709:\
                     interl={interlaced},format=uyvy422"
                ),
                "-fps_mode",
                "passthrough",
                "-c:v",
                "rawvideo",
                "-f",
                "rawvideo",
            ],
        )
    }

    fn audio(path: &Path, channels: u32) -> Result<Self, Error> {
        Self::start(
            path,
            &[
                "-map",
                "0:a:0",
                "-vn",
                "-sn",
                "-dn",
                "-af",
                "aresample=async=1:first_pts=0",
                "-ac",
                &channels.to_string(),
                "-ar",
                &AUDIO_RATE.to_string(),
                "-c:a",
                "pcm_s32le",
                "-f",
                "s32le",
            ],
        )
    }

    fn start(path: &Path, output: &[&str]) -> Result<Self, Error> {
        let mut child = Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-stream_loop",
                "-1",
                "-i",
            ])
            .arg(path)
            .args(output)
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot run ffmpeg (the media shell has it): {error}"))?;
        match child.stdout.take() {
            Some(pipe) => Ok(Self { child, pipe }),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                Err("FFmpeg stdout was not piped".into())
            }
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn by_name(pipeline: &gst::Pipeline, name: &str) -> Result<gst::Element, Error> {
    pipeline
        .by_name(name)
        .ok_or_else(|| format!("the input has no {name} element").into())
}

/// A stream ID unique to this process and its `n`th format.
fn stream_id(n: u64) -> String {
    format!("chr-sdi-in-{}-{}-{n}", std::process::id(), wire::now_ns())
}

/// What the converter must produce: RGBA, BT.709 limited range, in `format`
/// if the source does not set it (a DeckLink card in `mode=auto` does).
fn capture_caps(format: Option<Format>) -> gst::Caps {
    let mut caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .field("colorimetry", "1:1:5:1")
        .build();
    if let Some(format) = format {
        let structure = caps.make_mut().structure_mut(0).expect("one structure");
        structure.set("width", format.mode.width as i32);
        structure.set("height", format.mode.height as i32);
        structure.set("framerate", wire::fraction(format.mode.fps));
        let (mode, order) = format.scan.caps_fields();
        structure.set("interlace-mode", mode);
        if let Some(order) = order {
            structure.set("field-order", order);
        }
    }
    caps
}

/// The capture hop's caps of `caps` in stream `wire`: the converter's caps
/// plus vchao's `vchao-*` wire fields, the interlacing as it is.
fn stream_caps(caps: &gst::CapsRef, wire: &Wire) -> gst::Caps {
    let mut stream = caps.to_owned();
    let fields = wire.caps();
    let structure = stream.make_mut().structure_mut(0).expect("one structure");
    for (name, value) in fields.structure(0).expect("one structure").iter() {
        if name.starts_with("vchao-") {
            structure.set_value(name, value.clone());
        }
    }
    stream
}

/// The stream being stamped and its next `frame_id`.
struct Stamping {
    wire: Wire,
    format: Format,
    next: u64,
    /// The timecode of `frame_id` 0, unless the frames carry their own.
    timecode: Option<Timecode>,
}

/// How [`stamp`] treats frames besides numbering them.
struct Stamper {
    timecodes: Timecodes,
    /// Synthetic CEA-608 captions.
    captions: bool,
    /// Frames are paced by `clocksync`: one released over a period late is stale.
    paced: bool,
}

impl Stamper {
    /// The timecode of frame `frame_id` of `stamping`'s stream, else of
    /// `buffer` (a card's).
    fn timecode(
        &self,
        stamping: &mut Stamping,
        frame_id: u64,
        capture_ns: u64,
    ) -> Result<Timecode, Error> {
        let start = match stamping.timecode {
            Some(start) => start,
            None => {
                let (fps, scan) = (stamping.format.mode.fps, stamping.format.scan);
                let start = match &self.timecodes {
                    Timecodes::Tagged(tag) => Timecode::parse(tag, fps, scan)?,
                    Timecodes::TimeOfDay => {
                        // The wall clock at this frame's capture.
                        let behind = wire::now_ns().saturating_sub(capture_ns);
                        let now = SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)?
                            .saturating_sub(Duration::from_nanos(behind));
                        Timecode::time_of_day(fps, scan, now)?
                    }
                };
                *stamping.timecode.insert(start)
            }
        };
        start.add_frames(frame_id)
    }
}

/// Stamps every frame `clocksync` releases with its identity and ancillary
/// data (see the module), in the stream of the caps it comes with, and gives
/// each new format a new stream.
fn stamp(pace: &gst::Element, shared: Arc<Shared>, stamper: Stamper) -> Result<(), Error> {
    let element = pace.downgrade();
    let streams = AtomicU64::new(0);
    pace.static_pad("src")
        .ok_or("clocksync has no src pad")?
        .add_probe(
            gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
            move |_, info| {
                let mut stream = lock(&shared.stream);
                if let Some(gst::PadProbeData::Event(event)) = &info.data {
                    let gst::EventView::Caps(caps) = event.view() else {
                        return gst::PadProbeReturn::Ok;
                    };
                    let format = match format_of(caps.caps()) {
                        Ok(format) => format,
                        Err(error) => {
                            log::error!("caps of no format: {error}");
                            return gst::PadProbeReturn::Drop;
                        }
                    };
                    if stream
                        .as_ref()
                        .is_none_or(|stamping| stamping.format != format)
                    {
                        let wire = Wire {
                            stream_id: stream_id(streams.fetch_add(1, Ordering::Relaxed) + 1),
                            width: format.mode.width,
                            height: format.mode.height,
                            fps: format.mode.fps,
                        };
                        log::info!("stream {}: {}", wire.stream_id, format.name());
                        *stream = Some(Stamping {
                            wire,
                            format,
                            next: 0,
                            timecode: None,
                        });
                    }
                    let wire = &stream.as_ref().expect("set above").wire;
                    info.data = Some(gst::PadProbeData::Event(gst::event::Caps::new(
                        &stream_caps(caps.caps(), wire),
                    )));
                    return gst::PadProbeReturn::Ok;
                }
                let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data else {
                    return gst::PadProbeReturn::Ok;
                };
                let Some(element) = element.upgrade() else {
                    return gst::PadProbeReturn::Drop;
                };
                let (Some(base), Some(clock), Some(pts), Some(stamping)) = (
                    element.base_time(),
                    element.clock(),
                    buffer.pts(),
                    stream.as_mut(),
                ) else {
                    return gst::PadProbeReturn::Drop;
                };
                if shared.lost.load(Ordering::Acquire) {
                    return gst::PadProbeReturn::Drop;
                }
                let capture_ns = (base + pts).nseconds();
                let duration_ns = stamping.wire.duration_ns();
                if stamper.paced && clock.time().nseconds() > capture_ns + duration_ns {
                    shared.stale.fetch_add(1, Ordering::Relaxed);
                    return gst::PadProbeReturn::Drop;
                }
                let frame_id = stamping.next;
                stamping.next += 1;
                shared.stamped.fetch_add(1, Ordering::Relaxed);
                let mut ancillary = Ancillary::of_gst_metas(buffer);
                if ancillary.timecode.is_none() {
                    match stamper.timecode(stamping, frame_id, capture_ns) {
                        Ok(timecode) => ancillary.timecode = Some(timecode),
                        Err(error) => log::warn!("frame {frame_id} has no timecode: {error}"),
                    }
                }
                if stamper.captions {
                    ancillary.captions.push(ancillary::cea608_digits(frame_id));
                }
                let buffer = buffer.make_mut();
                buffer.set_offset(frame_id);
                buffer.set_offset_end(frame_id + 1);
                buffer.set_duration(gst::ClockTime::from_nseconds(duration_ns));
                let meta =
                    FrameMeta::request(&stamping.wire.stream_id, frame_id, capture_ns, duration_ns);
                match wire::attach(buffer, &meta).and_then(|()| ancillary.attach(buffer)) {
                    Ok(()) => gst::PadProbeReturn::Ok,
                    Err(error) => {
                        log::error!("frame {frame_id} not stamped: {error}");
                        gst::PadProbeReturn::Drop
                    }
                }
            },
        );
    Ok(())
}

/// Stamps every audio buffer with its capture time on the same-host system
/// clock (chr's capture meta); drops audio while the input is lost, and stale
/// audio as [`stamp`] drops stale frames.
fn gate(pace: &gst::Element, shared: Arc<Shared>, paced: bool) -> Result<(), Error> {
    let element = pace.downgrade();
    pace.static_pad("src")
        .ok_or("clocksync has no src pad")?
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data else {
                return gst::PadProbeReturn::Ok;
            };
            let (Some(element), Some(pts)) = (element.upgrade(), buffer.pts()) else {
                return gst::PadProbeReturn::Drop;
            };
            let (Some(base), Some(clock)) = (element.base_time(), element.clock()) else {
                return gst::PadProbeReturn::Drop;
            };
            let captured = base + pts;
            if shared.lost.load(Ordering::Acquire)
                || (paced && clock.time() > captured + buffer.duration().unwrap_or_default() * 4)
            {
                return gst::PadProbeReturn::Drop;
            }
            let ancillary = Ancillary {
                capture_ns: Some(captured.nseconds()),
                ..Ancillary::default()
            };
            match ancillary.attach(buffer.make_mut()) {
                Ok(()) => gst::PadProbeReturn::Ok,
                Err(error) => {
                    log::error!("audio not stamped: {error}");
                    gst::PadProbeReturn::Drop
                }
            }
        });
    Ok(())
}

/// A file as the input plays it.
struct Probed {
    format: Format,
    /// Its `timecode` tag, `hh:mm:ss:ff`.
    timecode: Option<String>,
    audio: bool,
}

/// The file's extent, rate and scan (1 to 120 frames per second, integer or
/// fractional; `r_frame_rate` such as `30000/1001` is the frame rate, of
/// interlaced video too), its timecode and whether it has audio.
fn probe(path: &Path) -> Result<Probed, Error> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,width,height,r_frame_rate,field_order:stream_tags=timecode\
             :format_tags=timecode",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .map_err(|error| format!("cannot run ffprobe (the media shell has it): {error}"))?;
    if !output.status.success() {
        return Err(format!("ffprobe cannot read {}", path.display()).into());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let streams = metadata["streams"].as_array().cloned().unwrap_or_default();
    let stream = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .ok_or_else(|| format!("{} has no video", path.display()))?;
    let dimension = |key: &str| {
        stream[key]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| format!("{} has no video {key}", path.display()))
    };
    let rate = stream["r_frame_rate"].as_str().unwrap_or_default();
    let fps: Rate = rate
        .parse()
        .map_err(|error| format!("{} runs at {rate:?}: {error}", path.display()))?;
    let scan = scan_of(stream["field_order"].as_str());
    let format = Format {
        mode: VideoMode {
            width: dimension("width")?,
            height: dimension("height")?,
            fps,
        },
        scan,
    };
    if !format.mode.width.is_multiple_of(2) || !format.mode.height.is_multiple_of(2) {
        return Err(format!(
            "{} is {}: UYVY needs an even extent",
            path.display(),
            format.name()
        )
        .into());
    }
    let timecode = [
        &stream["tags"]["timecode"],
        &metadata["format"]["tags"]["timecode"],
    ]
    .into_iter()
    .find_map(|tag| tag.as_str().map(str::to_owned));
    Ok(Probed {
        format,
        timecode,
        audio: streams.iter().any(|stream| stream["codec_type"] == "audio"),
    })
}

/// FFprobe's `field_order`: the order fields are displayed in (`tb` is top
/// coded first, bottom displayed first).
fn scan_of(field_order: Option<&str>) -> Scan {
    match field_order {
        Some("tt" | "bt") => Scan::Interlaced(FieldOrder::TopFirst),
        Some("bb" | "tb") => Scan::Interlaced(FieldOrder::BottomFirst),
        _ => Scan::Progressive,
    }
}

/// Two threads per started 1080p frame of pixels, 2 to 8.
fn threads(mode: VideoMode) -> u32 {
    (mode.width * mode.height)
        .div_ceil(1920 * 1080)
        .saturating_mul(2)
        .clamp(2, 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interlaced_files_keep_their_display_field_order() {
        assert_eq!(scan_of(Some("tt")), Scan::Interlaced(FieldOrder::TopFirst));
        assert_eq!(
            scan_of(Some("tb")),
            Scan::Interlaced(FieldOrder::BottomFirst)
        );
        assert_eq!(scan_of(Some("progressive")), Scan::Progressive);
        assert_eq!(scan_of(None), Scan::Progressive);
        assert_eq!(
            audio_socket(Path::new("/run/chr/capture.sock")),
            Path::new("/run/chr/capture.sock.audio")
        );
    }

    #[test]
    fn stream_caps_add_the_wire_fields_and_keep_the_interlacing() {
        gst::init().unwrap();
        let i50 = Format {
            mode: VideoMode {
                width: 64,
                height: 36,
                fps: Rate::fps(25),
            },
            scan: Scan::Interlaced(FieldOrder::TopFirst),
        };
        let wire = Wire {
            stream_id: "s".into(),
            width: 64,
            height: 36,
            fps: Rate::fps(25),
        };
        let caps = stream_caps(&capture_caps(Some(i50)), &wire);
        wire::check_caps(&caps, "s").unwrap();
        assert_eq!(format_of(&caps).unwrap(), i50);
    }
}
