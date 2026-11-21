//! A tracking worker's pose as the camera of one captured frame, and that
//! camera as the frame geometry the render worker draws.
//!
//! A pose maps to a [`TrackingFrame`] for exactly the frame it was selected
//! for: its `t` is the frame's `Ti`, the station centre is its `C`, and the
//! session's lens and the pose's frame warp travel with it. An `unavailable`
//! pose is no camera; it never becomes an identity matrix or another frame's
//! camera.

use chr_common::{methods::tracking::TrackingFrame, scene_frame as w};

use crate::{
    Checks, Error,
    protocol::{Pose, PoseStatus},
    replay::TrackingReplay,
};

/// Rows of a forward row-major wire matrix.
pub fn rows3(matrix: &w::Matrix3) -> [[f64; 3]; 3] {
    std::array::from_fn(|row| std::array::from_fn(|col| matrix[row * 3 + col].get()))
}

/// The wire form of `rows`, rejecting nonfinite values.
pub fn matrix3(rows: [[f64; 3]; 3]) -> Result<w::Matrix3, Error> {
    let flat: Vec<w::Finite> = rows
        .into_iter()
        .flatten()
        .map(w::Finite::try_from)
        .collect::<Result<_, _>>()
        .map_err(|e| e.0)?;
    Ok(flat.try_into().expect("nine values"))
}

/// One frame the tracking worker returned.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    /// The frame's capture sequence.
    pub sequence: u64,
    pub pose: PoseStatus,
    /// The pose was vchao's `smoothed` selection; `pose` is its raw selection's.
    pub smoothed: bool,
    pub overdue: bool,
    /// `None` for an `unavailable` pose.
    pub camera: Option<TrackingFrame>,
}

/// The camera that `pose` gives the captured frame `sequence`: its `t` with
/// the station's optical centre (world metres, negative Z up), the session's
/// `lens` and the pose's frame warp; `None` for an `unavailable` pose. The
/// pose's matrices were checked finite when its meta was read; a frame warp
/// must be invertible and needs the session's lens. An error is a worker
/// failure.
pub fn camera(
    pose: &Pose,
    lens: Option<&w::RadialLens>,
    sequence: u64,
    camera_center: w::Vec3,
    checks: &Checks,
) -> Result<Option<TrackingFrame>, String> {
    let frame_warp = pose
        .frame_warp()
        .map(|warp| {
            if lens.is_none() {
                return Err("a frame warp without a lens".to_owned());
            }
            let rows = std::array::from_fn(|r| std::array::from_fn(|c| warp[r * 3 + c]));
            (checks.frame_warp)(rows).map_err(|error| error.to_string())?;
            matrix3(rows).map_err(|error| error.to_string())
        })
        .transpose()?;
    Ok(pose.t().map(|t| TrackingFrame {
        sequence: sequence.into(),
        // Finite: checked when the meta was read.
        ti: t.map(|v| w::Finite::try_from(v).expect("finite Ti")),
        camera_center,
        frame_warp,
        lens: lens.cloned(),
    }))
}

/// The startup half of a tracked program: its world and native extent.
#[derive(Clone, Debug)]
pub struct Tracked {
    pub world: w::Version,
    pub extent: w::Extent,
    /// A fixed reference camera and warps, replayed by `chr-renderer-cli render
    /// --frame`; a render description may omit it.
    pub replay: Option<TrackingReplay>,
}

impl Tracked {
    /// The frame geometry `render.submit` receives for one tracking sample.
    pub fn geometry(&self, frame: &TrackingFrame) -> w::FrameGeometry {
        w::FrameGeometry::TiC {
            world: self.world.clone(),
            context_id: None,
            extent: self.extent,
            ti: frame.ti,
            camera_center: frame.camera_center,
            lens: frame.lens.clone().map(Box::new),
            frame_warp: frame.frame_warp.map(Box::new),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{protocol::Endpoint, replay::tests::lens, tests::CHECKS};

    const T: [f64; 9] = [0.02, 0., 0., 0., 0.02, 0., 0., 0., 1.];

    fn fresh(frame_warp: Option<[f64; 9]>) -> Pose {
        Pose::Fresh {
            t: T,
            left: Endpoint {
                frame_id: 3,
                sequence: 3,
                source_ns: 1,
                observed_ns: 2,
                t: T,
            },
            frame_warp,
        }
    }

    #[test]
    fn a_pose_is_the_camera_of_its_own_frame_only() {
        let centre = [0., 0., -10.].map(|v| w::Finite::try_from(v).unwrap());
        let warp = [1e-3, 0., -1., 0., 1e-3, -0.5, 0., 0., 1.];
        let frame = camera(&fresh(Some(warp)), Some(&lens()), 42, centre, &CHECKS)
            .unwrap()
            .unwrap();
        assert_eq!(frame.sequence.get(), 42);
        assert_eq!(frame.ti.map(w::Finite::get), T);
        assert_eq!(frame.camera_center, centre);
        assert_eq!(frame.frame_warp.unwrap().map(w::Finite::get), warp);
        assert_eq!(frame.lens, Some(lens()));
        let pinhole = camera(&fresh(None), None, 42, centre, &CHECKS)
            .unwrap()
            .unwrap();
        assert_eq!((pinhole.lens, pinhole.frame_warp), (None, None));
        assert_eq!(
            camera(&Pose::Unavailable {}, Some(&lens()), 42, centre, &CHECKS),
            Ok(None)
        );
        assert!(camera(&fresh(Some(warp)), None, 42, centre, &CHECKS).is_err());
        assert!(camera(&fresh(Some([0.; 9])), Some(&lens()), 42, centre, &CHECKS).is_err());
    }
}
