//! Tracking worker messages, each one buffer on a worker socket
//! ([`transport`](crate::worker::transport)): a JSON object in the buffer's
//! `chr-worker-message` meta, and for `track` the frame as the buffer's memory.
//!
//! | Direction | `type` | Payload | Purpose |
//! | --- | --- | --- | --- |
//! | server → worker | `hello` | – | Version, frame extent and rate, calibration file |
//! | worker → server | `ready` / `fatal` | – | Startup result |
//! | server → worker | `track` | RGBA8 frame | Frame `sequence` |
//! | worker → server | `camera` / `unavailable` | – | The answer to one `track` request |
//!
//! A frame is RGBA8 BT.709 code values at the `hello` extent, tightly packed, in
//! memfd-backed memory that the render worker may be reading too: read-only.
//! The server sends at most [`CREDITS`] unanswered frames. Answers may come in
//! any order. The end of the worker's input stream makes it exit. Matrices follow
//! `server/docs/coordinates.md`: `ti` is forward row-major ground XY to ideal
//! image SNC, `camera_center` is in world metres with negative Z up.

use std::path::PathBuf;

use chr_common::methods::tracking::TrackingFrame;
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 2;
/// Frames that may await an answer at once: the server's credits.
pub const CREDITS: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello {
        version: u32,
        /// The extent of every frame.
        width: u32,
        height: u32,
        fps: u32,
        /// The tracking reference the worker loads, for example a calibration
        /// result; the replay stand-in reads a `calibrated-video-v1` manifest.
        calibration: PathBuf,
    },
    /// The frame is the payload.
    Track { id: u64, sequence: u64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// `name` describes the tracking method, for example a replay.
    Ready {
        name: String,
    },
    Fatal {
        message: String,
    },
    /// `camera.sequence` must be the request's sequence.
    Camera {
        id: u64,
        camera: TrackingFrame,
    },
    /// The frame has no camera; chr-server skips it rather than render it with another frame's.
    Unavailable {
        id: u64,
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_tagged_json() {
        assert_eq!(
            serde_json::to_string(&Request::Track {
                id: 7,
                sequence: 42
            })
            .unwrap(),
            r#"{"type":"track","id":7,"sequence":42}"#
        );
        let reply: Reply = serde_json::from_value(serde_json::json!({
            "type": "camera", "id": 7,
            "camera": {"sequence": "42", "ti": [1, 0, 0, 0, 1, 0, 0, 0, 1],
                       "camera_center": [0, 0, -10], "frame_warp": null},
        }))
        .unwrap();
        let Reply::Camera { camera, .. } = reply else {
            panic!("{reply:?}")
        };
        assert_eq!(camera.sequence.get(), 42);
        assert!(serde_json::from_str::<Reply>(r#"{"type":"bogus"}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"type":"track","id":1}"#).is_err());
    }
}
