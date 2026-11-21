//! The recording worker's side: it connects to a [`Tap`](super::Tap)'s socket
//! and writes what arrives as segments, one per format.
//!
//! ```text
//! unixfdsrc ! appsink ─► segment: appsrc ! <encoder> ! h264parse
//!                                 ! mp4mux ! filesink      <dir>/<stream>-NNNN.mp4
//!                        sidecar: <dir>/<stream>-NNNN.jsonl, one line per frame
//! ```
//!
//! The [`Encoder`] is picked once per worker: the Intel iGPU's (VA-API
//! `vapostproc ! vah264lpenc`, colour conversion and encoding both on the
//! iGPU) when it encodes a probe frame, else openh264 on the CPU.
//!
//! A segment ends when the frames' caps change (a new input format, the
//! program following it), when the stream ends and when the worker stops; the
//! next one takes the next number, also across worker restarts. The MP4 is
//! written as it records, fragment by fragment (one a second) after an index
//! at its start, and unbuffered: a killed worker leaves a playable file up to
//! its last complete fragment. A segment that ends normally is finalised into
//! a plain MP4 (`fragment-mode=first-moov-then-finalise`). Every frame carries
//! its duration, the recorded rate's period, so that the last one has one.
//! Timestamps start at 0 in every segment; the sidecar keeps each frame's own. Interlaced frames are encoded as they are, both fields woven in
//! one progressive picture; the sidecar's header names their scan.
//!
//! At a recording rate N (`--fps`), a segment keeps frames on an N fps
//! cadence of their own timestamps ([`Cadence`]) and tells the encoder N; it
//! keeps every frame of an input at or below N.
//!
//! The encoder's queue is bounded: when the encoder falls behind, frames are
//! dropped (counted, and missing from the sidecar too) and released at once.
//! So is the socket's: frames the worker has not taken yet (while it closes a
//! segment, or writes to a slow disk) are dropped beyond a few. The worker never
//! holds more than a few of chr-server's frames.

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime},
};

use chr_common::rate::Rate;
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::AppSrc;
use serde_json::{Value, json};

use super::Stream;
use crate::{
    gst_core::{self, PROGRAM_META, ancillary::Ancillary, capture::format_of},
    worker::{
        tracking::wire,
        transport::{Error, Receiver},
    },
};

/// The H.264 encoder of every segment of a worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoder {
    /// The Intel iGPU's fixed-function (low-power) encoder through VA-API:
    /// `vapostproc` converts the frames to NV12 on the iGPU, so the CPU only
    /// hands them over.
    Va,
    /// gst-plugins-bad's openh264 on the CPU (the root Nix shell has no x264),
    /// sliced for its threads: where VA-API has no Intel GPU.
    OpenH264,
}

impl Encoder {
    /// The iGPU's encoder when it encodes a probe frame, else openh264.
    pub fn pick() -> Self {
        match probe(Self::Va) {
            Ok(()) => Self::Va,
            Err(error) => {
                log::warn!(
                    "the iGPU's H.264 encoder is unavailable ({error}); encoding with openh264 \
                     on the CPU"
                );
                Self::OpenH264
            }
        }
    }

    /// Its name in logs and sidecars.
    pub fn name(self) -> &'static str {
        match self {
            Self::Va => "vah264lpenc",
            Self::OpenH264 => "openh264enc",
        }
    }

    /// From raw frames in any format to H.264, in `gst-launch` syntax: 40 Mbit/s
    /// (VA-API's VBR: at most that, less for simple pictures; openh264's
    /// average) and a key frame every 50 frames.
    fn pipeline(self) -> &'static str {
        match self {
            Self::Va => {
                "vapostproc ! video/x-raw(memory:VAMemory),format=NV12 ! vah264lpenc \
                 rate-control=vbr bitrate=40000 key-int-max=50 b-frames=0"
            }
            Self::OpenH264 => {
                "videoconvert n-threads=4 ! video/x-raw,format=I420 ! openh264enc complexity=low \
                 rate-control=bitrate bitrate=40000000 max-bitrate=80000000 gop-size=50 \
                 multi-thread=8 slice-mode=n-slices num-slices=8"
            }
        }
    }
}

