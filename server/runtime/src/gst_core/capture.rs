//! The capture bin: `unixfdsrc` connected to the capture process
//! (`chr-sdi-in`), teed into the CG branch, whose frames go to the driver for
//! the tracking and render hops, and the clean branch, whose UYVY frames at the
//! output size go to the selector. A lost capture process ends the bin's
//! stream; the core then removes the bin and connects a new one.
//!
//! ```text
//! unixfdsrc ! video/x-raw,format=RGBA ! [number] ! tee
//!   tee. ! queue leaky ! appsink (CG)
//!   tee. ! queue leaky ! videoconvertscale ! video/x-raw,format=UYVY,<output extent> ! appsink (clean)
//! ```
//!
//! The capture's caps carry its format: any extent and rate, which may change
//! at any time (a new input format). Every captured frame carries the capture
//! process's VchaoFrame stamp; chr-sdi-in starts a new stream (stream id,
//! `frame_id`s from 0) for each format. Before the tee every frame gets its
//! program sequence as its offset ([`Streams`]), so sequences only grow across
//! streams and capture sessions, and both branches read the frame's format
//! from their caps.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSinkCallbacks};

use super::{CoreConfig, Error, queue, select::Frame, select::Selector};
use crate::{sdi::VideoMode, worker::tracking::wire};

/// A captured frame for the CG branch.
#[derive(Debug)]
pub struct Captured {
    pub key: Frame,
    /// Its extent and rate.
    pub format: VideoMode,
    /// Packed RGBA8 at the frame's extent, in the capture process's memfd.
    pub memory: gst::Memory,
}

/// Called with a new stream's first program sequence and its format, and
/// whether the format differs from the previous stream's.
pub type OnFormat = Box<dyn Fn(u64, VideoMode, bool) + Send + Sync>;

/// The capture's streams and their numbering, for every capture session of
/// the core.
pub struct Streams {
    next: AtomicU64,
    state: Mutex<StreamState>,
    on_format: OnFormat,
}

#[derive(Default)]
struct StreamState {
    /// The newest streams, by stream id: their base sequence.
    recent: VecDeque<(String, u64)>,
    format: Option<VideoMode>,
}

/// Streams remembered at once: frames of the previous stream may still be
/// numbered after the next one began.
const RECENT_STREAMS: usize = 2;

impl Streams {
    pub fn new(on_format: OnFormat) -> Self {
        Self {
            next: AtomicU64::new(0),
            state: Mutex::default(),
            on_format,
        }
    }

    /// The program sequence of frame `frame_id` of `stream_id` in `format`;
    /// a new stream numbers its frames on from the highest sequence so far.
    pub fn sequence(&self, stream_id: &str, frame_id: u64, format: VideoMode) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let base = match state.recent.iter().find(|(id, _)| id == stream_id) {
            Some((_, base)) => *base,
            None => {
                let base = self.next.load(Ordering::Acquire);
                state.recent.push_back((stream_id.to_owned(), base));
                if state.recent.len() > RECENT_STREAMS {
                    state.recent.pop_front();
                }
                let changed = state.format.is_some_and(|last| last != format);
                state.format = Some(format);
                (self.on_format)(base + frame_id, format, changed);
                base
            }
        };
        let sequence = base + frame_id;
        self.next.fetch_max(sequence + 1, Ordering::AcqRel);
        sequence
    }
}

/// The extent and rate of video caps.
pub fn format_of(caps: &gst::CapsRef) -> Result<VideoMode, Error> {
    let structure = caps.structure(0).ok_or("caps without a structure")?;
    let dimension =
        |name: &str| -> Result<u32, Error> { Ok(u32::try_from(structure.get::<i32>(name)?)?) };
    Ok(VideoMode {
        width: dimension("width")?,
        height: dimension("height")?,
        fps: wire::rate_of(structure.get::<gst::Fraction>("framerate")?)?,
    })
}

pub struct Capture {
    bin: gst::Bin,
    parent: gst::Pipeline,
    failed: Arc<AtomicBool>,
}

