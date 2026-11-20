//! The capture bin: `unixfdsrc` connected to the capture process
//! (`chr-sdi-in`), teed into the CG branch, whose frames go to the driver for
//! the tracking and render hops, and the clean branch, whose UYVY frames at the
//! output size go to the selector. A lost capture process ends the bin's
//! stream; the core then removes the bin and connects a new one.
//!
//! ```text
//! unixfdsrc ! video/x-raw,format=RGBA ! [number] ! tee
//!   tee. ! queue leaky ! deinterlace (first field) ! appsink (CG)
//!   tee. ! queue leaky ! [deinterlace, fixed rate] ! videoconvertscale
//!        ! video/x-raw,format=UYVY,<output extent> ! appsink (clean)
//!   tee. ! queue leaky ! appsink (recording, with --record)
//! ```
//!
//! The capture's caps carry its format: any extent and rate, progressive or
//! interlaced, which may change at any time (a new input format). Every
//! captured frame carries the capture process's VchaoFrame stamp and its
//! ancillary data (timecode, captions); chr-sdi-in starts a new stream (stream
//! id, `frame_id`s from 0) for each format. Before the tee every frame gets its
//! program sequence as its offset ([`Streams`]) and its capture time as
//! running time of the core's clock, and its ancillary data goes to its slot;
//! sequences only grow across streams and capture sessions, and both branches
//! read the frame's format from their caps.
//!
//! **Interlaced input.** Tracking and rendering need a progressive picture of
//! one instant: the CG branch's `deinterlace` (`method=linear`, `fields` the
//! first field) turns every interlaced frame into one progressive frame of its
//! first field, line-interpolated, at the frame's own time and rate; it uses no
//! other field, so it holds nothing back and adds no latency. Progressive frames
//! pass through untouched (the same memfd). Its frames are allocated by the
//! shared-memory allocator, as the workers' sockets require. The clean branch
//! keeps the interlaced picture as it came (both fields, their order) for an
//! output that follows the input or is interlaced at a fixed rate; a fixed,
//! progressive output gets it deinterlaced the same way.

use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSinkCallbacks};

use super::{
    CoreConfig, Error, OutputRate, Timeline, ancillary::Ancillary, queue, select::Frame,
    select::Selector,
};
use crate::{
    sdi::{FieldOrder, Format, Scan, VideoMode},
    worker::tracking::wire,
};

/// A captured frame for the CG branch.
#[derive(Debug)]
pub struct Captured {
    pub key: Frame,
    /// Its extent and rate; progressive (the CG branch deinterlaces).
    pub format: VideoMode,
    /// Packed RGBA8 at the frame's extent in a memfd: the capture process's,
    /// or the deinterlacer's.
    pub memory: gst::Memory,
}

