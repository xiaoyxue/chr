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

use chr_common::methods::sdi::SdiStatus;

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
}

/// One output slot as it left the port.
pub struct SdiSlot {
    /// Output slot index, counted at the video mode's cadence.
    pub slot: u64,
    pub uyvy: Arc<Vec<u8>>,
}

#[derive(Default)]
struct Counters {
    frames_output: AtomicU64,
    repeated: AtomicU64,
    dropped: AtomicU64,
    monitor_dropped: AtomicU64,
    /// `source_frame + 1`; zero means nothing has been output yet.
    last_source_frame: AtomicU64,
}

/// The pipeline's handle for submitting frames.
#[derive(Clone)]
pub struct SdiInput {
    frames: SyncSender<SdiFrame>,
    counters: Arc<Counters>,
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
                .last_source_frame
                .load(Ordering::Relaxed)
                .checked_sub(1),
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
        let counters = Arc::new(Counters::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (frames_tx, frames_rx) = mpsc::sync_channel(OUTPUT_QUEUE);
        let (tap_tx, tap_rx) = mpsc::sync_channel(TAP_QUEUE);
        let thread = {
            let (counters, stop) = (Arc::clone(&counters), Arc::clone(&stop));
            std::thread::Builder::new()
                .name("chr-sdi-out".into())
                .spawn(move || run_output(mode, &frames_rx, &tap_tx, &counters, &stop))?
        };
        let output = Self {
            input: SdiInput {
                frames: frames_tx,
                counters: Arc::clone(&counters),
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
            .store(current.source_frame + 1, Ordering::Relaxed);
        match tap.try_send(SdiSlot {
            slot,
            uyvy: Arc::clone(&current.uyvy),
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                counters.monitor_dropped.fetch_add(1, Ordering::Relaxed);
            }
            // The program output must not depend on the monitor: keep ticking without it.
            Err(TrySendError::Disconnected(_)) => {}
        }
        slot += 1;
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
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(output.stats().status().frames_output > 5);
    }
}
