//! `radial-poly-native-px-v1`: a forward radial polynomial in native pixels with an
//! explicit centre and normalisation scale, followed by an optional per-frame homography.
//!
//! ```text
//! a      ideal native pixel from the pinhole camera (S^-1 Ti A(C))
//! rho^2  = |(a - centre) / scale|^2
//! r      = centre + (a - centre) * (1 + k1 rho^2 + k2 rho^4 + k3 rho^6)
//! n      = pi(W [r, 1])        frame SNC; without a frame warp, W = S (pixels -> SNC)
//! u, v   = (s n + (W, H)) / 2, s = sqrt(W H)
//! ```
//!
//! The scale is a pixel length chosen by the calibration, not a focal length, so
//! focal-normalised coefficients from other lens models are not interchangeable.
//! Native pixel centres are integers with no half-pixel offset, as everywhere in chr.

use thiserror::Error;

use crate::{
    camera::{Camera, CameraError},
    coordinates::ImageExtent,
};

/// Largest normalised ideal radius considered when searching the monotonic branch.
const RHO_SEARCH: f64 = 16.0;

#[derive(Debug, Error, PartialEq)]
pub enum DistortionError {
    #[error("distortion parameters must be finite with a positive scale")]
    InvalidParameters,
    #[error("frame warp must be a finite invertible homography")]
    InvalidFrameWarp,
    #[error("lens extent does not match the camera")]
    ExtentMismatch,
    #[error("point has no finite projection through the frame warp")]
    Undefined,
    #[error("point lies outside the invertible radial domain")]
    NotInvertible,
    #[error(transparent)]
    Camera(#[from] CameraError),
}

/// Validated rectangle in ideal native pixels; outside it, results are extrapolation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SupportRect {
    pub min: [f64; 2],
    pub max: [f64; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadialDistortion {
    centre: [f64; 2],
    scale: f64,
    k: [f64; 3],
    support: Option<SupportRect>,
    /// Ideal normalised radius where d(rho_r)/d(rho_a) first reaches zero.
    ideal_limit: f64,
}

impl RadialDistortion {
    pub fn new(
        centre_px: [f64; 2],
        scale_px: f64,
        k: [f64; 3],
        support: Option<SupportRect>,
    ) -> Result<Self, DistortionError> {
        let finite = centre_px
            .into_iter()
            .chain(k)
            .chain([scale_px])
            .all(f64::is_finite);
        let support_valid = support.is_none_or(|s| {
            s.min.into_iter().chain(s.max).all(f64::is_finite)
                && s.min[0] < s.max[0]
                && s.min[1] < s.max[1]
        });
        if !finite || scale_px <= 0.0 || !support_valid {
            return Err(DistortionError::InvalidParameters);
        }
        let mut radial = Self {
            centre: centre_px,
            scale: scale_px,
            k,
            support,
            ideal_limit: RHO_SEARCH,
        };
        radial.ideal_limit = radial.monotonic_limit();
        Ok(radial)
    }

    pub fn centre(&self) -> [f64; 2] {
        self.centre
    }
    pub fn scale(&self) -> f64 {
        self.scale
    }
    pub fn coefficients(&self) -> [f64; 3] {
        self.k
    }
    pub fn support(&self) -> Option<SupportRect> {
        self.support
    }

    /// Normalised ideal radius bounding the monotonic branch that starts at the centre.
    pub fn ideal_radius_limit(&self) -> f64 {
        self.ideal_limit
    }

    /// Normalised distorted radius reachable on the monotonic branch.
    pub fn distorted_radius_limit(&self) -> f64 {
        self.radius(self.ideal_limit)
    }

    fn radius(&self, rho: f64) -> f64 {
        let r2 = rho * rho;
        rho * (1.0 + r2 * (self.k[0] + r2 * (self.k[1] + r2 * self.k[2])))
    }

    fn slope(&self, rho: f64) -> f64 {
        let r2 = rho * rho;
        1.0 + r2 * (3.0 * self.k[0] + r2 * (5.0 * self.k[1] + r2 * 7.0 * self.k[2]))
    }

    fn monotonic_limit(&self) -> f64 {
        let steps = 16_000;
        let step = RHO_SEARCH / f64::from(steps);
        for i in 1..=steps {
            let rho = f64::from(i) * step;
            if self.slope(rho) <= 0.0 {
                let (mut lo, mut hi) = (rho - step, rho);
                for _ in 0..60 {
                    let mid = 0.5 * (lo + hi);
                    if self.slope(mid) > 0.0 {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                return lo;
            }
        }
        RHO_SEARCH
    }

    pub fn in_support(&self, ideal: [f64; 2]) -> bool {
        self.support.is_none_or(|s| {
            (s.min[0]..=s.max[0]).contains(&ideal[0]) && (s.min[1]..=s.max[1]).contains(&ideal[1])
        })
    }

    /// Ideal -> distorted reference pixel. Points beyond the monotonic branch fold back
    /// and could not be rendered by inverse warping, so they are rejected.
    pub fn distort(&self, ideal: [f64; 2]) -> Result<[f64; 2], DistortionError> {
        let d = [ideal[0] - self.centre[0], ideal[1] - self.centre[1]];
        let rho = d[0].hypot(d[1]) / self.scale;
        if !rho.is_finite() || rho >= self.ideal_limit {
            return Err(DistortionError::NotInvertible);
        }
        let r2 = rho * rho;
        let factor = 1.0 + r2 * (self.k[0] + r2 * (self.k[1] + r2 * self.k[2]));
        Ok([
            self.centre[0] + d[0] * factor,
            self.centre[1] + d[1] * factor,
        ])
    }

    /// Distorted reference pixel -> ideal pixel on the monotonic branch.
    pub fn undistort(&self, distorted: [f64; 2]) -> Result<[f64; 2], DistortionError> {
        let d = [distorted[0] - self.centre[0], distorted[1] - self.centre[1]];
        let target = d[0].hypot(d[1]) / self.scale;
        if !target.is_finite() || target >= self.distorted_radius_limit() {
            return Err(DistortionError::NotInvertible);
        }
        if target == 0.0 {
            return Ok(distorted);
        }
        // Newton from the distorted radius; the slope stays positive on this branch.
        let mut rho = target.min(self.ideal_limit * 0.999);
        for _ in 0..32 {
            let step = (self.radius(rho) - target) / self.slope(rho);
            rho = (rho - step).clamp(0.0, self.ideal_limit);
            if step.abs() <= 1e-14 * rho.max(1.0) {
                break;
            }
        }
        if (self.radius(rho) - target).abs() > 1e-9 * target.max(1.0) || rho >= self.ideal_limit {
            return Err(DistortionError::NotInvertible);
        }
        let scale = rho / target;
        Ok([self.centre[0] + d[0] * scale, self.centre[1] + d[1] * scale])
    }
}

/// Row-major homography from reference native pixels to frame SNC (column vectors).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameWarp {
    forward: [[f64; 3]; 3],
    inverse: [[f64; 3]; 3],
}

impl FrameWarp {
    pub fn new(rows: [[f64; 3]; 3]) -> Result<Self, DistortionError> {
        let scale = rows.iter().flatten().map(|v| v.abs()).fold(0.0, f64::max);
        if !rows.iter().flatten().all(|v| v.is_finite()) || scale == 0.0 {
            return Err(DistortionError::InvalidFrameWarp);
        }
        let m = rows.map(|r| r.map(|v| v / scale));
        let cofactor = |r: usize, c: usize| {
            let (r0, r1) = ((r + 1) % 3, (r + 2) % 3);
            let (c0, c1) = ((c + 1) % 3, (c + 2) % 3);
            m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
        };
        let det = (0..3).map(|c| m[0][c] * cofactor(0, c)).sum::<f64>();
        if det.abs() < 1e-12 {
            return Err(DistortionError::InvalidFrameWarp);
        }
        // Adjugate / det; homographies are defined up to scale, so the matrix scale is moot.
        let inverse = std::array::from_fn(|r| std::array::from_fn(|c| cofactor(c, r) / det));
        Ok(Self {
            forward: rows,
            inverse,
        })
    }

    pub fn rows(&self) -> [[f64; 3]; 3] {
        self.forward
    }
}

fn apply(m: [[f64; 3]; 3], p: [f64; 2]) -> Result<[f64; 2], DistortionError> {
    let h = m.map(|r| r[0] * p[0] + r[1] * p[1] + r[2]);
    let scale = h.iter().map(|v| v.abs()).fold(0.0, f64::max);
    if !h.iter().all(|v| v.is_finite()) || h[2].abs() <= 1e-12 * scale {
        return Err(DistortionError::Undefined);
    }
    Ok([h[0] / h[2], h[1] / h[2]])
}

/// Result of projecting a world point through the pinhole camera and the lens model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DistortedProjection {
    /// Distorted native pixel of the frame.
    pub native: [f64; 2],
    /// Ideal native pixel before distortion.
    pub ideal: [f64; 2],
    /// Whether the ideal pixel lies in the validated support (always true without one).
    pub inside_support: bool,
}

/// The two supported native image models. `Pinhole` is the confirmed S^-1 Ti A(C)
/// contract; `Distorted` adds `radial-poly-native-px-v1` (and an optional frame warp)
/// after that same ideal projection.
#[derive(Clone, Debug, PartialEq)]
pub enum CameraModel {
    Pinhole,
    Distorted(Box<LensDistortion>),
}

impl CameraModel {
    /// Native pixel of a world point under this model; pinhole results are their own ideal.
    pub fn project(
        &self,
        camera: &Camera,
        world: [f64; 3],
    ) -> Result<DistortedProjection, DistortionError> {
        match self {
            Self::Pinhole => {
                let ideal = camera.project(world)?.0;
                Ok(DistortedProjection {
                    native: ideal,
                    ideal,
                    inside_support: true,
                })
            }
            Self::Distorted(lens) => lens.project(camera, world),
        }
    }
}

/// Lens mapping for one frame: radial model plus an optional frame warp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LensDistortion {
    extent: ImageExtent,
    radial: RadialDistortion,
    frame_warp: Option<FrameWarp>,
}

impl LensDistortion {
    pub fn new(
        extent: ImageExtent,
        radial: RadialDistortion,
        frame_warp: Option<FrameWarp>,
    ) -> Self {
        Self {
            extent,
            radial,
            frame_warp,
        }
    }

