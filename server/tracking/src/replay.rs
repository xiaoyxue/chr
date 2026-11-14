//! Recorded cameras of a calibrated clip, replayed per frame by the stand-in
//! worker: the manifest's fixed `ti` and `center` for every frame and its warp of
//! `sequence % frame_warps` (no warps means `W = S`). This is the convention of
//! `chr-runtime`'s calibrated replay; it reproduces recorded results and does not
//! estimate a camera from the image.

use std::path::Path;

use chr_common::{methods::tracking::TrackingFrame, scene_frame as w};
use serde::Deserialize;

type Error = Box<dyn std::error::Error + Send + Sync>;

/// The fields of a `calibrated-video-v1` manifest the replay needs.
#[derive(Deserialize)]
struct Manifest {
    ti: [[f64; 3]; 3],
    center: [f64; 3],
    #[serde(default)]
    frame_warps: Vec<[[f64; 3]; 3]>,
}

#[derive(Clone, Debug)]
pub struct Replay {
    ti: w::Matrix3,
    camera_center: w::Vec3,
    frame_warps: Vec<w::Matrix3>,
}

fn finite<const N: usize>(values: [f64; N]) -> Result<[w::Finite; N], Error> {
    Ok(values
        .map(w::Finite::try_from)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.0)?
        .try_into()
        .expect("N values"))
}

fn matrix(rows: [[f64; 3]; 3]) -> Result<w::Matrix3, Error> {
    finite(std::array::from_fn(|i| rows[i / 3][i % 3]))
}

impl Replay {
    pub fn new(
        ti: [[f64; 3]; 3],
        center: [f64; 3],
        frame_warps: &[[[f64; 3]; 3]],
    ) -> Result<Self, Error> {
        Ok(Self {
            ti: matrix(ti)?,
            camera_center: finite(center)?,
            frame_warps: frame_warps
                .iter()
                .map(|rows| matrix(*rows))
                .collect::<Result<_, _>>()?,
        })
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        let manifest: Manifest = serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Self::new(manifest.ti, manifest.center, &manifest.frame_warps)
    }

    pub fn warp_count(&self) -> usize {
        self.frame_warps.len()
    }

    pub fn frame(&self, sequence: u64) -> TrackingFrame {
        TrackingFrame {
            sequence: sequence.into(),
            ti: self.ti,
            camera_center: self.camera_center,
            frame_warp: (!self.frame_warps.is_empty())
                .then(|| self.frame_warps[(sequence % self.frame_warps.len() as u64) as usize]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays_the_clip_loop_and_rejects_nonfinite_input() {
        let identity = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];
        let shifted = [[1., 0., 2.], [0., 1., 0.], [0., 0., 1.]];
        let replay = Replay::new(identity, [0., 0., -10.], &[identity, shifted]).unwrap();
        assert_eq!(replay.frame(4).frame_warp, replay.frame(0).frame_warp);
        assert_ne!(replay.frame(3).frame_warp, replay.frame(0).frame_warp);
        assert_eq!(replay.frame(9).sequence.get(), 9);
        assert_eq!(replay.frame(9).ti[0].get(), 1.);
        assert_eq!(
            Replay::new(identity, [0.; 3], &[])
                .unwrap()
                .frame(1)
                .frame_warp,
            None
        );
        assert!(Replay::new(identity, [f64::NAN, 0., 0.], &[]).is_err());
    }
}
