//! Pitch-centred metres: X along length, Y along width, ground Z=0, up is -Z.
//! Ti is a forward, row-major homography acting on homogeneous column vectors.

use thiserror::Error;

/// Invalid geometry is reported rather than replaced with fabricated coordinates.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoordinateError {
    #[error("image dimensions must be nonzero")]
    EmptyImage,
    #[error("coordinates and matrix coefficients must be finite")]
    NonFinite,
    #[error("Ti must be invertible")]
    SingularTi,
    #[error("camera centre must have nonzero Z")]
    CameraOnGround,
    #[error("point has no finite image projection")]
    AtInfinity,
}

/// Native raster dimensions, not logical window dimensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageExtent {
    width: u32,
    height: u32,
}

impl ImageExtent {
    pub fn new(width: u32, height: u32) -> Result<Self, CoordinateError> {
        if width == 0 || height == 0 {
            return Err(CoordinateError::EmptyImage);
        }
        Ok(Self { width, height })
    }

    pub fn width(self) -> u32 {
        self.width
    }

    pub fn height(self) -> u32 {
        self.height
    }

    fn scale(self) -> f64 {
        (f64::from(self.width) * f64::from(self.height)).sqrt()
    }

    /// Integer pixel centres; deliberately no half-pixel offset.
    pub fn pixel_to_snc(self, [u, v]: [f64; 2]) -> [f64; 2] {
        [
            (2.0 * u - f64::from(self.width)) / self.scale(),
            (2.0 * v - f64::from(self.height)) / self.scale(),
        ]
    }

    pub fn snc_to_pixel(self, [x, y]: [f64; 2]) -> [f64; 2] {
        [
            (self.scale() * x + f64::from(self.width)) / 2.0,
            (self.scale() * y + f64::from(self.height)) / 2.0,
        ]
    }

    /// SNC is Y-down; wgpu NDC is Y-up. Depth is a separate rendering concern.
    pub fn snc_to_ndc_scale(self) -> [f64; 2] {
        [
            self.scale() / f64::from(self.width),
            -self.scale() / f64::from(self.height),
        ]
    }
}

/// Forward ground-to-SNC homography. Coefficients are never renormalized.
#[derive(Clone, Copy, Debug)]
pub struct Ti {
    rows: [[f64; 3]; 3],
}

impl Ti {
    /// Original forward row-major coefficients, including the supplied scale.
    pub fn rows(self) -> [[f64; 3]; 3] {
        self.rows
    }

    pub fn new(rows: [[f64; 3]; 3]) -> Result<Self, CoordinateError> {
        if !rows.iter().flatten().all(|value| value.is_finite()) {
            return Err(CoordinateError::NonFinite);
        }
        let [[a, b, c], [d, e, f], [g, h, i]] = rows;
        let determinant = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
        if !determinant.is_finite() {
            return Err(CoordinateError::NonFinite);
        }
        if determinant == 0.0 {
            return Err(CoordinateError::SingularTi);
        }
        Ok(Self { rows })
    }

    /// Projects ground XY metres; no camera centre is required for Z=0.
    pub fn project_ground(self, [x, y]: [f64; 2]) -> Result<[f64; 2], CoordinateError> {
        self.project_homogeneous([x, y, 1.0])
    }

    /// Projects XYZ metres through A(C), then Ti, dividing only at the end.
    /// The camera centre is in world metres, not extrinsic translation.
    pub fn project_world(
        self,
        [x, y, z]: [f64; 3],
        [cx, cy, cz]: [f64; 3],
    ) -> Result<[f64; 2], CoordinateError> {
        if ![x, y, z, cx, cy, cz].iter().all(|v| v.is_finite()) {
            return Err(CoordinateError::NonFinite);
        }
        if cz == 0.0 {
            return Err(CoordinateError::CameraOnGround);
        }
        self.project_homogeneous([x - z * cx / cz, y - z * cy / cz, 1.0 - z / cz])
    }

