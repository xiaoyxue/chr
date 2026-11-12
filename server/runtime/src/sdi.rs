//! Simulated SDI program output port.
//!
//! The pipeline hands rendered frames, already in the SDI pixel format, to a bounded
//! queue. An output thread sends one frame per slot at the video mode's cadence from its
//! own monotonic clock, like an SDI card: when no new frame is ready it repeats the
//! previous one, and when the queue is full the newest render is dropped. Every slot that
//! leaves the port is also offered on a tap, which is what the monitor encodes for
//! clients, so they see exactly the program output, repeats included.
//!
//! This is a simulation for development without SDI hardware; a real output adapter
//! (for example a DeckLink card) replaces the output thread.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use crate::frame_submissions::{OutputFrame, OutputPort, Provenance};
use chr_common::methods::sdi::SdiStatus;
use chr_common::scene_frame as w;

pub const PIXEL_FORMAT: &str = "UYVY 4:2:2 8-bit, BT.709 limited range";
/// Rendered frames buffered ahead of the output clock.
const OUTPUT_QUEUE: usize = 3;
/// Transmitted slots buffered ahead of the monitor encoder.
const TAP_QUEUE: usize = 2;
/// How often blocked threads check for shutdown.
pub const POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoMode {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl VideoMode {
    pub fn name(&self) -> String {
        format!("{}x{}p{}", self.width, self.height, self.fps)
    }

    pub fn frame_duration(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.fps))
    }
}

/// One rendered frame in the SDI pixel format: packed UYVY rows, `width * 2` bytes each.
pub struct SdiFrame {
    /// Render frame index (logical time).
    pub source_frame: u64,
    pub uyvy: Arc<Vec<u8>>,
    /// Keeps the separately allocated output alive through queued/current/repeated slots.
    pub program: Option<Arc<OutputFrame>>,
}

#[derive(Clone, Debug)]
pub struct ProgramAssociation {
    pub provenance: Arc<Provenance>,
    pub output_frame_id: w::Id,
    pub output_stream_id: w::Id,
}

/// One output slot as it left the port.
pub struct SdiSlot {
    /// Output slot index, counted at the video mode's cadence.
    pub slot: u64,
    pub uyvy: Arc<Vec<u8>>,
    pub program: Option<Arc<OutputFrame>>,
    pub association: Option<ProgramAssociation>,
}

#[derive(Default)]
struct Counters {
    frames_output: AtomicU64,
    repeated: AtomicU64,
    dropped: AtomicU64,
    monitor_dropped: AtomicU64,
    last_source_frame: AtomicU64,
    has_source_frame: AtomicBool,
}

/// The pipeline's handle for submitting frames.
#[derive(Clone)]
pub struct SdiInput {
    frames: SyncSender<SdiFrame>,
    counters: Arc<Counters>,
    frame_bytes: usize,
    program_enabled: bool,
}

impl OutputPort for SdiInput {
    fn try_handoff(&mut self, frame: OutputFrame) -> Result<(), OutputFrame> {
        if !self.program_enabled {
            let _ = frame.observer.failed();
            return Ok(());
        }
        let Some(pixels) = frame.resource.payload::<Arc<Vec<u8>>>() else {
            let _ = frame.observer.failed();
            return Ok(());
        };
        if pixels.len() != self.frame_bytes {
            let _ = frame.observer.failed();
            return Ok(());
        }
        let sdi = SdiFrame {
            source_frame: frame.provenance.input.frame.sequence.get(),
            uyvy: Arc::clone(pixels),
            program: Some(Arc::new(frame)),
        };
        match self.frames.try_send(sdi) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(mut sdi)) => {
                self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                Err(Arc::try_unwrap(sdi.program.take().expect("program frame"))
                    .unwrap_or_else(|_| unreachable!("unpublished frame is uniquely owned")))
            }
            Err(TrySendError::Disconnected(_)) => Ok(()),
        }
    }
}

