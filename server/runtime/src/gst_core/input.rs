//! The INPUT process's pipeline (`chr-sdi-in`): a simulated SDI input for the
//! GStreamer core (#154). It plays a source at the source's own rate (integer
//! or fractional), paced in real time on the system clock, stamps every frame
//! with its identity and hands it to chr-server over a unixfd socket:
//!
//! ```text
//! file:    ffmpeg (decode, loop, UYVY) | fdsrc ! rawvideoparse ! videoconvert ─┐
//! pattern: videotestsrc ───────────────────────────────────────────────────────┤
//!   ! video/x-raw,format=RGBA,<VchaoFrame wire caps> ! clocksync ! [stamp]
//!   ! queue leaky=downstream ! unixfdsink
//! ```
//!
//! - **Memory.** `unixfdsink` offers `GstShmAllocator`, so frames are converted
//!   straight into memfds and cross the socket without a copy; it refuses
//!   anything else. The converter's pool has no upper bound and never stalls
//!   the capture: buffers come back as soon as chr-server releases them, so
//!   the frames alive are those the core holds (about the program delay times
//!   the rate, plus the workers' frames in flight) plus this queue.
//! - **Identity.** After `clocksync` releases a frame, it gets `offset` =
//!   `frame_id` (counted from 0, continuous across loops of the file), its
//!   duration, and a VchaoFrame meta with `source_ns` = its release time on the
//!   same-host system clock (`chr_tracker::protocol::CLOCK_DOMAIN`); `unixfdsink`
//!   turns its PTS into that same clock time for the core.
//! - **Format changes.** The test pattern can switch to another extent or rate
//!   while it plays ([`Input::switch`]), as an SDI input whose signal changes:
//!   new caps cross the socket, and a new format is a new stream (a new stream
//!   id, `frame_id`s from 0 again), as vchao requires. Frame times run on
//!   without a gap: the pattern's first frame in the new format is due when
//!   the next one in the old format would have been.
//! - **Never waits for the core:** without a connected core frames are
//!   dropped, and a core that does not read makes the queue drop its oldest.
//!
//! The capture ring of `chr-sdi-sim` is not written.

use std::{
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use chr_common::rate::Rate;
use chr_tracker::protocol::FrameMeta;
use gstreamer::{self as gst, prelude::*};

use super::Error;
use crate::{
    sdi::VideoMode,
    worker::{
        tracking::wire::{self, Wire},
        transport,
    },
};

/// What the input plays.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A progressive video file at its own rate, decoded by FFmpeg and looped.
    File(PathBuf),
    /// GStreamer's moving test pattern, without FFmpeg.
    TestPattern(VideoMode),
}

pub struct Input {
    pipeline: gst::Pipeline,
    decoder: Option<Decoder>,
    mode: VideoMode,
    stream_id: String,
    /// Formats played so far; numbers the streams.
    streams: u64,
    stamped: Arc<AtomicU64>,
    sent: Arc<AtomicU64>,
}