    pub fn extent(&self) -> ImageExtent {
        self.extent
    }
    pub fn radial(&self) -> RadialDistortion {
        self.radial
    }
    pub fn frame_warp(&self) -> Option<FrameWarp> {
        self.frame_warp
    }

    fn snc_scale(&self) -> f64 {
        (f64::from(self.extent.width()) * f64::from(self.extent.height())).sqrt()
    }

    /// Ideal native pixel -> distorted native pixel of this frame.
    pub fn distort_pixel(&self, ideal: [f64; 2]) -> Result<[f64; 2], DistortionError> {
        let r = self.radial.distort(ideal)?;
        let Some(warp) = self.frame_warp else {
            return Ok(r);
        };
        let n = apply(warp.forward, r)?;
        let s = self.snc_scale();
        Ok([
            (s * n[0] + f64::from(self.extent.width())) / 2.0,
            (s * n[1] + f64::from(self.extent.height())) / 2.0,
        ])
    }

    /// Distorted native pixel of this frame -> ideal native pixel.
    pub fn ideal_pixel(&self, native: [f64; 2]) -> Result<[f64; 2], DistortionError> {
        let r = match self.frame_warp {
            Some(warp) => {
                let s = self.snc_scale();
                let snc = [
                    (2.0 * native[0] - f64::from(self.extent.width())) / s,
                    (2.0 * native[1] - f64::from(self.extent.height())) / s,
                ];
                apply(warp.inverse, snc)?
            }
            None => native,
        };
        self.radial.undistort(r)
    }