impl SdiInput {
    /// Queues a frame without blocking the render loop. A full queue drops this frame.
    pub fn submit(&self, frame: SdiFrame) {
        match self.frames.try_send(frame) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

/// Read-only view of the output counters.
#[derive(Clone)]
pub struct SdiStats {
    mode: VideoMode,
    counters: Arc<Counters>,
}

impl SdiStats {
    /// Stats for an output that never runs, for tests of status reporting.
    #[cfg(test)]
    pub fn idle(mode: VideoMode) -> Self {
        Self {
            mode,
            counters: Arc::default(),
        }
    }

    pub fn status(&self) -> SdiStatus {
        let counters = &self.counters;
        SdiStatus {
            video_mode: self.mode.name(),
            pixel_format: PIXEL_FORMAT.to_owned(),
            frames_output: counters.frames_output.load(Ordering::Relaxed),
            repeated_frames: counters.repeated.load(Ordering::Relaxed),
            dropped_frames: counters.dropped.load(Ordering::Relaxed),
            monitor_dropped_frames: counters.monitor_dropped.load(Ordering::Relaxed),
            last_source_frame: counters
                .has_source_frame
                .load(Ordering::Acquire)
                .then(|| counters.last_source_frame.load(Ordering::Relaxed)),
        }
    }
}

pub struct SdiOutput {
    input: SdiInput,
    stats: SdiStats,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SdiOutput {
    /// Starts the output clock. The returned receiver yields every transmitted slot; when
    /// its consumer falls behind, slots are skipped for it, never for the port.
    pub fn spawn(mode: VideoMode) -> std::io::Result<(Self, Receiver<SdiSlot>)> {
        Self::spawn_inner(mode, None)
    }

    /// A new stream ID is required whenever this clock is restarted.
    pub fn spawn_program(
        mode: VideoMode,
        stream: w::Id,
    ) -> std::io::Result<(Self, Receiver<SdiSlot>)> {
        Self::spawn_inner(mode, Some(stream))
    }

    fn spawn_inner(
        mode: VideoMode,
        stream: Option<w::Id>,
    ) -> std::io::Result<(Self, Receiver<SdiSlot>)> {
        let program_enabled = stream.is_some();
        let counters = Arc::new(Counters::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (frames_tx, frames_rx) = mpsc::sync_channel(OUTPUT_QUEUE);
        let (tap_tx, tap_rx) = mpsc::sync_channel(TAP_QUEUE);
        let thread = {
            let (counters, stop) = (Arc::clone(&counters), Arc::clone(&stop));
            std::thread::Builder::new()
                .name("chr-sdi-out".into())
                .spawn(move || {
                    run_output(mode, stream.as_ref(), &frames_rx, &tap_tx, &counters, &stop)
                })?
        };
        let output = Self {
            input: SdiInput {
                frames: frames_tx,
                counters: Arc::clone(&counters),
                frame_bytes: mode.width as usize * mode.height as usize * 2,
                program_enabled,
            },
            stats: SdiStats { mode, counters },
            stop,
            thread: Some(thread),
        };
        Ok((output, tap_rx))
    }

    pub fn input(&self) -> SdiInput {
        self.input.clone()
    }

    pub fn stats(&self) -> SdiStats {
        self.stats.clone()
    }
}

impl Drop for SdiOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_output(
    mode: VideoMode,
    stream: Option<&w::Id>,
    frames: &Receiver<SdiFrame>,
    tap: &SyncSender<SdiSlot>,
    counters: &Counters,
    stop: &AtomicBool,
) {
    // The port starts once program video exists.
    let mut current = loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match frames.recv_timeout(POLL) {
            Ok(frame) => break frame,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        }
    };
    let period = mode.frame_duration();
    // Two slots of preroll let the queue absorb jitter between the two clocks.
    let mut deadline = Instant::now() + period * 2;
    let mut slot = 0u64;
    while !stop.load(Ordering::Relaxed) {
        if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        if slot > 0 {
            match frames.try_recv() {
                Ok(frame) => current = frame,
                Err(TryRecvError::Empty) => {
                    counters.repeated.fetch_add(1, Ordering::Relaxed);
                }
                Err(TryRecvError::Disconnected) => return,
            }
        }
        counters.frames_output.fetch_add(1, Ordering::Relaxed);
        counters
            .last_source_frame
            .store(current.source_frame, Ordering::Relaxed);
        counters.has_source_frame.store(true, Ordering::Release);
        let association = current.program.as_ref().zip(stream).map(|(frame, stream)| {
            let _ = frame.observer.transmitted(stream.clone(), slot.into());
            ProgramAssociation {
                provenance: Arc::clone(&frame.provenance),
                output_frame_id: frame.resource.id().clone(),
                output_stream_id: stream.clone(),
            }
        });
        match tap.try_send(SdiSlot {
            slot,
            uyvy: Arc::clone(&current.uyvy),
            program: current.program.clone(),
            association,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                counters.monitor_dropped.fetch_add(1, Ordering::Relaxed);
            }
            // The program output must not depend on the monitor: keep ticking without it.
            Err(TrySendError::Disconnected(_)) => {}
        }
        let Some(next) = slot.checked_add(1) else {
            return;
        };
        slot = next;
        deadline += period;
        let now = Instant::now();
        if now > deadline + period {
            // A stalled process cannot send late slots in a burst; real hardware would
            // have kept ticking, so the missed slots are simply gone.
            log::debug!("SDI output stalled for {:?}", now - deadline);
            deadline = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn program_handoff_is_bounded_and_never_invents_transmission() {
        use crate::{
            program::synthetic::{Synthetic, runtime_id},
            retained::SceneControl,
        };
        use serde_json::json;
        let mode = VideoMode {
            width: 16,
            height: 16,
            fps: 50,
        };
        let (mut synthetic, adapter) = Synthetic::install(mode, runtime_id().unwrap()).unwrap();
        let (control, owner) = SceneControl::spawn(adapter, 8).unwrap();
        synthetic.create_scene(&control).await.unwrap();
        let (frames, receiver) = mpsc::sync_channel(1);
        let mut port = SdiInput {
            frames,
            counters: Arc::default(),
            frame_bytes: 512,
            program_enabled: true,
        };
        let first = synthetic.inject(&control).await.unwrap().unwrap();
        synthetic
            .scheduler
            .next_ready()
            .unwrap()
            .unwrap()
            .rendered(Arc::new(vec![16u8; 512]))
            .unwrap();
        synthetic.scheduler.handoff(&mut port).unwrap();
        let second = synthetic.inject(&control).await.unwrap().unwrap();
        synthetic
            .scheduler
            .next_ready()
            .unwrap()
            .unwrap()
            .rendered(Arc::new(vec![17u8; 512]))
            .unwrap();
        synthetic.scheduler.handoff(&mut port).unwrap();
        let get = |submission| json!({"runtime_id":synthetic.configuration.runtime_id,"submission_id":submission});
        let first_result = control
            .call(w::RENDER_GET, Some(get(&first)))
            .await
            .unwrap();
        assert_eq!(first_result["frames"][0]["delivery"]["state"], "queued");
        let second_result = control
            .call(w::RENDER_GET, Some(get(&second)))
            .await
            .unwrap();
        assert_eq!(
            second_result["frames"][0]["delivery"]["reason"],
            "queue_full"
        );
        assert_eq!(port.counters.dropped.load(Ordering::Relaxed), 1);
        let frame = receiver.try_recv().unwrap();
        let weak = Arc::downgrade(&frame.uyvy);
        drop(frame);
        assert!(weak.upgrade().is_none());
        let first_result = control
            .call(w::RENDER_GET, Some(get(&first)))
            .await
            .unwrap();
        assert_eq!(
            first_result["frames"][0]["delivery"]["reason"],
            "output_failed"
        );
        // Native shutdown drains even if live transports retain endpoint clones.
        let live_connection = control.clone();
        control.shutdown().await;
        owner.await.unwrap();
        assert!(
            live_connection
                .call(
                    w::GET_CONFIGURATION,
                    Some(json!({"runtime_id":synthetic.configuration.runtime_id}))
                )
                .await
                .is_err()
        );
    }

    fn uyvy_frame(mode: VideoMode, luma: u8) -> Arc<Vec<u8>> {
        let pixels = (mode.width * mode.height) as usize;
        Arc::new([128, luma, 128, luma].repeat(pixels / 2))
    }

    #[test]
    fn output_repeats_on_underrun_and_drops_when_full() {
        let mode = VideoMode {
            width: 64,
            height: 32,
            fps: 100,
        };
        let (output, tap) = SdiOutput::spawn(mode).unwrap();
        let input = output.input();
        let stats = output.stats();
        assert_eq!(stats.status().last_source_frame, None);

        // More frames than the queue holds, faster than the output clock.
        for source_frame in 0..10 {
            input.submit(SdiFrame {
                source_frame,
                uyvy: uyvy_frame(mode, 100),
                program: None,
            });
        }
        // Then nothing: the port must keep ticking by repeating the last frame.
        std::thread::sleep(Duration::from_millis(300));
        let status = stats.status();
        assert_eq!(status.video_mode, "64x32p100");
        // The queue holds OUTPUT_QUEUE frames, plus one if the port already took the first.
        let kept = 10 - status.dropped_frames;
        assert!(
            (OUTPUT_QUEUE as u64..=OUTPUT_QUEUE as u64 + 1).contains(&kept),
            "{status:?}"
        );
        assert!(status.last_source_frame.is_some_and(|frame| frame < 10));
        assert!(status.frames_output > 10, "{status:?}");
        assert!(status.repeated_frames > 5, "{status:?}");
        // Nobody drains the tap, so the port skipped slots for it but kept going.
        assert!(status.monitor_dropped_frames > 0, "{status:?}");

        // The tap carries slots in order, starting at the first.
        let first = tap.try_recv().expect("a transmitted slot");
        assert_eq!(first.slot, 0);
        let second = tap.try_recv().expect("a second slot");
        assert_eq!(second.slot, 1);
    }

    #[test]
    fn output_keeps_ticking_without_a_monitor() {
        let mode = VideoMode {
            width: 16,
            height: 16,
            fps: 200,
        };
        let (output, tap) = SdiOutput::spawn(mode).unwrap();
        drop(tap);
        output.input().submit(SdiFrame {
            source_frame: 0,
            uyvy: uyvy_frame(mode, 16),
            program: None,
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(output.stats().status().frames_output > 5);
    }
}
