//! Tracking worker messages: one JSON object per line, at most [`MAX_LINE_BYTES`].
//!
//! | Direction | `type` | Purpose |
//! | --- | --- | --- |
//! | server → worker | `hello` | Version, input ring, frame extent and rate, calibration file |
//! | worker → server | `ready` / `fatal` | Startup result |
//! | server → worker | `track` | Frame `sequence` waits in ring slot `slot` |
//! | worker → server | `camera` / `unavailable` | The answer to one `track` request |
//!
//! The ring holds RGBA8 BT.709 code values at the frame extent; each slot's
//! header index is the request `id`. The server rewrites a slot only after its
//! request was answered, or after replacing the worker. Answers may come in any
//! order. EOF on the worker's stdin makes it exit. Matrices follow
//! `server/docs/coordinates.md`: `ti` is forward row-major ground XY to ideal
//! image SNC, `camera_center` is in world metres with negative Z up.

use std::{
    io::{self, BufRead, Read, Write},
    path::PathBuf,
};

use chr_common::methods::tracking::TrackingFrame;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const VERSION: u32 = 1;
pub const MAX_LINE_BYTES: u64 = 1 << 16;
/// Frames that may await an answer at once.
pub const RING_SLOTS: u32 = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello {
        version: u32,
        /// Created by the server; RGBA8 frames of `width` x `height`.
        input_ring: PathBuf,
        width: u32,
        height: u32,
        fps: u32,
        /// The tracking reference the worker loads, for example a calibration
        /// result; the replay stand-in reads a `calibrated-video-v1` manifest.
        calibration: PathBuf,
    },
    Track {
        id: u64,
        sequence: u64,
        slot: u32,
    },
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

/// Writes `message` as one line and flushes it.
pub fn write_line(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    if line.len() as u64 >= MAX_LINE_BYTES {
        return Err(io::Error::other("tracking message exceeds the line limit"));
    }
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

/// Reads one line, or `None` at EOF.
pub fn read_line<T: DeserializeOwned>(reader: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut line = Vec::new();
    reader.take(MAX_LINE_BYTES).read_until(b'\n', &mut line)?;
    if line.is_empty() {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(io::Error::other(
            "tracking message exceeds the line limit or is truncated",
        ));
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_tagged_json_lines() {
        let mut buffer = Vec::new();
        write_line(
            &mut buffer,
            &Request::Track {
                id: 7,
                sequence: 42,
                slot: 3,
            },
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(buffer.clone()).unwrap(),
            "{\"type\":\"track\",\"id\":7,\"sequence\":42,\"slot\":3}\n"
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
        assert_eq!(
            read_line::<Request>(&mut io::Cursor::new(buffer))
                .unwrap()
                .unwrap(),
            Request::Track {
                id: 7,
                sequence: 42,
                slot: 3
            }
        );
        assert!(read_line::<Reply>(&mut io::Cursor::new(b"{\"type\":\"ready\"")).is_err());
        assert!(read_line::<Reply>(&mut io::Cursor::new(b"{\"type\":\"bogus\"}\n")).is_err());
        assert_eq!(read_line::<Reply>(&mut io::Cursor::new(b"")).unwrap(), None);
    }
}