    pub fn project(
        &self,
        camera: &Camera,
        world: [f64; 3],
    ) -> Result<DistortedProjection, DistortionError> {
        if camera.extent() != self.extent {
            return Err(DistortionError::ExtentMismatch);
        }
        let ideal = camera.project(world)?.0;
        Ok(DistortedProjection {
            native: self.distort_pixel(ideal)?,
            ideal,
            inside_support: self.radial.in_support(ideal),
        })
    }

    /// Whole-pixel [left, top, right, bottom] guard band so every invertible native
    /// sample position (and its bilinear neighbours) reads rendered ideal CG. The border
    /// plus an interior grid bounds the image of the native raster under this
    /// continuous injective inverse.
    pub(crate) fn guard_band(&self) -> Result<[u32; 4], DistortionError> {
        let (w, h) = (self.extent.width(), self.extent.height());
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        let mut include = |x: u32, y: u32| {
            if let Ok(a) = self.ideal_pixel([f64::from(x) + 0.5, f64::from(y) + 0.5]) {
                for i in 0..2 {
                    lo[i] = lo[i].min(a[i]);
                    hi[i] = hi[i].max(a[i]);
                }
            }
        };
        for x in (0..w).step_by(2).chain([w - 1]) {
            include(x, 0);
            include(x, h - 1);
        }
        for y in (0..h).step_by(2).chain([h - 1]) {
            include(0, y);
            include(w - 1, y);
        }
        for x in (0..w).step_by(32) {
            for y in (0..h).step_by(32) {
                include(x, y);
            }
        }
        if !lo[0].is_finite() {
            return Err(DistortionError::NotInvertible);
        }
        // Texel index = ideal coordinate - 0.5 + margin; keep both bilinear taps inside.
        let margin = |extra: f64| -> Result<u32, DistortionError> {
            let value = extra.max(0.0).ceil() + 1.0;
            if value > 4.0 * f64::from(w.max(h)) {
                return Err(DistortionError::NotInvertible);
            }
            Ok(value as u32)
        };
        Ok([
            margin(0.5 - lo[0])?,
            margin(0.5 - lo[1])?,
            margin(hi[0] + 1.5 - f64::from(w))?,
            margin(hi[1] + 1.5 - f64::from(h))?,
        ])
    }

