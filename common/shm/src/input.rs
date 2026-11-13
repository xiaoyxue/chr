//! The capture input ring: frames a capture process (for example `chr-tracking-sim`)
//! hands to `chr-server`, standing in for an SDI input card.
//!
//! The capture process pushes each frame into a [`SLOTS`]-slot ring: `index` is the
//! capture sequence, which also identifies the frame's tracking sample; the payload
//! is RGBA8 with BT.709 code values (`width * 4` bytes per row, straight alpha);
//! `meta` carries [`SlotMeta`]. The server reads every slot in write order.

use std::path::PathBuf;

use crate::Layout;

/// Slots in the input ring: a few frame periods of slack for the reader.
pub const SLOTS: u32 = 8;

/// Ring layout for `width` x `height` RGBA8 frames.
pub fn layout(width: u32, height: u32) -> Layout {
    Layout {
        slot_count: SLOTS,
        slot_bytes: u64::from(width) * u64::from(height) * 4,
    }
}

/// `$XDG_RUNTIME_DIR/chr/input.ring`, the default input ring path of both processes.
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join("chr").join("input.ring"))
}

/// Per-slot metadata of the input ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotMeta {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl SlotMeta {
    pub fn to_meta(self) -> [u64; 4] {
        [
            0,
            0,
            (u64::from(self.width) << 32) | u64::from(self.height),
            u64::from(self.fps),
        ]
    }

    pub fn from_meta(meta: [u64; 4]) -> Self {
        Self {
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
            width: 1920,
            height: 1080,
            fps: 30,
        };
        assert_eq!(SlotMeta::from_meta(meta.to_meta()), meta);
        assert_eq!(layout(1920, 1080).slot_bytes, 1920 * 1080 * 4);
    }
}
