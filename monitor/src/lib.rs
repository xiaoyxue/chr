//! Program monitor: turns `chr-server`'s transmitted SDI output back into H.264 for
//! viewers.
//!
//! ```text
//! chr-server program tap (shared memory, read-only here)
//!     -> tap: newest transmitted slot -> H.264
//!         -> encoder::VideoFeed -> webrtc (browsers) / srt (MPEG-TS callers)
//! ```
//!
//! The monitor is a separate process so that it can never delay or break the program
//! output: it only reads the tap, which the server writes without waiting. Encoding is
//! software (openh264, or GStreamer x264 with the `gstreamer-media` feature) on CPU-side
//! frames as a development shortcut; on hardware the input is a physical SDI loop-out.

pub mod control;
pub mod encoder;
#[cfg(all(unix, feature = "gstreamer-media"))]
pub mod gstreamer;
#[cfg(all(unix, feature = "gstreamer-media"))]
mod gstreamer_support;
#[cfg(all(unix, feature = "gstreamer-media"))]
pub mod srt;
pub mod tap;
pub mod webrtc;

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use chr_common::methods::monitor::MonitorStatus;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The program output's video mode; it must match `chr-server`'s.
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

/// Counters reported by `monitor.get_status`.
#[derive(Debug, Default)]
pub struct Counters {
    pub frames_encoded: AtomicU64,
    pub frames_skipped: AtomicU64,
    pub srt_callers: AtomicU64,
}

impl Counters {
    pub fn status(&self, viewers: usize) -> MonitorStatus {
        MonitorStatus {
            frames_encoded: self.frames_encoded.load(Ordering::Relaxed),
            frames_skipped: self.frames_skipped.load(Ordering::Relaxed),
            viewers: viewers as u64,
            srt_callers: self.srt_callers.load(Ordering::Relaxed),
        }
    }
}
