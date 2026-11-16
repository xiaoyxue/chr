//! Render worker messages, each one buffer on a worker socket
//! ([`transport`](super::transport)): a JSON object in the buffer's meta, and
//! for some messages a payload as the buffer's memory.
//!
//! | Direction | `type` | Payload | Purpose |
//! | --- | --- | --- | --- |
//! | server → worker | `hello` | – | Program, runtime ID, output mode, assets, tracked lens |
//! | worker → server | `ready` / `fatal` | – | Startup result |
//! | server → worker | `render` | input image | One program frame |
//! | server → worker | `triangle` | – | One frame of the default triangle |
//! | worker → server | `done` | UYVY frame | Completion of one request, in order |
//! | worker → server | `failed` | – | Failure of one request, in order |
//!
//! The end of the worker's input stream makes it exit.

use std::path::PathBuf;

use chr_common::scene_frame as w;
use serde::{Deserialize, Serialize};

use crate::program::spec::ProgramSpec;

pub const VERSION: u32 = 2;
/// Requests that may be outstanding at once: the server's credits. Each holds
/// one input image in the worker and, once done, one UYVY frame.
pub const CREDITS: usize = 4;

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
    TrackedScene {
        path: PathBuf,
    },
    Description {
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
            Some(ProgramSpec::TrackedScene { path }) => Self::TrackedScene { path: path.clone() },
            Some(ProgramSpec::Description { path }) => Self::Description { path: path.clone() },
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
            Self::TrackedScene { path } => Some(ProgramSpec::TrackedScene { path: path.clone() }),
            Self::Description { path } => Some(ProgramSpec::Description { path: path.clone() }),
            Self::PitchDemo { path } => Some(ProgramSpec::PitchDemo { path: path.clone() }),
        }
    }
}

/// The lens a tracked program's frames go through.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackedLens {
    /// The startup lens of the description or manifest, with each frame's warp:
    /// cameras whose `Ti` maps to the ideal (undistorted) image, such as the
    /// recorded cameras the replay stand-in returns.
    #[default]
    Program,
    /// No lens: cameras whose `Ti` maps to the real, distorted image, such as
    /// vchao's `pitch-analysis` estimates (`server/docs/tracking-worker.md`).
    Identity,
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
        /// Assets registered after startup, as the server's private verified copies;
        /// the worker imports and registers them after installing its program.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        assets: Vec<chr_common::description::Asset>,
        #[serde(default)]
        lens: TrackedLens,
    },
    /// The input image, tightly packed as `image` describes it, is the payload.
    Render {
        id: u64,
        image: w::ImageDescription,
        frame: w::FrameInput,
        scene: Box<w::SceneSnapshot>,
    },
    Triangle {
        id: u64,
        frame: u64,
        angle_rad: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Ready {
        adapter: String,
    },
    Fatal {
        message: String,
    },
    /// The UYVY frame at the output extent is the payload.
    Done {
        id: u64,
    },
    Failed {
        id: u64,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::transport::{Receiver, Sender, SharedPixels};

    #[test]
    fn messages_cross_a_socket_in_their_meta() {
        let dir = std::env::temp_dir().join(format!("chr-render-protocol-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("replies.sock");
        let sender = Sender::listen(&path).unwrap();
        let receiver = Receiver::connect(&path).unwrap();
        let replies = [
            Reply::Ready {
                adapter: "GPU".into(),
            },
            Reply::Done { id: 3 },
            Reply::Failed {
                id: 4,
                message: "x".into(),
            },
        ];
        let uyvy = SharedPixels::copy_from(&[1, 2, 3, 4]).unwrap();
        for reply in &replies {
            let payload = matches!(reply, Reply::Done { .. }).then(|| uyvy.memory());
            sender.send(reply, payload).unwrap();
        }
        for reply in &replies {
            let incoming = receiver.recv::<Reply>().unwrap().unwrap();
            assert_eq!(&incoming.message, reply);
            let expected: &[u8] = if matches!(reply, Reply::Done { .. }) {
                &[1, 2, 3, 4]
            } else {
                &[]
            };
            assert_eq!(*incoming.payload().unwrap(), *expected);
        }
        drop((sender, receiver));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn messages_are_tagged_json() {
        assert_eq!(
            serde_json::to_value(Reply::Ready {
                adapter: "GPU".into()
            })
            .unwrap(),
            serde_json::json!({"type": "ready", "adapter": "GPU"})
        );
        assert!(serde_json::from_str::<Reply>(r#"{"type":"bogus"}"#).is_err());
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
