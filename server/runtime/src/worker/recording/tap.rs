//! chr-server's side of one recorded stream: a leaky branch of a core `tee`
//! whose frames go out on the stream's own unixfd socket to the recording
//! worker.
//!
//! ```text
//! tee ! queue leaky ! appsink ─► [memfd] ─► appsrc ! unixfdsink   (own pipeline)
//! ```
//!
//! A frame already in a memfd (every captured frame, every rendered one) goes
//! as it is; one in other memory (the clean feed, input-loss fills) is copied
//! once into a recycled memfd, since `unixfdsink` passes file descriptors only.
//! Frames keep their timestamps, offsets (the program sequence) and custom
//! metas: the VchaoFrame stamp and the capture's ancillary data on the input,
//! the program meta on the program. Nothing is sent while no worker is
//! connected, and a full send queue drops the frame, so the tap can never hold
//! up its `tee`.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use gstreamer::{self as gst, prelude::*};
use gstreamer_allocators::FdMemory;
use gstreamer_app::{AppSink, AppSinkCallbacks};

use super::Stream;
use crate::worker::transport::{Error, Sender, SharedPixels};

/// Frames waiting for the worker before the tap drops.
const QUEUE: u64 = 4;
/// How long closing a tap waits for its last frames to be sent.
const CLOSE: Duration = Duration::from_secs(1);

pub struct Tap {
    stream: Stream,
    path: PathBuf,
    sender: Mutex<Option<Sender>>,
    caps: Mutex<Option<gst::Caps>>,
    sent: AtomicU64,
    dropped: AtomicU64,
}

/// What a tap sent since it opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub sent: u64,
    /// Frames a worker was connected for but that could not be sent: its queue
    /// was full.
    pub dropped: u64,
}

impl Counters {
    pub fn report(&self) -> String {
        format!("{} frames sent, {} dropped", self.sent, self.dropped)
    }
}

impl Tap {
    /// Listens on `path` for the recording worker.
    pub fn listen(stream: Stream, path: &Path) -> Result<Self, Error> {
        Ok(Self {
            stream,
            path: path.to_owned(),
            sender: Mutex::new(Some(Sender::listen_live(path, None, QUEUE)?)),
            caps: Mutex::new(None),
            sent: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn counters(&self) -> Counters {
        Counters {
            sent: self.sent.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }

    /// Adds `tee ! queue leaky ! appsink` to `bin`, feeding this tap.
    pub fn branch(self: &Arc<Self>, bin: &gst::Bin, tee: &gst::Element) -> Result<(), Error> {
        let queue = gst::ElementFactory::make("queue")
            .name(format!("record-{}-queue", self.stream.name()))
            .property("max-size-buffers", 2_u32)
            .property("max-size-bytes", 0_u32)
            .property("max-size-time", 0_u64)
            .property_from_str("leaky", "downstream")
            .build()?;
        let sink = AppSink::builder()
            .name(format!("record-{}", self.stream.name()))
            .sync(false)
            .async_(false)
            .enable_last_sample(false)
            .max_buffers(2)
            .drop(true)
            .build();
        bin.add_many([&queue, sink.upcast_ref()])?;
        gst::Element::link_many([tee, &queue, sink.upcast_ref()])?;
        let tap = Arc::clone(self);
        sink.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                    tap.offer(&sample);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        Ok(())
    }

    /// Sends one frame if a worker is connected; drops it if the worker is behind.
    fn offer(&self, sample: &gst::Sample) {
        let sender = self.sender.lock().unwrap_or_else(PoisonError::into_inner);
        let (Some(sender), Some(buffer), Some(caps)) =
            (sender.as_ref(), sample.buffer(), sample.caps())
        else {
            return;
        };
        if sender.clients() == 0 {
            return;
        }
        {
            let mut last = self.caps.lock().unwrap_or_else(PoisonError::into_inner);
            if last.as_deref() != Some(caps) {
                let caps = caps.to_owned();
                sender.set_caps(&caps);
                *last = Some(caps);
            }
        }
        match shareable(buffer).and_then(|buffer| sender.push(buffer)) {
            Ok(()) => {
                self.sent.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                if self.dropped.fetch_add(1, Ordering::Relaxed) == 0 {
                    log::warn!(
                        "recording {}: frame {} dropped ({error}); further drops are counted",
                        self.stream.name(),
                        buffer.offset()
                    );
                }
            }
        }
    }

    /// Ends the stream (the worker finishes its segment and exits) and stops listening.
    pub fn close(&self) {
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(sender) = sender {
            sender.finish(CLOSE);
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// `buffer` with its pixels in one memfd: itself, or a copy with its
/// timestamps, offsets, flags and metas.
fn shareable(buffer: &gst::BufferRef) -> Result<gst::Buffer, Error> {
    if buffer.n_memory() == 1
        && buffer
            .peek_memory(0)
            .downcast_memory_ref::<FdMemory>()
            .is_some()
    {
        return Ok(buffer.to_owned());
    }
    let (pixels, copied) =
        SharedPixels::filled(buffer.size(), |memory| buffer.copy_to_slice(0, memory))?;
    copied.map_err(|_| "cannot copy the frame")?;
    let mut copy = gst::Buffer::new();
    {
        let writable = copy.get_mut().expect("new buffer is not shared");
        writable.append_memory(pixels.memory());
        buffer.copy_into(
            writable,
            gst::BufferCopyFlags::FLAGS
                | gst::BufferCopyFlags::TIMESTAMPS
                | gst::BufferCopyFlags::META,
            ..,
        )?;
    }
    Ok(copy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_outside_a_memfd_are_copied_with_their_stamps() {
        crate::gst_core::init().unwrap();
        let mut buffer = gst::Buffer::from_mut_slice(vec![7_u8; 64]);
        {
            let writable = buffer.get_mut().unwrap();
            writable.set_pts(gst::ClockTime::from_mseconds(40));
            writable.set_offset(12);
            gst::meta::CustomMeta::add(writable, crate::gst_core::PROGRAM_META)
                .unwrap()
                .mut_structure()
                .set("sequence", 12_u64);
        }
        let copy = shareable(&buffer).unwrap();
        assert!(
            copy.peek_memory(0)
                .downcast_memory_ref::<FdMemory>()
                .is_some()
        );
        assert_eq!(copy.map_readable().unwrap().as_slice(), &[7_u8; 64]);
        assert_eq!(copy.pts(), Some(gst::ClockTime::from_mseconds(40)));
        assert_eq!(copy.offset(), 12);
        let meta =
            gst::meta::CustomMeta::from_buffer(&copy, crate::gst_core::PROGRAM_META).unwrap();
        assert_eq!(meta.structure().get::<u64>("sequence").unwrap(), 12);
        // A memfd frame goes as it is.
        let again = shareable(&copy).unwrap();
        assert_eq!(
            again.peek_memory(0).as_ptr(),
            copy.peek_memory(0).as_ptr(),
            "the same memory"
        );
    }
}