/// Called with a new stream's first program sequence and its format, and
/// whether the format differs from the previous stream's.
pub type OnFormat = Box<dyn Fn(u64, Format, bool) + Send + Sync>;

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
    format: Option<Format>,
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

    /// The first program sequence of the newest stream; 0 before the first.
    pub fn base(&self) -> u64 {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.recent.back().map_or(0, |(_, base)| *base)
    }

    /// The program sequence of frame `frame_id` of `stream_id` in `format`;
    /// a new stream numbers its frames on from the highest sequence so far.
    pub fn sequence(&self, stream_id: &str, frame_id: u64, format: Format) -> u64 {
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

/// The extent, frame rate and scan of video caps. Interlaced video is
/// `interlace-mode=interleaved` (or `mixed`), both fields in one frame;
/// without a `field-order` the top field is taken to come first, as in HD.
pub fn format_of(caps: &gst::CapsRef) -> Result<Format, Error> {
    let structure = caps.structure(0).ok_or("caps without a structure")?;
    let dimension =
        |name: &str| -> Result<u32, Error> { Ok(u32::try_from(structure.get::<i32>(name)?)?) };
    let scan = match structure
        .get::<&str>("interlace-mode")
        .unwrap_or("progressive")
    {
        "progressive" => Scan::Progressive,
        "interleaved" | "mixed" => Scan::Interlaced(
            match structure
                .get::<&str>("field-order")
                .unwrap_or("top-field-first")
            {
                "bottom-field-first" => FieldOrder::BottomFirst,
                _ => FieldOrder::TopFirst,
            },
        ),
        other => return Err(format!("interlace-mode {other} is not supported").into()),
    };
    Ok(Format {
        mode: VideoMode {
            width: dimension("width")?,
            height: dimension("height")?,
            fps: wire::rate_of(structure.get::<gst::Fraction>("framerate")?)?,
        },
        scan,
    })
}

/// `deinterlace` keeping each interlaced frame's first field (top or bottom,
/// from the caps' field order), line-interpolated: one progressive frame per
/// frame at its own time. Progressive frames pass through. It does not keep
/// buffer offsets, so the frame's sequence is carried over by its time.
pub fn first_field_deinterlacer() -> Result<gst::Element, Error> {
    let deinterlace = gst::ElementFactory::make("deinterlace")
        .property_from_str("mode", "auto")
        .property_from_str("method", "linear")
        .property_from_str("fields", "top")
        .build()
        .map_err(|_| {
            "GStreamer has no deinterlace element; the core needs gst-plugins-good's \
             deinterlace on GST_PLUGIN_SYSTEM_PATH_1_0 (the Nix shells provide it)"
        })?;
    let element = deinterlace.downgrade();
    let sequences = Arc::new(Mutex::new(VecDeque::<(gst::ClockTime, u64)>::new()));
    let seen = Arc::clone(&sequences);
    deinterlace
        .static_pad("sink")
        .ok_or("deinterlace has no sink pad")?
        .add_probe(
            gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
            move |_, info| {
                match &info.data {
                    Some(gst::PadProbeData::Event(event)) => {
                        if let gst::EventView::Caps(caps) = event.view()
                            && let (Some(element), Ok(format)) =
                                (element.upgrade(), format_of(caps.caps()))
                        {
                            let first = match format.scan {
                                Scan::Interlaced(FieldOrder::BottomFirst) => "bottom",
                                _ => "top",
                            };
                            element.set_property_from_str("fields", first);
                        }
                    }
                    Some(gst::PadProbeData::Buffer(buffer)) => {
                        if let Some(pts) = buffer.pts() {
                            let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
                            seen.push_back((pts, buffer.offset()));
                            if seen.len() > 16 {
                                seen.pop_front();
                            }
                        }
                    }
                    _ => {}
                }
                gst::PadProbeReturn::Ok
            },
        );
    deinterlace
        .static_pad("src")
        .ok_or("deinterlace has no src pad")?
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data else {
                return gst::PadProbeReturn::Ok;
            };
            let sequence = buffer.pts().and_then(|pts| {
                sequences
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .iter()
                    .find(|(at, _)| *at == pts)
                    .map(|(_, sequence)| *sequence)
            });
            match sequence {
                Some(sequence) => {
                    let buffer = buffer.make_mut();
                    buffer.set_offset(sequence);
                    buffer.set_offset_end(sequence + 1);
                    gst::PadProbeReturn::Ok
                }
                None => gst::PadProbeReturn::Drop,
            }
        });
    Ok(deinterlace)
}

pub struct Capture {
    bin: gst::Bin,
    parent: gst::Pipeline,
    failed: Arc<AtomicBool>,
}

/// How long one [`probe`] connection waits for the capture's caps.
const PROBE_WAIT: Duration = Duration::from_secs(2);

/// The format the capture process at `socket` sends now: connects, takes its
/// caps and disconnects. Waits, connecting again every half a second, until
/// a capture process listens there and sends a frame. A DeckLink output
/// opens in this format: the card cannot change its mode once it plays.
pub fn probe(socket: &Path) -> Result<Format, Error> {
    let path = socket
        .to_str()
        .ok_or("the capture socket path is not UTF-8")?;
    let mut waiting = false;
    loop {
        match probe_once(path) {
            Ok(Some(format)) => return Ok(format),
            Ok(None) | Err(_) if !waiting => {
                log::info!("waiting for the input's format on {path}");
                waiting = true;
            }
            _ => {}
        }
        std::thread::sleep(super::CAPTURE_RETRY);
    }
}