impl Input {
    /// Starts playing `source` into a unixfd socket at `socket`; at most
    /// `queue` frames wait for the core.
    pub fn start(source: &Source, socket: &Path, queue: u32) -> Result<Self, Error> {
        transport::check()?;
        let (mode, decoder, front) = match source {
            Source::File(path) => {
                let mode = probe(path)?;
                let decoder = Decoder::start(path, mode)?;
                let front = format!(
                    "fdsrc fd={} ! rawvideoparse format=uyvy width={} height={} framerate={}/{} \
                     colorimetry=bt709 ! videoconvert n-threads={}",
                    decoder.pipe.as_raw_fd(),
                    mode.width,
                    mode.height,
                    mode.fps.num(),
                    mode.fps.den(),
                    threads(mode)
                );
                (mode, Some(decoder), front)
            }
            Source::TestPattern(mode) => (
                *mode,
                None,
                "videotestsrc pattern=smpte horizontal-speed=4 is-live=false".to_owned(),
            ),
        };
        let socket_path = socket.to_str().ok_or("the socket path is not UTF-8")?;
        let _ = std::fs::remove_file(socket);
        let pipeline = gst::parse::launch(&format!(
            "{front} ! capsfilter name=format ! clocksync name=pace \
             ! queue leaky=downstream max-size-buffers={queue} max-size-bytes=0 max-size-time=0 \
             ! unixfdsink name=send socket-path=\"{socket_path}\" wait-for-connection=false \
               min-memory-size=-1 sync=false async=false enable-last-sample=false"
        ))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "the input is not a pipeline")?;
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        let stream_id = stream_id(1);
        by_name(&pipeline, "format")?.set_property("caps", wire_caps(&stream_id, mode));
        let stamped = Arc::new(AtomicU64::new(0));
        stamp(&by_name(&pipeline, "pace")?, Arc::clone(&stamped))?;
        let sent = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&sent);
        by_name(&pipeline, "send")?
            .static_pad("sink")
            .ok_or("unixfdsink has no sink pad")?
            .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                counted.fetch_add(1, Ordering::Relaxed);
                gst::PadProbeReturn::Ok
            });
        let input = Self {
            pipeline,
            decoder,
            mode,
            stream_id,
            streams: 1,
            stamped,
            sent,
        };
        input.pipeline.set_state(gst::State::Playing)?;
        Ok(input)
    }

    pub fn mode(&self) -> VideoMode {
        self.mode
    }

    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Plays the test pattern in `mode` from its next frame on: a new stream.
    pub fn switch(&mut self, mode: VideoMode) -> Result<(), Error> {
        if self.decoder.is_some() {
            return Err("only the test pattern switches its format".into());
        }
        self.streams += 1;
        self.stream_id = stream_id(self.streams);
        self.mode = mode;
        by_name(&self.pipeline, "format")?.set_property("caps", wire_caps(&self.stream_id, mode));
        Ok(())
    }

    /// Frames stamped so far, and frames handed to the socket.
    pub fn counts(&self) -> (u64, u64) {
        (
            self.stamped.load(Ordering::Relaxed),
            self.sent.load(Ordering::Relaxed),
        )
    }

    /// Fails once the pipeline or the decoder stopped.
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
        if let Some(decoder) = &mut self.decoder
            && let Some(status) = decoder.child.try_wait()?
        {
            return Err(format!("FFmpeg exited ({status})").into());
        }
        Ok(())
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        // Before the decoder and its pipe go.
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// FFmpeg decoding a file in an endless loop to raw UYVY on its stdout,
/// unpaced: `clocksync` sets the pace, and the pipe holds FFmpeg back.
struct Decoder {
    child: Child,
    pipe: ChildStdout,
}

impl Decoder {
    fn start(path: &Path, mode: VideoMode) -> Result<Self, Error> {
        let threads = threads(mode).to_string();
        let mut child = Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-threads",
                &threads,
                "-stream_loop",
                "-1",
                "-i",
            ])
            .arg(path)
            .args([
                "-map",
                "0:v:0",
                "-an",
                "-sn",
                "-dn",
                "-vf",
                "scale=in_range=tv:out_range=tv:in_color_matrix=bt709:out_color_matrix=bt709,format=uyvy422",
                "-fps_mode",
                "passthrough",
                "-c:v",
                "rawvideo",
                "-f",
                "rawvideo",
                "pipe:1",
            ])
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

/// The capsfilter caps of stream `stream_id` in `mode`: vchao's wire caps,
/// BT.709 limited range.
fn wire_caps(stream_id: &str, mode: VideoMode) -> gst::Caps {
    let mut caps = Wire {
        stream_id: stream_id.to_owned(),
        width: mode.width,
        height: mode.height,
        fps: mode.fps,
    }
    .caps();
    caps.make_mut()
        .structure_mut(0)
        .expect("one structure")
        .set("colorimetry", "1:1:5:1");
    caps
}

