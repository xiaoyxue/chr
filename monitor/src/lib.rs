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
//! output: it only reads the tap, which the server writes without waiting. The tap
//! stays at the program's native size; the monitor scales its preview down to fit
//! [`PREVIEW_MAX`] and encodes it with NVENC (`nvh264enc`) when the GPU offers it, else
//! x264 (both with the `gstreamer-media` feature), or openh264. On hardware the input
//! is a physical SDI loop-out.

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
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
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

    /// The preview's size: this mode scaled down to fit [`PREVIEW_MAX`] with its
    /// aspect ratio, each side rounded to the nearest even number; never scaled up.
    pub fn preview(&self) -> [u32; 2] {
        let [max_width, max_height] = PREVIEW_MAX.map(u64::from);
        let (width, height) = (u64::from(self.width), u64::from(self.height));
        let even = |numerator: u64, denominator: u64| {
            // Nearest even integer to numerator / denominator, at least 2.
            ((numerator + denominator) / (2 * denominator) * 2).max(2) as u32
        };
        if width <= max_width && height <= max_height {
            [self.width, self.height]
        } else if width * max_height >= height * max_width {
            [max_width as u32, even(height * max_width, width)]
        } else {
            [even(width * max_height, height), max_height as u32]
        }
    }
}

/// Largest program output the monitor reads: DCI 4K, as `chr-server`.
pub const MAX_SIZE: [u32; 2] = [4096, 2160];

/// Largest preview the monitor encodes: H.264 NVENC takes at most 4096 pixels per
/// side, and a 1080p preview is enough to judge the program.
pub const PREVIEW_MAX: [u32; 2] = [1920, 1080];

/// The encoder and preview size a running monitor uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderInfo {
    /// `nvh264enc`, `x264enc` or `openh264`.
    pub name: &'static str,
    pub preview: [u32; 2],
}

/// Counters reported by `monitor.get_status`.
#[derive(Debug, Default)]
pub struct Counters {
    pub frames_encoded: AtomicU64,
    pub frames_skipped: AtomicU64,
    pub srt_callers: AtomicU64,
    /// Set once the encoder is chosen.
    pub encoder: OnceLock<EncoderInfo>,
}

impl Counters {
    pub fn status(&self, viewers: usize) -> MonitorStatus {
        MonitorStatus {
            frames_encoded: self.frames_encoded.load(Ordering::Relaxed),
            frames_skipped: self.frames_skipped.load(Ordering::Relaxed),
            viewers: viewers as u64,
            srt_callers: self.srt_callers.load(Ordering::Relaxed),
            encoder: self.encoder.get().map_or("", |info| info.name).to_owned(),
            preview_width: self.encoder.get().map_or(0, |info| info.preview[0]),
            preview_height: self.encoder.get().map_or(0, |info| info.preview[1]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_fits_1080p_keeps_aspect_and_never_scales_up() {
        let preview = |width, height| {
            VideoMode {
                width,
                height,
                fps: 30,
            }
            .preview()
        };
        assert_eq!(preview(3840, 2160), [1920, 1080]);
        assert_eq!(preview(1920, 1080), [1920, 1080]);
        assert_eq!(preview(1280, 720), [1280, 720]);
        assert_eq!(preview(64, 48), [64, 48]);
        // DCI 4K: 2160 * 1920 / 4096 = 1012.5, rounded to the nearest even height.
        assert_eq!(preview(4096, 2160), [1920, 1012]);
        // Taller than 16:9: height-limited. 2880 * 1080 / 2160 = 1440.
        assert_eq!(preview(2880, 2160), [1440, 1080]);
        // Portrait: 2160 * 1080 / 3840 = 607.5 -> 608.
        assert_eq!(preview(2160, 3840), [608, 1080]);
        // An extreme aspect still keeps an even, nonzero side.
        assert_eq!(preview(4096, 2), [1920, 2]);
    }
}