/// One [`probe`] connection: `None` if no caps came within [`PROBE_WAIT`].
fn probe_once(path: &str) -> Result<Option<Format>, Error> {
    let pipeline = gst::Pipeline::with_name("chr-capture-probe");
    let source = gst::ElementFactory::make("unixfdsrc")
        .property("socket-path", path)
        .build()?;
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .property("async", false)
        .build()?;
    pipeline.add_many([&source, &sink])?;
    source.link(&sink)?;
    let pad = sink
        .static_pad("sink")
        .ok_or("fakesink without a sink pad")?;
    let played = pipeline.set_state(gst::State::Playing);
    let until = Instant::now() + PROBE_WAIT;
    let caps = loop {
        if played.is_err() || Instant::now() >= until {
            break None;
        }
        if let Some(caps) = pad.current_caps() {
            break Some(caps);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let _ = pipeline.set_state(gst::State::Null);
    caps.map(|caps| format_of(&caps)).transpose()
}

impl Capture {
    /// Connects to the capture socket inside `pipeline`; fails if the capture
    /// process does not accept the connection.
    pub fn open(
        pipeline: &gst::Pipeline,
        config: &CoreConfig,
        timeline: Timeline,
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
                    .field(
                        "interlace-mode",
                        gst::List::new(["progressive", "interleaved", "mixed"]),
                    )
                    .build(),
            )
            .build()?;
        number(&captured, streams, timeline, selector.clone())?;
        let tee = gst::ElementFactory::make("tee")
            .property("allow-not-linked", true)
            .build()?;
        let cg_queue = queue(2, true)?;
        let progressive = first_field_deinterlacer()?;
        let cg = sink("cg");
        let clean_queue = queue(2, true)?;
        let convert = gst::ElementFactory::make("videoconvertscale")
            .property("n-threads", convert_threads(config))
            .build()?;
        // A fixed progressive output deinterlaces; following the input, or a
        // fixed interlaced output, keeps the frame's fields.
        let progressive_output =
            matches!(config.output_rate, OutputRate::Fixed(_, Scan::Progressive));
        // The output's extent; the rate and scan stay the frame's, unless the
        // output is progressive at a fixed rate.
        let clean_caps = gst::ElementFactory::make("capsfilter")
            .property("caps", {
                let mut caps = super::uyvy_caps(config.output.into());
                let structure = caps.make_mut().structure_mut(0).expect("one structure");
                structure.remove_field("framerate");
                if !progressive_output {
                    structure.remove_fields(["interlace-mode", "field-order"]);
                }
                caps
            })
            .build()?;
        let clean = sink("clean");
        bin.add_many([
            &source,
            &captured,
            &tee,
            &cg_queue,
            &progressive,
            cg.upcast_ref(),
            &clean_queue,
            &convert,
            &clean_caps,
            clean.upcast_ref(),
        ])?;
        gst::Element::link_many([&source, &captured, &tee])?;
        gst::Element::link_many([&tee, &cg_queue, &progressive, cg.upcast_ref()])?;
        let mut clean_branch = vec![&tee, &clean_queue];
        let fixed_deinterlace = if progressive_output {
            Some(first_field_deinterlacer()?)
        } else {
            None
        };
        if let Some(deinterlace) = &fixed_deinterlace {
            bin.add(deinterlace)?;
            clean_branch.push(deinterlace);
        }
        clean_branch.extend([&convert, &clean_caps, clean.upcast_ref()]);
        gst::Element::link_many(clean_branch)?;
        if let Some(record) = &config.record_input {
            record.branch(&bin, &tee)?;
        }

        let failed = Arc::new(AtomicBool::new(false));
        let ended = |failed: &Arc<AtomicBool>| {
            let failed = Arc::clone(failed);
            move |_: &AppSink| failed.store(true, Ordering::Release)
        };
        // Frames the deinterlacer writes go to the workers' sockets as they are.
        cg.set_callbacks(
            AppSinkCallbacks::builder()
                .propose_allocation(|_, query| {
                    query.add_allocation_param(
                        gstreamer_allocators::ShmAllocator::get().as_ref(),
                        gst::AllocationParams::default(),
                    );
                    true
                })
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
/// sequence, its PTS its capture time in the core's running time, and its
/// ancillary data goes to its slot. Frames without a valid VchaoFrame stamp
/// are dropped.
fn number(
    element: &gst::Element,
    streams: Arc<Streams>,
    timeline: Timeline,
    selector: Selector,
) -> Result<(), Error> {
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
                .and_then(|format| Ok((wire::read(buffer)?, Ancillary::read(buffer)?, format)));
            match numbered {
                Ok((meta, ancillary, format)) => {
                    let sequence = streams.sequence(&meta.stream_id, meta.frame_id, format);
                    let capture = timeline.of_system(meta.source_ns);
                    let buffer = buffer.make_mut();
                    buffer.set_offset(sequence);
                    buffer.set_offset_end(sequence + 1);
                    buffer.set_pts(capture);
                    if let Some(ancillary) = ancillary.filter(|ancillary| !ancillary.is_empty()) {
                        selector.ancillary(
                            Frame {
                                sequence,
                                capture,
                                rate: format.mode.fps,
                                scan: format.scan,
                            },
                            ancillary,
                        );
                    }
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

/// A numbered frame's sequence, capture time, rate and scan.
fn frame_key(sample: &gst::Sample) -> Result<Frame, Error> {
    let buffer = sample.buffer().ok_or("a sample without a buffer")?;
    let format = format_of(sample.caps().ok_or("a sample without caps")?)?;
    Ok(Frame {
        sequence: buffer.offset(),
        capture: buffer.pts().ok_or("a captured frame without a timestamp")?,
        rate: format.mode.fps,
        scan: format.scan,
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
        format: format_of(sample.caps().ok_or("a sample without caps")?)?.mode,
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

    fn mode(fps: u32) -> Format {
        VideoMode {
            width: 64,
            height: 36,
            fps: Rate::fps(fps),
        }
        .into()
    }

    #[test]
    fn streams_number_on_and_report_format_changes() {
        let changes = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&changes);
        let streams = Streams::new(Box::new(move |first, format, changed| {
            seen.lock().unwrap().push((first, format.mode.fps, changed));
        }));
        assert_eq!(streams.sequence("a", 0, mode(25)), 0);
        assert_eq!(streams.sequence("a", 4, mode(25)), 4);
        // A new format is a new stream, numbered on; a late frame of the old
        // stream keeps its number.
        assert_eq!(streams.sequence("b", 0, mode(50)), 5);
        assert_eq!(streams.sequence("a", 3, mode(25)), 3);
        assert_eq!(streams.base(), 5, "b is the newest stream");
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
            .into()
        );
        let i5994 = gst::Caps::builder("video/x-raw")
            .field("width", 1920)
            .field("height", 1080)
            .field("framerate", gst::Fraction::new(30000, 1001))
            .field("interlace-mode", "interleaved")
            .field("field-order", "bottom-field-first")
            .build();
        let format = format_of(&i5994).unwrap();
        assert_eq!(format.scan, Scan::Interlaced(FieldOrder::BottomFirst));
        assert_eq!(
            format.mode.fps,
            Rate::new(30000, 1001).unwrap(),
            "the frame rate"
        );
        assert_eq!(format.name(), "1920x1080i59.94");
        let variable = gst::Caps::builder("video/x-raw")
            .field("width", 1920)
            .field("height", 1080)
            .field("framerate", gst::Fraction::new(0, 1))
            .build();
        assert!(format_of(&variable).is_err());
    }
}