/// The stream of caps that `wire_caps` made.
fn wire_of(caps: &gst::CapsRef) -> Result<Wire, Error> {
    let structure = caps.structure(0).ok_or("caps without a structure")?;
    Ok(Wire {
        stream_id: structure.get::<String>("vchao-stream-id")?,
        width: u32::try_from(structure.get::<i32>("width")?)?,
        height: u32::try_from(structure.get::<i32>("height")?)?,
        fps: wire::rate_of(structure.get::<gst::Fraction>("framerate")?)?,
    })
}

/// The stream being stamped and its next `frame_id`.
struct Stamping {
    wire: Wire,
    next: u64,
}

/// Stamps every frame `clocksync` releases with its identity (see the module),
/// in the stream of the caps it comes with.
fn stamp(pace: &gst::Element, stamped: Arc<AtomicU64>) -> Result<(), Error> {
    let element = pace.downgrade();
    let stream: Mutex<Option<Stamping>> = Mutex::new(None);
    pace.static_pad("src")
        .ok_or("clocksync has no src pad")?
        .add_probe(
            gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
            move |_, info| {
                let mut stream = stream.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(gst::PadProbeData::Event(event)) = &info.data {
                    if let gst::EventView::Caps(caps) = event.view() {
                        match wire_of(caps.caps()) {
                            Ok(wire) => {
                                log::info!(
                                    "stream {}: {}x{} at {} fps",
                                    wire.stream_id,
                                    wire.width,
                                    wire.height,
                                    wire.fps.label()
                                );
                                *stream = Some(Stamping { wire, next: 0 });
                            }
                            Err(error) => log::error!("caps without a stream: {error}"),
                        }
                    }
                    return gst::PadProbeReturn::Ok;
                }
                let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data else {
                    return gst::PadProbeReturn::Ok;
                };
                let (Some(base), Some(pts), Some(stamping)) = (
                    element.upgrade().and_then(|element| element.base_time()),
                    buffer.pts(),
                    stream.as_mut(),
                ) else {
                    return gst::PadProbeReturn::Drop;
                };
                let frame_id = stamping.next;
                stamping.next += 1;
                stamped.fetch_add(1, Ordering::Relaxed);
                let duration_ns = stamping.wire.duration_ns();
                let buffer = buffer.make_mut();
                buffer.set_offset(frame_id);
                buffer.set_offset_end(frame_id + 1);
                buffer.set_duration(gst::ClockTime::from_nseconds(duration_ns));
                let meta = FrameMeta::request(
                    &stamping.wire.stream_id,
                    frame_id,
                    (base + pts).nseconds(),
                    duration_ns,
                );
                match wire::attach(buffer, &meta) {
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

/// The file's extent and rate: progressive video at 1 to 120 fps, integer or
/// fractional (`r_frame_rate` such as `30000/1001`).
fn probe(path: &Path) -> Result<VideoMode, Error> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,r_frame_rate,field_order",
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
    let stream = &metadata["streams"][0];
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
    if matches!(
        stream["field_order"].as_str(),
        Some("tt" | "bb" | "tb" | "bt")
    ) {
        return Err(format!(
            "{} is interlaced; the input takes progressive video (interlaced formats are \
             phase 3 of #154)",
            path.display()
        )
        .into());
    }
    let mode = VideoMode {
        width: dimension("width")?,
        height: dimension("height")?,
        fps,
    };
    if !mode.width.is_multiple_of(2) || !mode.height.is_multiple_of(2) {
        return Err(format!(
            "{} is {}: UYVY needs an even extent",
            path.display(),
            mode.name()
        )
        .into());
    }
    Ok(mode)
}

/// Two threads per started 1080p frame of pixels, 2 to 8.
fn threads(mode: VideoMode) -> u32 {
    (mode.width * mode.height)
        .div_ceil(1920 * 1080)
        .saturating_mul(2)
        .clamp(2, 8)
}
