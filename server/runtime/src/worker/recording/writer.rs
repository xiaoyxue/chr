//! The recording worker's side: it connects to a [`Tap`](super::Tap)'s socket
//! and writes what arrives as segments, one per format.
//!
//! ```text
//! unixfdsrc ! appsink ─► segment: appsrc ! videoconvert ! I420 ! openh264enc ! h264parse
//!                                 ! mpegtsmux ! filesink   <dir>/<stream>-NNNN.ts
//!                        sidecar: <dir>/<stream>-NNNN.jsonl, one line per frame
//! ```
//!
//! A segment ends when the frames' caps change (a new input format, the
//! program following it), when the stream ends and when the worker stops; the
//! next one takes the next number, also across worker restarts. MPEG-TS needs
//! no index at the end and the file is written unbuffered, so a killed worker
//! leaves every frame it encoded playable. Timestamps start at 0 in every segment; the sidecar keeps each
//! frame's own. Interlaced frames are encoded as they are, both fields woven in
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

/// The encoder: H.264 from gst-plugins-bad's openh264 (the root Nix shell has
/// no x264 or MKV/MP4 muxers), high-rate and sliced for its threads.
pub const ENCODER: &str = "openh264enc complexity=low rate-control=bitrate bitrate=40000000 \
                           max-bitrate=80000000 gop-size=50 multi-thread=8 slice-mode=n-slices \
                           num-slices=8";
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

/// Records `stream` from `socket` into `dir`, at most `fps` frames per second
/// if given, until the stream ends (chr-server closed the tap or went away),
/// `stop` is set or `alive` turns false.
pub fn record(
    stream: Stream,
    socket: &Path,
    dir: &Path,
    fps: Option<Rate>,
    stop: &AtomicBool,
    alive: impl Fn() -> bool,
) -> Result<(), Error> {
    gst_core::init()?;
    let receiver = connect(socket, stop)?;
    let reader = receiver.reader();
    log::info!("recording {} from {}", stream.name(), socket.display());
    let mut recorder = Recorder::new(stream, dir, fps);
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
    segment: Option<Segment>,
}

impl Recorder {
    pub fn new(stream: Stream, dir: &Path, fps: Option<Rate>) -> Self {
        Self {
            stream,
            dir: dir.to_owned(),
            fps,
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

/// The number after the highest `<stream>-NNNN.ts` in `dir`, from 1.
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
                .strip_suffix(".ts")?
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
        dir.join(format!("{base}.ts")),
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
    ) -> Result<Self, Error> {
        let format = format_of(caps)?;
        let input = format.mode.fps;
        // Below the input's rate, the encoder is told the rate it gets.
        let recorded = fps.filter(|fps| fps.as_f64() < input.as_f64());
        let (video, sidecar) = segment_paths(dir, stream, index);
        let pipeline = gst::parse::launch(&format!(
            "appsrc name=frames format=time ! videoconvert n-threads=4 \
             ! video/x-raw,format=I420 ! {ENCODER} ! h264parse ! mpegtsmux \
             ! filesink name=file sync=false buffer-mode=unbuffered"
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
                "caps": caps.to_string(),
                "started_unix_ms": SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |since| since.as_millis() as u64),
            })
        )?;
        log::info!(
            "recording {} into {} ({}{})",
            stream.name(),
            video.display(),
            format.name(),
            recorded.map_or_else(String::new, |fps| format!(", recorded at {fps} fps"))
        );
        Ok(Self {
            stream,
            caps: caps.to_owned(),
            pipeline,
            source,
            video,
            sidecar,
            first: None,
            failed: false,
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
        }
        if let Err(error) = self.source.push_buffer(buffer) {
            self.failed = true;
            return Err(error.into());
        }
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
            "recorded {}: {} frames, {} left out for the recording rate, {} dropped",
            self.video.display(),
            self.frames,
            self.skipped,
            self.dropped
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
            "input-0001.ts",
            "input-0007.ts",
            "program-0009.ts",
            "input-x.ts",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        assert_eq!(next_index(&dir, Stream::Input), 8);
        assert_eq!(next_index(&dir, Stream::Program), 10);
        assert_eq!(
            segment_paths(&dir, Stream::Program, 10),
            (dir.join("program-0010.ts"), dir.join("program-0010.jsonl"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
