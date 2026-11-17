//! The capture bin: `unixfdsrc` connected to the capture process
//! (`chr-sdi-in`), teed into the CG branch, whose frames go to the driver for
//! the tracking and render hops, and the clean branch, whose UYVY frames at the
//! output size go to the selector. A lost capture process ends the bin's
//! stream; the core then removes the bin and connects a new one.
//!
//! ```text
//! unixfdsrc ! video/x-raw,format=RGBA,<capture> ! tee
//!   tee. ! queue leaky ! appsink (CG)
//!   tee. ! queue leaky ! videoconvertscale ! video/x-raw,format=UYVY,<output> ! appsink (clean)
//! ```
//!
//! Every captured frame carries the capture process's VchaoFrame stamp
//! (`frame_id` = its offset). Within the core it is known by its program
//! sequence: the session's base plus its `frame_id`, so sequences only grow
//! across capture sessions.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSinkCallbacks};

use super::{CoreConfig, Error, queue, select::Selector, uyvy_caps};
use crate::worker::tracking::wire;

/// A captured frame for the CG branch.
#[derive(Debug)]
pub struct Captured {
    /// The program sequence.
    pub sequence: u64,
    /// Running time of its capture.
    pub capture: gst::ClockTime,
    /// Packed RGBA8 at the capture extent, in the capture process's memfd.
    pub memory: gst::Memory,
}

/// One capture session's numbering.
pub struct Session {
    base: u64,
    next: Arc<AtomicU64>,
}

impl Session {
    /// A session numbering its frames on from `next`, which it advances.
    pub fn new(next: Arc<AtomicU64>) -> Self {
        Self {
            base: next.load(Ordering::Acquire),
            next,
        }
    }

    fn sequence(&self, frame_id: u64) -> u64 {
        let sequence = self.base + frame_id;
        self.next.fetch_max(sequence + 1, Ordering::AcqRel);
        sequence
    }
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
        session: Session,
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
        let mode = config.capture;
        let captured = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .field("width", mode.width as i32)
                    .field("height", mode.height as i32)
                    .field("framerate", gst::Fraction::new(mode.fps as i32, 1))
                    .build(),
            )
            .build()?;
        let tee = gst::ElementFactory::make("tee")
            .property("allow-not-linked", true)
            .build()?;
        let cg_queue = queue(2, true)?;
        let cg = sink("cg");
        let clean_queue = queue(2, true)?;
        let convert = gst::ElementFactory::make("videoconvertscale")
            .property("n-threads", convert_threads(config))
            .build()?;
        let clean_caps = gst::ElementFactory::make("capsfilter")
            .property("caps", uyvy_caps(config.output))
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
        let session = Arc::new(session);
        let ended = |failed: &Arc<AtomicBool>| {
            let failed = Arc::clone(failed);
            move |_: &AppSink| failed.store(true, Ordering::Release)
        };
        let numbering = Arc::clone(&session);
        cg.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                    match captured_frame(&sample, &numbering) {
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
                    if let Some(buffer) = sample.buffer_owned()
                        && let Some(capture) = buffer.pts()
                        && buffer.offset() != u64::MAX
                    {
                        selector.clean(session.sequence(buffer.offset()), capture, buffer);
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

/// The CG branch's frame: its VchaoFrame stamp validated, its one memory.
fn captured_frame(sample: &gst::Sample, session: &Session) -> Result<Captured, Error> {
    let buffer = sample.buffer().ok_or("a sample without a buffer")?;
    let meta = wire::read(buffer)?;
    let capture = buffer.pts().ok_or("a captured frame without a timestamp")?;
    if buffer.n_memory() != 1 {
        return Err(format!("a captured frame in {} memories", buffer.n_memory()).into());
    }
    Ok(Captured {
        sequence: session.sequence(meta.frame_id),
        capture,
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