impl Capture {
    /// Connects to the capture socket inside `pipeline`; fails if the capture
    /// process does not accept the connection.
    pub fn open(
        pipeline: &gst::Pipeline,
        config: &CoreConfig,
        streams: Arc<Streams>,
        frames: tokio::sync::mpsc::Sender<Captured>,
        selector: Selector,
    ) -> Result<Self, Error> {
        static SESSIONS: AtomicU64 = AtomicU64::new(0);
        let bin = gst::Bin::with_name(&format!(
            "capture-{}",
            SESSIONS.fetch_add(1, Ordering::Relaxed)
        ));
        let source = gst::ElementFactory::make("unixfdsrc")
            .property(
                "socket-path",
                config
                    .capture_socket
                    .to_str()
                    .ok_or("the capture socket path is not UTF-8")?,
            )
            .build()?;
        let captured = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .field(
                        "framerate",
                        gst::FractionRange::new(
                            gst::Fraction::new(1, 1),
                            gst::Fraction::new(chr_common::rate::MAX_FPS as i32, 1),
                        ),
                    )
                    .build(),
            )
            .build()?;
        number(&captured, streams)?;
        let tee = gst::ElementFactory::make("tee")
            .property("allow-not-linked", true)
            .build()?;
        let cg_queue = queue(2, true)?;
        let cg = sink("cg");
        let clean_queue = queue(2, true)?;
        let convert = gst::ElementFactory::make("videoconvertscale")
            .property("n-threads", convert_threads(config))
            .build()?;
        // The output's extent; the rate stays the frame's.
        let clean_caps = gst::ElementFactory::make("capsfilter")
            .property("caps", {
                let mut caps = super::uyvy_caps(config.output);
                caps.make_mut()
                    .structure_mut(0)
                    .expect("one structure")
                    .remove_field("framerate");
                caps
            })
            .build()?;
        let clean = sink("clean");
        bin.add_many([
            &source,
            &captured,
            &tee,
            &cg_queue,
            cg.upcast_ref(),
            &clean_queue,
            &convert,
            &clean_caps,
            clean.upcast_ref(),
        ])?;
        gst::Element::link_many([&source, &captured, &tee])?;
        gst::Element::link_many([&tee, &cg_queue, cg.upcast_ref()])?;
        gst::Element::link_many([
            &tee,
            &clean_queue,
            &convert,
            &clean_caps,
            clean.upcast_ref(),
        ])?;

        let failed = Arc::new(AtomicBool::new(false));
        let ended = |failed: &Arc<AtomicBool>| {
            let failed = Arc::clone(failed);
            move |_: &AppSink| failed.store(true, Ordering::Release)
        };
        cg.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                    match captured_frame(&sample) {
                        Ok(frame) => {
                            if frames.try_send(frame).is_err() {
                                log::debug!("captured frame dropped: the driver is behind");
                            }
                        }
                        Err(error) => log::warn!("captured frame refused: {error}"),
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .eos(ended(&failed))
                .build(),
        );
        clean.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                    match frame_key(&sample) {
                        Ok(key) => {
                            let buffer = sample.buffer_owned().expect("a sample with a frame");
                            selector.clean(key, buffer);
                        }
                        Err(error) => log::warn!("clean frame refused: {error}"),
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .eos(ended(&failed))
                .build(),
        );

        pipeline.add(&bin)?;
        let capture = Self {
            bin,
            parent: pipeline.clone(),
            failed,
        };
        capture.bin.sync_state_with_parent()?;
        Ok(capture)
    }

    /// Whether the capture's stream ended: the capture process went away, or
    /// sent frames the core cannot take.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.bin.set_state(gst::State::Null);
        let _ = self.parent.remove(&self.bin);
    }
}

fn sink(name: &str) -> AppSink {
    AppSink::builder()
        .name(name)
        .sync(false)
        .async_(false)
        .enable_last_sample(false)
        .max_buffers(2)
        .drop(true)
        .build()
}

