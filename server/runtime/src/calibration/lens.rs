//! The camera model of a tracked frame (`server/docs/tracking-worker.md#lens`).
//!
//! The lens belongs to whoever produces `Ti`. A frame is rendered with, in order:
//! the lens its tracking data carries (the tracking worker's `lens`, or
//! `tracking.frame`'s), chr-server's `--tracking-lens-*` lens, or none: the
//! pinhole camera `S^-1 Ti A(C)`. `--tracking-lens pinhole` ignores every lens.
//! A lens in the description or manifest is never applied to tracked frames.
//! The chosen lens travels with the frame's geometry to the render worker, so a
//! frame is never rendered with another frame's lens.

use chr_common::{
    methods::tracking::{ActiveCameraModel, CameraModel, LensSource, TrackingFrame},
    scene_frame as w,
};
use chr_renderer::distortion::{RadialDistortion, SupportRect};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// The renderer's lens of a wire lens: finite, a positive scale and a nonempty
/// support, as [`RadialDistortion::new`] requires. Its monotonic radius is
/// computed there too; points beyond it are never drawn.
pub fn radial(lens: &w::RadialLens) -> Result<RadialDistortion, Error> {
    Ok(RadialDistortion::new(
        lens.centre_px.map(w::Finite::get),
        lens.scale_px.get(),
        lens.k.map(w::Finite::get),
        lens.support.map(|[min, max]| SupportRect {
            min: min.map(w::Finite::get),
            max: max.map(w::Finite::get),
        }),
    )?)
}

/// The wire form of a lens given as numbers, validated like [`radial`].
pub fn wire(
    centre_px: [f64; 2],
    scale_px: f64,
    k: [f64; 3],
    support: Option<[[f64; 2]; 2]>,
) -> Result<w::RadialLens, Error> {
    let finite = |v: f64| w::Finite::try_from(v).map_err(|e| e.0);
    let pair =
        |[x, y]: [f64; 2]| -> Result<[w::Finite; 2], &'static str> { Ok([finite(x)?, finite(y)?]) };
    let lens = w::RadialLens {
        model: w::LensModel::RadialPolyNativePxV1,
        centre_px: pair(centre_px)?,
        scale_px: finite(scale_px)?,
        k: [finite(k[0])?, finite(k[1])?, finite(k[2])?],
        support: support
            .map(|[min, max]| Ok::<_, &str>([pair(min)?, pair(max)?]))
            .transpose()?,
    };
    radial(&lens)?;
    Ok(lens)
}

/// The model a frame's geometry describes.
pub fn model_of(lens: Option<&w::RadialLens>, frame_warp: bool) -> CameraModel {
    match (lens, frame_warp) {
        (None, _) => CameraModel::Pinhole,
        (Some(_), false) => CameraModel::Radial,
        (Some(_), true) => CameraModel::RadialWarp,
    }
}

/// chr-server's lens choice for tracked frames: `--tracking-lens auto|pinhole`
/// and the optional CLI lens.
#[derive(Clone, Debug, PartialEq)]
pub struct LensSelection {
    forced_pinhole: bool,
    cli: Option<w::RadialLens>,
    /// The tracking data's lens that last disagreed with the CLI lens.
    warned: Option<w::RadialLens>,
}

impl LensSelection {
    /// `--tracking-lens auto` with an optional validated CLI lens.
    pub fn auto(cli: Option<w::RadialLens>) -> Self {
        Self {
            forced_pinhole: false,
            cli,
            warned: None,
        }
    }

    /// `--tracking-lens pinhole`.
    pub fn pinhole() -> Self {
        Self {
            forced_pinhole: true,
            cli: None,
            warned: None,
        }
    }

    pub fn cli(&self) -> Option<&w::RadialLens> {
        self.cli.as_ref()
    }

    pub fn is_forced_pinhole(&self) -> bool {
        self.forced_pinhole
    }

    /// The frame with the lens it is rendered with, and that model. The frame's
    /// lens and warp were validated at the boundary; a warp comes only with the
    /// frame's own lens.
    pub fn apply(&mut self, mut frame: TrackingFrame) -> (TrackingFrame, ActiveCameraModel) {
        let source = if self.forced_pinhole {
            frame.lens = None;
            frame.frame_warp = None;
            LensSource::ForcedPinhole
        } else if let Some(lens) = &frame.lens {
            if let Some(cli) = self.cli.as_ref().filter(|cli| *cli != lens)
                && self.warned.as_ref() != Some(lens)
            {
                log::warn!(
                    "the tracking data's lens {} overrides the --tracking-lens-* lens {}",
                    describe(lens),
                    describe(cli)
                );
                self.warned = Some(lens.clone());
            }
            LensSource::Worker
        } else if let Some(cli) = &self.cli {
            frame.lens = Some(cli.clone());
            LensSource::Cli
        } else {
            LensSource::None
        };
        let model = model_of(frame.lens.as_ref(), frame.frame_warp.is_some());
        (frame, ActiveCameraModel { model, source })
    }
}