    /// Uniform block for `distortion.wgsl` (see its `Warp` struct).
    pub(crate) fn gpu_uniform(&self, margins: [u32; 4]) -> [[f32; 4]; 7] {
        let inverse = self.frame_warp.map_or([[0.0; 3]; 3], |warp| {
            let scale = warp
                .inverse
                .iter()
                .flatten()
                .map(|v| v.abs())
                .fold(0.0, f64::max);
            warp.inverse.map(|r| r.map(|v| (v / scale) as f32))
        });
        let r = self.radial;
        [
            [inverse[0][0], inverse[0][1], inverse[0][2], 0.0],
            [inverse[1][0], inverse[1][1], inverse[1][2], 0.0],
            [inverse[2][0], inverse[2][1], inverse[2][2], 0.0],
            [
                r.centre[0] as f32,
                r.centre[1] as f32,
                r.scale as f32,
                r.distorted_radius_limit() as f32,
            ],
            [
                r.k[0] as f32,
                r.k[1] as f32,
                r.k[2] as f32,
                r.ideal_limit as f32,
            ],
            [
                self.extent.width() as f32,
                self.extent.height() as f32,
                self.snc_scale() as f32,
                if self.frame_warp.is_some() { 1.0 } else { 0.0 },
            ],
            [margins[0] as f32, margins[1] as f32, 0.0, 0.0],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extent() -> ImageExtent {
        ImageExtent::new(1920, 1080).unwrap()
    }

    fn radial(k: [f64; 3]) -> RadialDistortion {
        RadialDistortion::new([960.0, 540.0], 960.0, k, None).unwrap()
    }

    #[test]
    fn single_coefficient_limits_match_the_closed_form() {
        let k1 = -0.05394372167711824;
        let model = radial([k1, 0.0, 0.0]);
        let limit = (1.0 / (3.0 * k1.abs())).sqrt();
        assert!((model.ideal_radius_limit() - limit).abs() < 1e-9);
        assert!((model.distorted_radius_limit() - limit * (1.0 + k1 * limit * limit)).abs() < 1e-9);
        // Barrel distortion pulls the corner inwards by about 78 px.
        let corner = model.distort([1920.0, 1080.0]).unwrap();
        let shift = (1920.0 - corner[0]).hypot(1080.0 - corner[1]);
        assert!((shift - 78.2).abs() < 0.1, "{shift}");
        assert_eq!(radial([0.0; 3]).ideal_radius_limit(), RHO_SEARCH);
    }

    #[test]
    fn inverse_round_trips_on_the_monotonic_branch() {
        for k in [
            [-0.054, 0.0, 0.0],
            [-0.08, 0.01, 0.0],
            [0.03, -0.002, 0.0005],
        ] {
            let model = radial(k);
            for x in (-200..=2120).step_by(97) {
                for y in (-150..=1230).step_by(83) {
                    let ideal = [f64::from(x), f64::from(y)];
                    let Ok(distorted) = model.distort(ideal) else {
                        continue;
                    };
                    let back = model.undistort(distorted).unwrap();
                    assert!(
                        (back[0] - ideal[0]).hypot(back[1] - ideal[1]) < 1e-7,
                        "{k:?} {ideal:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn failed_inversion_and_support_are_explicit() {
        let model = radial([-0.3, 0.0, 0.0]);
        let limit = model.distorted_radius_limit() * 960.0;
        assert_eq!(
            model.undistort([960.0 + limit + 1.0, 540.0]),
            Err(DistortionError::NotInvertible)
        );
        assert_eq!(
            model.distort([960.0 + 960.0 * model.ideal_radius_limit() + 1.0, 540.0]),
            Err(DistortionError::NotInvertible)
        );
        let supported = RadialDistortion::new(
            [960.0, 540.0],
            960.0,
            [-0.05, 0.0, 0.0],
            Some(SupportRect {
                min: [-160.0, 680.0],
                max: [2080.0, 1090.0],
            }),
        )
        .unwrap();
        assert!(supported.in_support([960.0, 800.0]));
        assert!(!supported.in_support([960.0, 300.0]));
        for invalid in [
            RadialDistortion::new([960.0, 540.0], 0.0, [0.0; 3], None),
            RadialDistortion::new([f64::NAN, 540.0], 960.0, [0.0; 3], None),
            RadialDistortion::new([960.0, 540.0], 960.0, [f64::INFINITY, 0.0, 0.0], None),
        ] {
            assert_eq!(invalid, Err(DistortionError::InvalidParameters));
        }
    }

    #[test]
    fn frame_warp_chain_round_trips_and_rejects_singular_matrices() {
        let s = (1920.0_f64 * 1080.0).sqrt();
        // Pixels -> SNC with a small rotation, zoom and shift, like a postwarp.
        let (c, sn) = (0.01_f64.cos() * 1.002, 0.01_f64.sin() * 1.002);
        let warp = FrameWarp::new([
            [2.0 * c / s, -2.0 * sn / s, -1920.0 / s + 0.003],
            [2.0 * sn / s, 2.0 * c / s, -1080.0 / s - 0.002],
            [1e-7, 0.0, 1.0],
        ])
        .unwrap();
        let lens = LensDistortion::new(extent(), radial([-0.054, 0.0, 0.0]), Some(warp));
        for ideal in [
            [960.0, 540.0],
            [100.0, 900.0],
            [1800.0, 200.0],
            [1919.0, 1079.0],
        ] {
            let native = lens.distort_pixel(ideal).unwrap();
            let back = lens.ideal_pixel(native).unwrap();
            assert!(
                (back[0] - ideal[0]).hypot(back[1] - ideal[1]) < 1e-6,
                "{ideal:?}"
            );
        }
        // Without a frame warp the radial result is already in native pixels.
        let plain = LensDistortion::new(extent(), radial([0.0; 3]), None);
        assert_eq!(plain.distort_pixel([12.5, 34.0]).unwrap(), [12.5, 34.0]);
        assert_eq!(
            FrameWarp::new([[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.0, 0.0, 1.0]]),
            Err(DistortionError::InvalidFrameWarp)
        );
    }
}