/// Checks that `encoder` encodes a frame of RGBA.
fn probe(encoder: Encoder) -> Result<(), Error> {
    gst_core::init()?;
    if gst::ElementFactory::find(encoder.name()).is_none() {
        return Err(format!(
            "GStreamer has no {}; VA-API needs an Intel GPU and its driver on \
             LIBVA_DRIVERS_PATH",
            encoder.name()
        )
        .into());
    }
    let pipeline = gst::parse::launch(&format!(
        "videotestsrc num-buffers=1 ! video/x-raw,format=RGBA,width=192,height=128,framerate=50/1 \
         ! {} ! fakesink",
        encoder.pipeline()
    ))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| "the encoder probe is not a pipeline")?;
    pipeline.set_state(gst::State::Playing)?;
    let message = pipeline.bus().and_then(|bus| {
        bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
    });
    let _ = pipeline.set_state(gst::State::Null);
    match message.as_ref().map(|message| message.view()) {
        Some(gst::MessageView::Eos(_)) => Ok(()),
        Some(gst::MessageView::Error(error)) => Err(error.error().to_string().into()),
        _ => Err(format!("{} did not encode a probe frame within 5 s", encoder.name()).into()),
    }
}
/// Frames waiting for the encoder before frames are dropped.
const QUEUE: u64 = 8;
/// How long a segment's encoder may take to drain at its end.
const DRAIN: Duration = Duration::from_secs(5);
/// How long the worker tries to connect to a stream's socket at start.
const CONNECT: Duration = Duration::from_secs(5);
/// How often a waiting worker checks whether it should stop.
const POLL: Duration = Duration::from_millis(200);
/// Frames received but not yet taken: a worker held up (closing a segment,
/// a slow disk) drops older ones and releases chr-server's memory.
const RECEIVED: u32 = 4;

/// Records `stream` from `socket` into `dir` with `encoder`, at most `fps`
/// frames per second if given, until the stream ends (chr-server closed the tap
/// or went away), `stop` is set or `alive` turns false.
pub fn record(
    stream: Stream,
    socket: &Path,
    dir: &Path,
    fps: Option<Rate>,
    encoder: Encoder,
    stop: &AtomicBool,
    alive: impl Fn() -> bool,
) -> Result<(), Error> {
    gst_core::init()?;
    let receiver = connect(socket, stop)?;
    let reader = receiver.reader();
    log::info!("recording {} from {}", stream.name(), socket.display());
    let mut recorder = Recorder::new(stream, dir, fps, encoder);
    let ended = loop {
        if stop.load(Ordering::Acquire) {
            break "stopped";
        }
        match reader.recv_sample_timeout(POLL) {
            Some(sample) => {
                if let Err(error) = recorder.write(&sample) {
                    log::warn!(
                        "recording {}: {error}; a new segment follows",
                        stream.name()
                    );
                    recorder.close();
                }
            }
            None if reader.is_eos() => break "the stream ended",
            None if receiver.error().is_some() => break "the connection was lost",
            None if !alive() => break "chr-server is gone",
            None => {}
        }
    };
    recorder.close();
    log::info!("recording {}: {ended}", stream.name());
    Ok(())
}

fn connect(socket: &Path, stop: &AtomicBool) -> Result<Receiver, Error> {
    let deadline = Instant::now() + CONNECT;
    loop {
        match Receiver::connect_dropping(socket, RECEIVED) {
            Ok(receiver) => return Ok(receiver),
            Err(error) if Instant::now() >= deadline || stop.load(Ordering::Acquire) => {
                return Err(format!("cannot connect to {}: {error}", socket.display()).into());
            }
            Err(_) => std::thread::sleep(POLL),
        }
    }
}

/// One stream's segments.
pub struct Recorder {
    stream: Stream,
    dir: PathBuf,
    fps: Option<Rate>,
    encoder: Encoder,
    segment: Option<Segment>,
}

impl Recorder {
    pub fn new(stream: Stream, dir: &Path, fps: Option<Rate>, encoder: Encoder) -> Self {
        Self {
            stream,
            dir: dir.to_owned(),
            fps,
            encoder,
            segment: None,
        }
    }