/// A lens for a log line.
pub fn describe(lens: &w::RadialLens) -> String {
    let f = |v: &[w::Finite]| v.iter().map(|v| v.get()).collect::<Vec<_>>();
    format!(
        "radial(k {:?}, centre {:?}, scale {}, support {:?})",
        f(&lens.k),
        f(&lens.centre_px),
        lens.scale_px.get(),
        lens.support
            .map(|[min, max]| [min.map(w::Finite::get), max.map(w::Finite::get)])
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lens(k1: f64) -> w::RadialLens {
        wire([960.0, 540.0], 960.0, [k1, 0.0, 0.0], None).unwrap()
    }

    fn frame(lens: Option<w::RadialLens>, warp: bool) -> TrackingFrame {
        let m = |v: [f64; 9]| v.map(|v| w::Finite::try_from(v).unwrap());
        TrackingFrame {
            sequence: 7_u64.into(),
            ti: m([0.1, 0., 0., 0., 0.1, 0., 0., 0., 1.]),
            camera_center: [0., 0., -10.].map(|v| w::Finite::try_from(v).unwrap()),
            frame_warp: warp.then(|| m([1e-3, 0., -1., 0., 1e-3, -0.5, 0., 0., 1.])),
            lens,
        }
    }

    #[test]
    fn worker_data_then_cli_then_pinhole() {
        use CameraModel::{Pinhole, Radial, RadialWarp};
        use LensSource::{Cli, ForcedPinhole, Worker};
        let (worker, cli) = (lens(-0.05), lens(-0.02));
        let cases = [
            // (selection, frame lens, frame warp) -> (lens rendered, model, source)
            (
                LensSelection::auto(None),
                None,
                false,
                None,
                Pinhole,
                LensSource::None,
            ),
            (
                LensSelection::auto(Some(cli.clone())),
                None,
                false,
                Some(&cli),
                Radial,
                Cli,
            ),
            (
                LensSelection::auto(None),
                Some(&worker),
                false,
                Some(&worker),
                Radial,
                Worker,
            ),
            (
                LensSelection::auto(None),
                Some(&worker),
                true,
                Some(&worker),
                RadialWarp,
                Worker,
            ),
            (
                LensSelection::auto(Some(cli.clone())),
                Some(&worker),
                true,
                Some(&worker),
                RadialWarp,
                Worker,
            ),
            (
                LensSelection::pinhole(),
                Some(&worker),
                true,
                None,
                Pinhole,
                ForcedPinhole,
            ),
            (
                LensSelection::pinhole(),
                None,
                false,
                None,
                Pinhole,
                ForcedPinhole,
            ),
        ];
        for (mut selection, given, warp, rendered, model, source) in cases {
            let (out, active) = selection.apply(frame(given.cloned(), warp));
            assert_eq!(out.lens.as_ref(), rendered, "{selection:?} {given:?}");
            assert_eq!(out.frame_warp.is_some(), model == RadialWarp);
            assert_eq!(active, ActiveCameraModel { model, source });
            assert_eq!(out.ti, frame(None, false).ti, "Ti is never changed");
        }
    }

    #[test]
    fn a_disagreeing_worker_lens_is_remembered_once() {
        let mut selection = LensSelection::auto(Some(lens(-0.02)));
        for _ in 0..3 {
            selection.apply(frame(Some(lens(-0.05)), false));
        }
        assert_eq!(selection.warned, Some(lens(-0.05)));
        selection.apply(frame(Some(lens(-0.02)), false));
        assert_eq!(
            selection.warned,
            Some(lens(-0.05)),
            "an agreeing lens is no news"
        );
    }

    #[test]
    fn invalid_lenses_are_rejected() {
        assert!(wire([960.0, 540.0], 0.0, [0.0; 3], None).is_err());
        assert!(wire([960.0, 540.0], -1.0, [0.0; 3], None).is_err());
        assert!(wire([f64::NAN, 540.0], 960.0, [0.0; 3], None).is_err());
        assert!(wire([960.0, 540.0], 960.0, [f64::INFINITY, 0.0, 0.0], None).is_err());
        assert!(
            wire(
                [960.0, 540.0],
                960.0,
                [0.0; 3],
                Some([[10.0, 0.0], [5.0, 9.0]])
            )
            .is_err()
        );
        let json = serde_json::json!({"model": "radial-poly-native-px-v1", "centre_px": [1, 2],
            "scale_px": -3, "k": [0, 0, 0]});
        let parsed: w::RadialLens = serde_json::from_value(json).unwrap();
        assert!(
            radial(&parsed).is_err(),
            "a negative scale fails validation"
        );
        let unknown = serde_json::json!({"model": "opencv", "centre_px": [1, 2],
            "scale_px": 3, "k": [0, 0, 0]});
        assert!(serde_json::from_value::<w::RadialLens>(unknown).is_err());
    }
}
