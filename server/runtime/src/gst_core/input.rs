//! The INPUT process's pipeline (`chr-sdi-in`): a simulated SDI input for the
//! GStreamer core (#154). It plays a source at the source's own rate, paced in
//! real time on the system clock, stamps every frame with its identity and
//! hands it to chr-server over a unixfd socket:
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
//! - **Never waits for the core:** without a connected core frames are
//!   dropped, and a core that does not read makes the queue drop its oldest.
//!
//! The capture ring of `chr-sdi-sim` is not written.

use std::{
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

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
    /// A progressive video file at an integer rate, decoded by FFmpeg and looped.
    File(PathBuf),
    /// GStreamer's moving test pattern, without FFmpeg.
    TestPattern(VideoMode),
}

pub struct Input {
    pipeline: gst::Pipeline,
    decoder: Option<Decoder>,
    mode: VideoMode,
    stream_id: String,
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
                    "fdsrc fd={} ! rawvideoparse format=uyvy width={} height={} framerate={}/1 \
                     colorimetry=bt709 ! videoconvert n-threads={}",
                    decoder.pipe.as_raw_fd(),
                    mode.width,
                    mode.height,
                    mode.fps,
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
        let stream_id = format!("chr-sdi-in-{}-{}", std::process::id(), wire::now_ns());
        let wire = Wire {
            stream_id: stream_id.clone(),
            width: mode.width,
            height: mode.height,
            fps: mode.fps,
        };
        let mut caps = wire.caps();
        caps.make_mut()
            .structure_mut(0)
            .expect("one structure")
            .set("colorimetry", "1:1:5:1");
        by_name(&pipeline, "format")?.set_property("caps", &caps);
        let stamped = Arc::new(AtomicU64::new(0));
        stamp(&by_name(&pipeline, "pace")?, wire, Arc::clone(&stamped))?;
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

/// Stamps every frame `clocksync` releases with its identity (see the module).
fn stamp(pace: &gst::Element, wire: Wire, stamped: Arc<AtomicU64>) -> Result<(), Error> {
    let duration_ns = wire.duration_ns();
    let element = pace.downgrade();
    pace.static_pad("src")
        .ok_or("clocksync has no src pad")?
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data else {
                return gst::PadProbeReturn::Ok;
            };
            let (Some(base), Some(pts)) = (
                element.upgrade().and_then(|element| element.base_time()),
                buffer.pts(),
            ) else {
                return gst::PadProbeReturn::Drop;
            };
            let frame_id = stamped.fetch_add(1, Ordering::Relaxed);
            let buffer = buffer.make_mut();
            buffer.set_offset(frame_id);
            buffer.set_offset_end(frame_id + 1);
            buffer.set_duration(gst::ClockTime::from_nseconds(duration_ns));
            let meta = FrameMeta::request(
                &wire.stream_id,
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
        });
    Ok(())
}

/// The file's extent and rate. Phase 1 takes progressive video at an
/// integer rate; fractional rates such as 30000/1001 are phase 2.
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
    let fps = match rate.split_once('/') {
        Some((fps, "1")) => fps.parse::<u32>().ok(),
        _ => None,
    }
    .filter(|fps| (1..=120).contains(fps))
    .ok_or_else(|| {
        format!(
            "{} runs at {rate:?}: phase 1 takes integer rates of 1-120 fps (fractional rates \
             are phase 2 of #154)",
            path.display()
        )
    })?;
    if matches!(
        stream["field_order"].as_str(),
        Some("tt" | "bb" | "tb" | "bt")
    ) {
        return Err(format!(
            "{} is interlaced; phase 1 takes progressive video",
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
