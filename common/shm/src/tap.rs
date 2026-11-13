//! The program tap: every output slot `chr-server` transmits, for `chr-monitor`.
//!
//! The server pushes each slot, repeats included, into a [`SLOTS`]-slot ring:
//! `index` is the output slot, the payload is the UYVY 4:2:2 frame (`width * 2`
//! bytes per row), and `meta` carries [`SlotMeta`].

use std::path::PathBuf;

use crate::Layout;

/// Slots in the program tap ring; the monitor reads the newest.
pub const SLOTS: u32 = 8;

/// Ring layout for a `width` x `height` UYVY program output.
pub fn layout(width: u32, height: u32) -> Layout {
    Layout {
        slot_count: SLOTS,
        slot_bytes: u64::from(width) * u64::from(height) * 2,
    }
}

/// `$XDG_RUNTIME_DIR/chr/program.tap`, the default tap path of both processes.
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|dir| PathBuf::from(dir).join("chr").join("program.tap"))
}

/// Per-slot metadata of the program tap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotMeta {
    /// Render frame index (logical time) of the transmitted frame.
    pub source_frame: u64,
    /// The slot carries a program frame rather than the default triangle.
    pub program: bool,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl SlotMeta {
    pub fn to_meta(self) -> [u64; 4] {
        [
            self.source_frame,
            u64::from(self.program),
            (u64::from(self.width) << 32) | u64::from(self.height),
            u64::from(self.fps),
        ]
    }

    pub fn from_meta(meta: [u64; 4]) -> Self {
        Self {
            source_frame: meta[0],
            program: meta[1] != 0,
            width: (meta[2] >> 32) as u32,
            height: meta[2] as u32,
            fps: meta[3] as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_meta_round_trips() {
        let meta = SlotMeta {
            source_frame: u64::MAX - 1,
            program: true,
            width: 1920,
            height: 1080,
            fps: 50,
        };
        assert_eq!(SlotMeta::from_meta(meta.to_meta()), meta);
        assert_eq!(layout(1920, 1080).slot_bytes, 1920 * 1080 * 2);
    }
}