    fn project_homogeneous(self, point: [f64; 3]) -> Result<[f64; 2], CoordinateError> {
        if !point.iter().all(|v| v.is_finite()) {
            return Err(CoordinateError::NonFinite);
        }
        let h = self.rows.map(|row| {
            row.into_iter()
                .zip(point)
                .map(|(coefficient, coordinate)| coefficient * coordinate)
                .sum::<f64>()
        });
        if !h.iter().all(|v| v.is_finite()) {
            return Err(CoordinateError::NonFinite);
        }
        if h[2] == 0.0 {
            return Err(CoordinateError::AtInfinity);
        }
        let result = [h[0] / h[2], h[1] / h[2]];
        if !result.iter().all(|v| v.is_finite()) {
            return Err(CoordinateError::NonFinite);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: [f64; 2], expected: [f64; 2]) {
        for (a, e) in actual.into_iter().zip(expected) {
            assert!((a - e).abs() < 1e-10, "{actual:?} != {expected:?}");
        }
    }

    #[test]
    fn native_pixels_round_trip_without_half_pixel_shift() {
        for (width, height) in [(1280, 720), (720, 1280), (641, 481)] {
            let extent = ImageExtent::new(width, height).unwrap();
            close(
                extent.pixel_to_snc([f64::from(width) / 2.0, f64::from(height) / 2.0]),
                [0.0, 0.0],
            );
            for pixel in [[0.0, 0.0], [17.25, 30.75]] {
                close(extent.snc_to_pixel(extent.pixel_to_snc(pixel)), pixel);
            }
            let snc = extent.pixel_to_snc([0.0, 0.0]);
            let scale = extent.snc_to_ndc_scale();
            close([snc[0] * scale[0], snc[1] * scale[1]], [-1.0, 1.0]);
        }
        assert_eq!(ImageExtent::new(0, 720), Err(CoordinateError::EmptyImage));
    }

    #[test]
    fn forward_row_major_ti_divides_by_point_dependent_scale() {
        let rows = [[2.0, 1.0, 3.0], [0.0, 3.0, -2.0], [0.25, 0.0, 1.0]];
        let ti = Ti::new(rows).unwrap();
        close(ti.project_ground([4.0, 2.0]).unwrap(), [6.5, 2.0]);
        let scaled = Ti::new(rows.map(|row| row.map(|v| -2.0 * v))).unwrap();
        close(scaled.project_ground([4.0, 2.0]).unwrap(), [6.5, 2.0]);
        assert_eq!(
            ti.project_ground([-4.0, 0.0]),
            Err(CoordinateError::AtInfinity)
        );
    }

    #[test]
    fn elevated_points_use_negative_z_and_camera_centre() {
        let ti = Ti::new([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]).unwrap();
        let camera = [2.0, 4.0, -10.0];
        close(
            ti.project_world([3.0, 5.0, 0.0], camera).unwrap(),
            ti.project_ground([3.0, 5.0]).unwrap(),
        );
        close(
            ti.project_world([3.0, 5.0, -5.0], camera).unwrap(),
            [4.0, 6.0],
        );
        assert_eq!(
            ti.project_world(camera, camera),
            Err(CoordinateError::AtInfinity)
        );
        assert_eq!(
            ti.project_world([1.0; 3], [0.0; 3]),
            Err(CoordinateError::CameraOnGround)
        );
    }

    #[test]
    fn intermediate_ground_infinity_can_have_finite_image_projection() {
        let ti = Ti::new([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 1.0]]).unwrap();
        close(
            ti.project_world([3.0, 5.0, -10.0], [2.0, 4.0, -10.0])
                .unwrap(),
            [1.0, 1.0],
        );
        assert!(matches!(
            Ti::new([[0.0; 3]; 3]),
            Err(CoordinateError::SingularTi)
        ));
        assert!(matches!(
            Ti::new([[f64::NAN; 3]; 3]),
            Err(CoordinateError::NonFinite)
        ));
    }
}