/// Numbers every frame before the tee: its offset becomes its program
/// sequence. Frames without a valid VchaoFrame stamp are dropped.
fn number(element: &gst::Element, streams: Arc<Streams>) -> Result<(), Error> {
    element
        .static_pad("src")
        .ok_or("capsfilter has no src pad")?
        .add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data else {
                return gst::PadProbeReturn::Ok;
            };
            let numbered = pad
                .current_caps()
                .ok_or_else(|| Error::from("a frame without caps"))
                .and_then(|caps| format_of(&caps))
                .and_then(|format| Ok((wire::read(buffer)?, format)));
            match numbered {
                Ok((meta, format)) => {
                    let sequence = streams.sequence(&meta.stream_id, meta.frame_id, format);
                    let buffer = buffer.make_mut();
                    buffer.set_offset(sequence);
                    buffer.set_offset_end(sequence + 1);
                    gst::PadProbeReturn::Ok
                }
                Err(error) => {
                    log::warn!("captured frame refused: {error}");
                    gst::PadProbeReturn::Drop
                }
            }
        })
        .ok_or("cannot number the captured frames")?;
    Ok(())
}

/// A numbered frame's sequence, capture time and rate.
fn frame_key(sample: &gst::Sample) -> Result<Frame, Error> {
    let buffer = sample.buffer().ok_or("a sample without a buffer")?;
    let caps = sample.caps().ok_or("a sample without caps")?;
    Ok(Frame {
        sequence: buffer.offset(),
        capture: buffer.pts().ok_or("a captured frame without a timestamp")?,
        rate: format_of(caps)?.fps,
    })
}

/// The CG branch's frame: its key, format and one memory.
fn captured_frame(sample: &gst::Sample) -> Result<Captured, Error> {
    let buffer = sample.buffer().ok_or("a sample without a buffer")?;
    if buffer.n_memory() != 1 {
        return Err(format!("a captured frame in {} memories", buffer.n_memory()).into());
    }
    Ok(Captured {
        key: frame_key(sample)?,
        format: format_of(sample.caps().ok_or("a sample without caps")?)?,
        memory: buffer.memory(0).expect("one memory"),
    })
}

/// Conversion threads: two per started 1080p of output, 2 to 8.
fn convert_threads(config: &CoreConfig) -> u32 {
    (config.output.width * config.output.height)
        .div_ceil(1920 * 1080)
        .saturating_mul(2)
        .clamp(2, 8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chr_common::rate::Rate;

    fn mode(fps: u32) -> VideoMode {
        VideoMode {
            width: 64,
            height: 36,
            fps: Rate::fps(fps),
        }
    }

    #[test]
    fn streams_number_on_and_report_format_changes() {
        let changes = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&changes);
        let streams = Streams::new(Box::new(move |first, format, changed| {
            seen.lock().unwrap().push((first, format.fps, changed));
        }));
        assert_eq!(streams.sequence("a", 0, mode(25)), 0);
        assert_eq!(streams.sequence("a", 4, mode(25)), 4);
        // A new format is a new stream, numbered on; a late frame of the old
        // stream keeps its number.
        assert_eq!(streams.sequence("b", 0, mode(50)), 5);
        assert_eq!(streams.sequence("a", 3, mode(25)), 3);
        assert_eq!(streams.sequence("b", 2, mode(50)), 7);
        // A reconnected capture in the same format is no format change.
        assert_eq!(streams.sequence("c", 1, mode(50)), 9);
        assert_eq!(
            *changes.lock().unwrap(),
            [
                (0, Rate::fps(25), false),
                (5, Rate::fps(50), true),
                (9, Rate::fps(50), false)
            ]
        );
    }

    #[test]
    fn formats_come_from_caps() {
        gst::init().unwrap();
        let caps = gst::Caps::builder("video/x-raw")
            .field("width", 1920)
            .field("height", 1080)
            .field("framerate", gst::Fraction::new(60000, 1001))
            .build();
        assert_eq!(
            format_of(&caps).unwrap(),
            VideoMode {
                width: 1920,
                height: 1080,
                fps: Rate::new(60000, 1001).unwrap()
            }
        );
        let variable = gst::Caps::builder("video/x-raw")
            .field("width", 1920)
            .field("height", 1080)
            .field("framerate", gst::Fraction::new(0, 1))
            .build();
        assert!(format_of(&variable).is_err());
    }
}