    /// Writes one frame, into a new segment if its caps differ from the current one's.
    pub fn write(&mut self, sample: &gst::Sample) -> Result<(), Error> {
        let caps = sample.caps().ok_or("a frame without caps")?;
        let buffer = sample.buffer().ok_or("a sample without a frame")?;
        if self
            .segment
            .as_ref()
            .is_some_and(|segment| segment.caps.as_ref() != caps)
        {
            self.close();
        }
        let segment = match &mut self.segment {
            Some(segment) => segment,
            None => self.segment.insert(Segment::open(
                self.stream,
                &self.dir,
                next_index(&self.dir, self.stream),
                caps,
                self.fps,
                self.encoder,
            )?),
        };
        segment.push(buffer)
    }

    /// Ends the current segment, if any: its encoder drains into the file.
    pub fn close(&mut self) {
        if let Some(segment) = self.segment.take() {
            segment.close();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.close();
    }
}

/// The number after the highest `<stream>-NNNN.mp4` in `dir`, from 1.
fn next_index(dir: &Path, stream: Stream) -> u32 {
    let prefix = format!("{}-", stream.name());
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()?
                .strip_prefix(&prefix)?
                .strip_suffix(".mp4")?
                .parse::<u32>()
                .ok()
        })
        .max()
        .map_or(1, |last| last + 1)
}

/// The paths of segment `index` of `stream`: its video and its sidecar.
pub fn segment_paths(dir: &Path, stream: Stream, index: u32) -> (PathBuf, PathBuf) {
    let base = format!("{}-{index:04}", stream.name());
    (
        dir.join(format!("{base}.mp4")),
        dir.join(format!("{base}.jsonl")),
    )
}

struct Segment {
    stream: Stream,
    caps: gst::Caps,
    pipeline: gst::Pipeline,
    source: AppSrc,
    video: PathBuf,
    sidecar: BufWriter<File>,
    first: Option<gst::ClockTime>,
    /// The last written frame's timestamp.
    last: Option<gst::ClockTime>,
    /// Frames whose timestamp did not advance past the last one's.
    untimed: u64,
    /// Every frame's duration: the recorded rate's period.
    period: gst::ClockTime,
    /// Its pipeline reported an error: it will not drain.
    failed: bool,
    /// Keeps frames on the recording rate's cadence; `None` keeps every frame.
    cadence: Option<Cadence>,
    frames: u64,
    /// Frames the cadence left out.
    skipped: u64,
    /// Frames the encoder was too far behind for.
    dropped: u64,
}

