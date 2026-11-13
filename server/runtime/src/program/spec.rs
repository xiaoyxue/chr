//! The program a process runs. `chr-server` and `chr-renderer-cli` install it with
//! the same code, so the same specification yields the same registrations,
//! configuration, lens replay and initial scene.
use super::{Error, pitch::Prepared, synthetic::Synthetic, video};
use crate::{retained::SceneAdapter, sdi::VideoMode};
use chr_common::scene_frame as w;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramSpec {
    /// `synthetic-program-v1`: a ground receiver and a red triangle.
    Synthetic,
    /// The small uncalibrated overlay used with a software video input.
    SoftwareVideo,
    /// A local `calibrated-video-v1` scene/lens manifest.
    VideoScene { path: PathBuf },
    /// The same manifest as `tracked-video-v1`: each frame carries its own camera.
    TrackedScene { path: PathBuf },
    /// An owner-local `pitch-demo-v1` manifest.
    PitchDemo { path: PathBuf },
}

pub struct Installed {
    pub program: Synthetic,
    pub adapter: SceneAdapter,
    /// Harness metadata of a pitch demo.
    pub controls: Option<serde_json::Value>,
}

impl ProgramSpec {
    /// Installs the program for `mode` in the `runtime` namespace.
    pub fn install(&self, mode: VideoMode, runtime: w::Id) -> Result<Installed, Error> {
        let (program, adapter, controls) = match self {
            Self::Synthetic => {
                let (program, adapter) = Synthetic::install(mode, runtime)?;
                (program, adapter, None)
            }
            Self::SoftwareVideo => {
                let (program, adapter) = Synthetic::install_video(mode, runtime)?;
                (program, adapter, None)
            }
            Self::VideoScene { path } => {
                let (program, adapter) = video::load(path, mode, runtime)?;
                (program, adapter, None)
            }
            Self::TrackedScene { path } => {
                let (program, adapter) =
                    video::load_with(path, mode, runtime, video::CameraSource::Tracked)?;
                (program, adapter, None)
            }
            Self::PitchDemo { path } => {
                let prepared = Prepared::load(path, mode, runtime)?;
                (prepared.program, prepared.adapter, Some(prepared.controls))
            }
        };
        Ok(Installed {
            program,
            adapter,
            controls,
        })
    }
}
