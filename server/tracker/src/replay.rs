//! The replay stand-in's logic: the recorded camera of a calibrated clip
//! (`calibrated-video-v1`) as tracking data. `chr-tracking-sim-cli` returns it
//! as every frame's pose ([`TrackingReplay::select`]), `chr-sdi-sim
//! --send-cameras` sends it as `tracking.frame` ([`TrackingReplay::frame`]).
//! It replays; it never estimates.

use std::path::Path;

use chr_common::{methods::tracking::TrackingFrame, scene_frame as w};
use serde::Deserialize;

use crate::{
    Checks, Error,
    pose::matrix3,
    protocol::{Endpoint, FrameMeta, Pose, Selection},
};

/// The radial lens of a `calibrated-video-v1` manifest.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Radial {
    pub centre_px: [f64; 2],
    pub scale_px: f64,
    pub coefficients: [f64; 3],
    pub support: Option<[[f64; 2]; 2]>,
}

impl Radial {
    /// The same lens as tracking data carries it, validated.
    pub fn wire(&self, checks: &Checks) -> Result<w::RadialLens, Error> {
        crate::lens::wire(
            self.centre_px,
            self.scale_px,
            self.coefficients,
            self.support,
            checks,
        )
    }
}

/// The fields of a `calibrated-video-v1` manifest the replay needs.
#[derive(Deserialize)]
pub struct Manifest {
    pub ti: [[f64; 3]; 3],
    pub center: [f64; 3],
    pub radial: Radial,
    #[serde(default)]
    pub frame_warps: Vec<[[f64; 3]; 3]>,
}

/// The replay of the `calibrated-video-v1` manifest at `path`.
pub fn load(path: &Path, checks: &Checks) -> Result<TrackingReplay, Error> {
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    TrackingReplay::new(
        manifest.ti,
        manifest.center,
        manifest.radial.wire(checks)?,
        &manifest.frame_warps,
        checks,
    )
}

/// Tracking samples replayed from a calibrated clip: the reference `Ti` and `C` and
/// the clip's lens for every frame, and the clip's warp of `sequence % warps`, the
/// convention of the calibrated video replay. No warps means `W = S`.
#[derive(Clone, Debug)]
pub struct TrackingReplay {
    ti: w::Matrix3,
    camera_center: w::Vec3,
    lens: w::RadialLens,
    frame_warps: Vec<w::Matrix3>,
}

impl TrackingReplay {
    pub fn new(
        ti: [[f64; 3]; 3],
        camera_center: [f64; 3],
        lens: w::RadialLens,
        frame_warps: &[[[f64; 3]; 3]],
        checks: &Checks,
    ) -> Result<Self, Error> {
        (checks.ti)(ti)?;
        (checks.lens)(&lens)?;
        for rows in frame_warps {
            (checks.frame_warp)(*rows)?;
        }
        Ok(Self {
            ti: matrix3(ti)?,
            camera_center: camera_center
                .map(w::Finite::try_from)
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.0)?
                .try_into()
                .expect("three values"),
            lens,
            frame_warps: frame_warps
                .iter()
                .map(|rows| matrix3(*rows))
                .collect::<Result<_, _>>()?,
        })
    }

    pub fn warp_count(&self) -> usize {
        self.frame_warps.len()
    }

    /// The clip's `radial-poly-native-px-v1` lens.
    pub fn lens(&self) -> &w::RadialLens {
        &self.lens
    }

    /// The clip's warp of `index % warps`, if it has warps.
    pub fn warp(&self, index: u64) -> Option<w::Matrix3> {
        (!self.frame_warps.is_empty())
            .then(|| self.frame_warps[(index % self.frame_warps.len() as u64) as usize])
    }

    /// The reference camera's optical centre: world metres, negative Z up.
    pub fn camera_center(&self) -> [f64; 3] {
        self.camera_center.map(w::Finite::get)
    }

    /// The reference `Ti`, forward row-major ground XY to ideal image SNC.
    pub fn ti(&self) -> [f64; 9] {
        self.ti.map(w::Finite::get)
    }

    pub fn frame(&self, sequence: u64) -> TrackingFrame {
        TrackingFrame {
            sequence: sequence.into(),
            ti: self.ti,
            camera_center: self.camera_center,
            frame_warp: self.warp(sequence),
            lens: Some(self.lens.clone()),
        }
    }

    /// The stand-in worker's answer for a frame: a `fresh` pose with the
    /// reference `Ti`, the frame itself as its endpoint, selected at `now_ns`,
    /// or `unavailable`; and the clip's lens. No frame warp: the worker never
    /// sees capture sequences, so it cannot tell which of the clip's warps
    /// belongs to a frame, and a misaligned warp jitters more than none.
    pub fn select(&self, meta: &mut FrameMeta, now_ns: u64, unavailable: bool) {
        let t = self.ti();
        let pose = if unavailable {
            Pose::Unavailable {}
        } else {
            Pose::Fresh {
                t,
                left: Endpoint {
                    frame_id: meta.frame_id,
                    sequence: meta.frame_id,
                    source_ns: meta.source_ns,
                    observed_ns: now_ns,
                    t,
                },
                frame_warp: None,
            }
        };
        meta.lens = Some(self.lens.clone());
        meta.selection = Some(Selection {
            selected_ns: now_ns,
            overdue: false,
            pose,
        });
    }
}