impl Segment {
    fn open(
        stream: Stream,
        dir: &Path,
        index: u32,
        caps: &gst::CapsRef,
        fps: Option<Rate>,
        encoder: Encoder,
    ) -> Result<Self, Error> {
        let format = format_of(caps)?;
        let input = format.mode.fps;
        // Below the input's rate, the encoder is told the rate it gets.
        let recorded = fps.filter(|fps| fps.as_f64() < input.as_f64());
        let (video, sidecar) = segment_paths(dir, stream, index);
        let pipeline = gst::parse::launch(&format!(
            "appsrc name=frames format=time ! {} ! h264parse \
             ! mp4mux fragment-duration=1000 fragment-mode=first-moov-then-finalise \
             ! filesink name=file sync=false buffer-mode=unbuffered",
            encoder.pipeline()
        ))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "the segment is not a pipeline")?;
        let source = pipeline
            .by_name("frames")
            .and_then(|element| element.downcast::<AppSrc>().ok())
            .ok_or("the segment has no appsrc")?;
        // The encoder takes progressive pictures: an interlaced frame's two
        // fields stay woven in one.
        let mut encoded = caps.to_owned();
        {
            let structure = encoded
                .make_mut()
                .structure_mut(0)
                .ok_or("caps without a structure")?;
            structure.remove_fields(["interlace-mode", "field-order"]);
            if let Some(fps) = recorded {
                structure.set("framerate", wire::fraction(fps));
            }
        }
        source.set_caps(Some(&encoded));
        source.set_property("block", false);
        pipeline
            .by_name("file")
            .ok_or("the segment has no filesink")?
            .set_property(
                "location",
                video.to_str().ok_or("the recording path is not UTF-8")?,
            );
        pipeline.set_state(gst::State::Playing)?;
        let mut sidecar = BufWriter::new(File::create(&sidecar)?);
        writeln!(
            sidecar,
            "{}",
            json!({
                "stream": stream.name(),
                "segment": index,
                "format": format.name(),
                "recorded_fps": recorded.unwrap_or(input).to_string(),
                "encoder": encoder.name(),
                "caps": caps.to_string(),
                "started_unix_ms": SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |since| since.as_millis() as u64),
            })
        )?;
        log::info!(
            "recording {} into {} ({}{}, {})",
            stream.name(),
            video.display(),
            format.name(),
            recorded.map_or_else(String::new, |fps| format!(", recorded at {fps} fps")),
            encoder.name()
        );
        Ok(Self {
            stream,
            caps: caps.to_owned(),
            pipeline,
            source,
            video,
            sidecar,
            first: None,
            last: None,
            untimed: 0,
            failed: false,
            period: gst::ClockTime::from_nseconds(recorded.unwrap_or(input).period_ns()),
            cadence: recorded.map(|fps| Cadence::new(fps, input)),
            frames: 0,
            skipped: 0,
            dropped: 0,
        })
    }

    fn push(&mut self, frame: &gst::BufferRef) -> Result<(), Error> {
        if let Some(error) = self.error() {
            self.failed = true;
            return Err(format!("{}: {error}", self.video.display()).into());
        }
        if self.source.current_level_buffers() >= QUEUE {
            if self.dropped == 0 {
                log::warn!(
                    "recording {}: the encoder is behind; frames are dropped and counted",
                    self.stream.name()
                );
            }
            self.dropped += 1;
            return Ok(());
        }
        let pts = frame.pts().ok_or("a frame without a timestamp")?;
        // An MP4 cannot hold two frames at one time. With a DeckLink output the
        // core clamps capture times from before the card's clock started to 0,
        // so the first frames can share one.
        if self.last.is_some_and(|last| pts <= last) {
            self.untimed += 1;
            return Ok(());
        }
        if let Some(cadence) = &mut self.cadence
            && !cadence.keep(pts.nseconds())
        {
            self.skipped += 1;
            return Ok(());
        }
        let first = *self.first.get_or_insert(pts);
        let at = pts.saturating_sub(first);
        let mut buffer = frame.to_owned();
        {
            let buffer = buffer.make_mut();
            buffer.set_pts(at);
            buffer.set_dts(gst::ClockTime::NONE);
            buffer.set_duration(self.period);
        }
        if let Err(error) = self.source.push_buffer(buffer) {
            self.failed = true;
            return Err(error.into());
        }
        self.last = Some(pts);
        // A line per write: a killed worker's sidecar ends where its video does.
        writeln!(self.sidecar, "{}", frame_line(self.stream, frame, at))?;
        self.sidecar.flush()?;
        self.frames += 1;
        Ok(())
    }

    fn error(&self) -> Option<String> {
        let message = self
            .pipeline
            .bus()?
            .pop_filtered(&[gst::MessageType::Error])?;
        match message.view() {
            gst::MessageView::Error(error) => Some(error.error().to_string()),
            _ => None,
        }
    }

    fn close(mut self) {
        if !self.failed
            && self.source.end_of_stream().is_ok()
            && let Some(bus) = self.pipeline.bus()
        {
            let _ = bus.timed_pop_filtered(
                gst::ClockTime::from_nseconds(DRAIN.as_nanos() as u64),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            );
        }
        let _ = self.pipeline.set_state(gst::State::Null);
        if let Err(error) = self.sidecar.flush() {
            log::warn!("{}: {error}", self.video.display());
        }
        log::info!(
            "recorded {}: {} frames, {} left out for the recording rate, {} dropped, {} left \
             out as their time did not advance",
            self.video.display(),
            self.frames,
            self.skipped,
            self.dropped,
            self.untimed
        );
    }
}

/// Picks frames of a stream at `input` fps on a cadence of `fps` (below
/// `input`) from their timestamps: each kept frame is the first at or after its
/// due time, give or take half the shorter period for timestamp jitter. After a
/// gap the cadence starts again from the next frame.
struct Cadence {
    period: u64,
    tolerance: u64,
    next: Option<u64>,
}

impl Cadence {
    fn new(fps: Rate, input: Rate) -> Self {
        Self {
            period: fps.period_ns(),
            tolerance: fps.period_ns().min(input.period_ns()) / 2,
            next: None,
        }
    }

