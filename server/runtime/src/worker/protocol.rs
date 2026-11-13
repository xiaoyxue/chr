//! Worker messages: one JSON object per line, at most [`MAX_LINE_BYTES`].
//!
//! | Direction | `type` | Purpose |
//! | --- | --- | --- |
//! | server → worker | `hello` | Program, runtime ID, output mode and ring paths |
//! | worker → server | `ready` / `fatal` | Startup result |
//! | server → worker | `render` / `triangle` | One frame into an output ring slot |
//! | worker → server | `done` / `failed` | Completion of one request, in order |
//!
//! EOF on the worker's stdin makes it exit.

use std::{
    io::{self, BufRead, Read, Write},
    path::PathBuf,
};

use chr_common::scene_frame as w;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::program::spec::ProgramSpec;

pub const VERSION: u32 = 1;
pub const MAX_LINE_BYTES: u64 = 1 << 20;
/// Slots in each ring; a slot is reused only after its request completed or
/// failed, or the worker was replaced.
pub const RING_SLOTS: u32 = 4;

/// What the worker renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerProgram {
    /// The default rotating triangle.
    Triangle,
    Synthetic,
    SoftwareVideo,
    VideoScene {
        path: PathBuf,
    },
    PitchDemo {
        path: PathBuf,
    },
}

impl WorkerProgram {
    pub fn new(spec: Option<&ProgramSpec>) -> Self {
        match spec {
            None => Self::Triangle,
            Some(ProgramSpec::Synthetic) => Self::Synthetic,
            Some(ProgramSpec::SoftwareVideo) => Self::SoftwareVideo,
            Some(ProgramSpec::VideoScene { path }) => Self::VideoScene { path: path.clone() },
            Some(ProgramSpec::PitchDemo { path }) => Self::PitchDemo { path: path.clone() },
        }
    }

    /// The program specification, or `None` for the triangle.
    pub fn spec(&self) -> Option<ProgramSpec> {
        match self {
            Self::Triangle => None,
            Self::Synthetic => Some(ProgramSpec::Synthetic),
            Self::SoftwareVideo => Some(ProgramSpec::SoftwareVideo),
            Self::VideoScene { path } => Some(ProgramSpec::VideoScene { path: path.clone() }),
            Self::PitchDemo { path } => Some(ProgramSpec::PitchDemo { path: path.clone() }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello {
        version: u32,
        program: WorkerProgram,
        runtime_id: w::Id,
        output: Output,
        /// Program input images; absent for the triangle.
        input_ring: Option<PathBuf>,
        /// Created by the worker; UYVY output frames.
        output_ring: PathBuf,
    },
    Render {
        id: u64,
        input_slot: u32,
        output_slot: u32,
        image: w::ImageDescription,
        frame: w::FrameInput,
        scene: Box<w::SceneSnapshot>,
    },
    Triangle {
        id: u64,
        output_slot: u32,
        frame: u64,
        angle_rad: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Ready { adapter: String },
    Fatal { message: String },
    Done { id: u64 },
    Failed { id: u64, message: String },
}

/// Writes `message` as one line and flushes it.
pub fn write_line(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    if line.len() as u64 >= MAX_LINE_BYTES {
        return Err(io::Error::other("worker message exceeds the line limit"));
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
            "worker message exceeds the line limit or is truncated",
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
    fn messages_round_trip_as_lines() {
        let messages = [
            Reply::Ready {
                adapter: "GPU".into(),
            },
            Reply::Done { id: 3 },
            Reply::Failed {
                id: 4,
                message: "x".into(),
            },
        ];
        let mut buffer = Vec::new();
        for message in &messages {
            write_line(&mut buffer, message).unwrap();
        }
        let text = String::from_utf8(buffer.clone()).unwrap();
        assert!(
            text.starts_with("{\"type\":\"ready\",\"adapter\":\"GPU\"}\n"),
            "{text}"
        );
        let mut reader = io::Cursor::new(buffer);
        for message in &messages {
            assert_eq!(
                read_line::<Reply>(&mut reader).unwrap().as_ref(),
                Some(message)
            );
        }
        assert_eq!(read_line::<Reply>(&mut reader).unwrap(), None);
    }

    #[test]
    fn rejects_oversized_truncated_and_unknown_lines() {
        let long = vec![b'x'; MAX_LINE_BYTES as usize + 10];
        assert!(read_line::<Reply>(&mut io::Cursor::new(long)).is_err());
        assert!(read_line::<Reply>(&mut io::Cursor::new(b"{\"type\":\"done\",\"id\":1}")).is_err());
        assert!(read_line::<Reply>(&mut io::Cursor::new(b"{\"type\":\"bogus\"}\n")).is_err());
        let program = WorkerProgram::VideoScene {
            path: "scene.json".into(),
        };
        assert_eq!(
            serde_json::to_value(&program).unwrap(),
            serde_json::json!({"kind": "video_scene", "path": "scene.json"})
        );
        assert_eq!(WorkerProgram::new(program.spec().as_ref()), program);
        assert_eq!(WorkerProgram::new(None).spec(), None);
    }
}