/// `pitch-analysis`' input rules, which the stand-in follows too: one stream,
/// no `frame_id` gaps (`frame_id` is the frames received before it), no
/// selection yet and strictly increasing source times.
pub fn check_input(
    meta: &FrameMeta,
    stream_id: &str,
    frame_id: u64,
    last_source_ns: Option<u64>,
) -> Result<(), String> {
    if meta.stream_id != stream_id || meta.frame_id != frame_id {
        return Err(format!(
            "frame {} of stream {:?}: an identity gap or another stream",
            meta.frame_id, meta.stream_id
        ));
    }
    if meta.selection.is_some() || last_source_ns >= Some(meta.source_ns) {
        return Err("an input frame with a selection or an old source time".into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{protocol::PoseStatus, tests::CHECKS};

    const S: f64 = 0.001;

    pub(crate) fn lens() -> w::RadialLens {
        crate::lens::wire([96., 64.], 96., [-0.05, 0., 0.], None, &CHECKS).unwrap()
    }

    pub(crate) fn replay() -> TrackingReplay {
        TrackingReplay::new(
            [[0.02, 0., 0.], [0., 0.02, 0.], [0., 0., 1.]],
            [0., 0., -10.],
            lens(),
            &[
                [[S, 0., -1.], [0., S, -0.5], [0., 0., 1.]],
                [[S, 0., -0.9], [0., S, -0.5], [0., 0., 1.]],
            ],
            &CHECKS,
        )
        .unwrap()
    }

    #[test]
    fn replay_follows_the_clip_loop_and_rejects_singular_input() {
        let replay = replay();
        assert_eq!(replay.frame(2).frame_warp, replay.frame(0).frame_warp);
        assert_ne!(replay.frame(1).frame_warp, replay.frame(0).frame_warp);
        assert_eq!(replay.frame(5).sequence.get(), 5);
        assert_eq!(
            replay.frame(4).lens,
            Some(lens()),
            "every frame has the lens"
        );
        let unwarped = TrackingReplay::new(
            [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            [0., 0., -1.],
            lens(),
            &[],
            &CHECKS,
        )
        .unwrap();
        assert_eq!(unwarped.frame(3).frame_warp, None);
        assert!(TrackingReplay::new([[0.; 3]; 3], [0., 0., -1.], lens(), &[], &CHECKS).is_err());
        assert!(
            TrackingReplay::new(
                [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
                [0., 0., -1.],
                lens(),
                &[[[0.; 3]; 3]],
                &CHECKS
            )
            .is_err()
        );
    }

    #[test]
    fn the_stand_in_returns_the_recorded_ti_and_the_lens_but_no_warp() {
        let replay = replay();
        let mut meta = FrameMeta::request("s", 3, 100, 20_000_000);
        check_input(&meta, "s", 3, Some(99)).unwrap();
        replay.select(&mut meta, 150, false);
        let selection = meta.selection.as_ref().unwrap();
        assert_eq!(selection.pose.status(), PoseStatus::Fresh);
        assert_eq!(selection.pose.t(), Some(replay.ti()));
        assert_eq!(selection.pose.frame_warp(), None);
        assert_eq!(meta.lens.as_ref(), Some(replay.lens()));
        crate::protocol::check_selection(&meta).unwrap();
        assert!(
            check_input(&meta, "s", 3, Some(99)).is_err(),
            "already selected"
        );
        replay.select(&mut meta, 150, true);
        assert_eq!(meta.selection.unwrap().pose, Pose::Unavailable {});

        let request = FrameMeta::request("s", 3, 100, 20_000_000);
        assert!(check_input(&request, "s", 2, None).is_err(), "a gap");
        assert!(
            check_input(&request, "t", 3, None).is_err(),
            "another stream"
        );
        assert!(
            check_input(&request, "s", 3, Some(100)).is_err(),
            "an old time"
        );
    }
}