    fn keep(&mut self, pts: u64) -> bool {
        let due = match self.next {
            Some(next) if pts + self.tolerance < next => return false,
            Some(next) if next + self.period > pts => next,
            _ => pts,
        };
        self.next = Some(due + self.period);
        true
    }
}

/// A frame's sidecar line: its time in the segment and what its stamps say.
fn frame_line(stream: Stream, frame: &gst::BufferRef, at: gst::ClockTime) -> Value {
    let mut line = json!({
        "pts_ns": at.nseconds(),
        "sequence": frame.offset(),
    });
    let fields = line.as_object_mut().expect("an object");
    match stream {
        Stream::Input => {
            // The core numbered the frame (its offset is now the program
            // sequence), so its stamp is read as it is, not as a hop checks it.
            if let Some(meta) =
                gst::meta::CustomMeta::from_buffer(frame, chr_tracker::protocol::META)
                    .ok()
                    .and_then(|meta| {
                        chr_tracker::protocol::parse(meta.structure().get::<&str>("json").ok()?)
                            .ok()
                    })
            {
                fields.insert("stream_id".into(), meta.stream_id.into());
                fields.insert("frame_id".into(), meta.frame_id.into());
                fields.insert("source_ns".into(), meta.source_ns.into());
            }
            if let Some(timecode) = Ancillary::read(frame)
                .ok()
                .flatten()
                .and_then(|ancillary| ancillary.timecode)
            {
                fields.insert("timecode".into(), timecode.to_string().into());
            }
        }
        Stream::Program => {
            if let Ok(meta) = gst::meta::CustomMeta::from_buffer(frame, PROGRAM_META) {
                let structure = meta.structure();
                for name in ["cg", "fill"] {
                    if let Ok(value) = structure.get::<bool>(name) {
                        fields.insert(name.into(), value.into());
                    }
                }
                if let Ok(source) = structure.get::<u64>("source-ns") {
                    fields.insert("source_ns".into(), source.into());
                }
                if let Ok(timecode) = structure.get::<String>("timecode") {
                    fields.insert("timecode".into(), timecode.into());
                }
            }
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The timestamps kept of `count` frames at `input` fps, jittered ±1 ms.
    fn kept(fps: Rate, input: Rate, count: u64) -> Vec<u64> {
        let mut cadence = Cadence::new(fps, input);
        (0..count)
            .map(|n| input.frame_ns(n) + if n % 2 == 0 { 1_000_000 } else { 0 })
            .filter(|&pts| cadence.keep(pts))
            .collect()
    }

    #[test]
    fn a_recording_rate_keeps_frames_on_its_cadence() {
        let ms = |values: &[u64]| values.iter().map(|ns| ns / 1_000_000).collect::<Vec<_>>();
        assert_eq!(ms(&kept(Rate::fps(25), Rate::fps(50), 8)), [1, 41, 81, 121]);
        assert_eq!(ms(&kept(Rate::fps(10), Rate::fps(50), 11)), [1, 100, 201]);
        // A rate that does not divide the input's keeps as many frames on average.
        let ntsc = kept(Rate::fps(30), Rate::fps(50), 500);
        assert!((299..=301).contains(&ntsc.len()), "{}", ntsc.len());
        // After a gap in the input the cadence starts again.
        let mut cadence = Cadence::new(Rate::fps(25), Rate::fps(50));
        assert!(cadence.keep(0));
        assert!(cadence.keep(1_000_000_000));
        assert!(!cadence.keep(1_020_000_000));
        assert!(cadence.keep(1_040_000_000));
    }

    #[test]
    fn segments_number_on_from_the_highest_in_the_folder() {
        let dir = std::env::temp_dir().join(format!("chr-recording-index-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(next_index(&dir, Stream::Input), 1);
        for name in [
            "input-0001.mp4",
            "input-0007.mp4",
            "program-0009.mp4",
            "input-x.mp4",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        assert_eq!(next_index(&dir, Stream::Input), 8);
        assert_eq!(next_index(&dir, Stream::Program), 10);
        assert_eq!(
            segment_paths(&dir, Stream::Program, 10),
            (dir.join("program-0010.mp4"), dir.join("program-0010.jsonl"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
