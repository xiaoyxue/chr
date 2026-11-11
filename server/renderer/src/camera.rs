//! Pinhole and parallel-ray cameras in chr's negative-Z-up world.
//! Optical axes are right/down/forward; only pinhole cameras have K or Ti.

use thiserror::Error;

use crate::coordinates::{ImageExtent, Ti};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CameraError {
    #[error("camera geometry must be finite and nondegenerate")]
    InvalidGeometry,
    #[error("intrinsics require finite positive focal lengths")]
    InvalidIntrinsics,
    #[error("orthographic spans must be finite and positive")]
    InvalidSpan,
    #[error("clip planes require 0 < near < far")]
    InvalidClip,
    #[error("camera pose must be a proper rigid camera-to-world transform")]
    InvalidPose,
    #[error("point is behind the camera or outside the depth interval")]
    OutsideDepth,
}

/// Pixel units; skew is K[0][1]. These are not SNC coefficients.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Intrinsics {
    pub fx: f64,
    pub fy: f64,
    pub skew: f64,
    pub cx: f64,
    pub cy: f64,
}

impl Intrinsics {
    pub fn from_fov(extent: ImageExtent, vertical_radians: f64) -> Result<Self, CameraError> {
        if !(0.0..std::f64::consts::PI).contains(&vertical_radians) || vertical_radians == 0.0 {
            return Err(CameraError::InvalidIntrinsics);
        }
        let focal = f64::from(extent.height()) / (2.0 * (vertical_radians / 2.0).tan());
        Ok(Self {
            fx: focal,
            fy: focal,
            skew: 0.0,
            cx: f64::from(extent.width()) / 2.0,
            cy: f64::from(extent.height()) / 2.0,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Ray {
    pub origin: [f64; 3],
    pub direction: [f64; 3],
}

/// Shared ideal-camera operations for the internal renderer, not a wire schema.
/// Ray origins lie in the optical depth-zero plane; directions are unit world vectors.
/// Implementations are sealed so renderer invariants cannot be bypassed.
pub trait CameraView: sealed::Sealed {
    fn extent(&self) -> ImageExtent;
    fn center(&self) -> [f64; 3];
    fn forward(&self) -> [f64; 3];
    /// Positive optical near/far distances in metres.
    fn depth_range(&self) -> [f64; 2];
    fn is_orthographic(&self) -> bool;
    /// Ideal native pixels and wgpu depth; image XY may lie outside the raster.
    fn project(&self, world: [f64; 3]) -> Result<([f64; 2], f64), CameraError>;
    fn ray(&self, pixel: [f64; 2]) -> Result<Ray, CameraError>;
    /// Column-major f32 clip-from-world, including the image Y flip.
    fn clip_from_world(&self) -> Result<[[f32; 4]; 4], CameraError>;
}

mod sealed {
    pub trait Sealed {}
}

/// A genuine parallel-ray camera with horizontal/vertical spans in world metres.
/// There are no perspective intrinsics, Ti import, or lens-distortion parameters.
#[derive(Clone, Debug)]
pub struct OrthographicCamera {
    extent: ImageExtent,
    span: [f64; 2],
    center: [f64; 3],
    axes: [[f64; 3]; 3],
    near: f64,
    far: f64,
}

impl OrthographicCamera {
    /// Row-major rigid camera-to-world pose acting on column vectors.
    /// The pose origin maps to the image centre. Spans are full width/height,
    /// independent of aspect ratio and optical depth; `0 < near < far`.
    pub fn from_pose(
        extent: ImageExtent,
        span: [f64; 2],
        pose: [[f64; 4]; 4],
        near: f64,
        far: f64,
    ) -> Result<Self, CameraError> {
        if !span.into_iter().all(|v| v.is_finite() && v > 0.0) {
            return Err(CameraError::InvalidSpan);
        }
        if !pose.iter().flatten().all(|v| v.is_finite()) || pose[3] != [0.0, 0.0, 0.0, 1.0] {
            return Err(CameraError::InvalidPose);
        }
        let center = [pose[0][3], pose[1][3], pose[2][3]];
        let axes = std::array::from_fn(|axis| std::array::from_fn(|row| pose[row][axis]));
        validate_frame(center, axes, near, far)?;
        Ok(Self {
            extent,
            span,
            center,
            axes,
            near,
            far,
        })
    }

    pub fn span(&self) -> [f64; 2] {
        self.span
    }
}

impl sealed::Sealed for OrthographicCamera {}

impl CameraView for OrthographicCamera {
    fn extent(&self) -> ImageExtent {
        self.extent
    }
    fn center(&self) -> [f64; 3] {
        self.center
    }
    fn forward(&self) -> [f64; 3] {
        self.axes[2]
    }
    fn depth_range(&self) -> [f64; 2] {
        [self.near, self.far]
    }
    fn is_orthographic(&self) -> bool {
        true
    }

    fn project(&self, world: [f64; 3]) -> Result<([f64; 2], f64), CameraError> {
        let p = self.axes.map(|axis| dot(axis, sub(world, self.center)));
        if !p.into_iter().all(f64::is_finite) {
            return Err(CameraError::InvalidGeometry);
        }
        if p[2] < self.near || p[2] > self.far {
            return Err(CameraError::OutsideDepth);
        }
        let pixel = [
            (p[0] / self.span[0] + 0.5) * f64::from(self.extent.width()),
            (p[1] / self.span[1] + 0.5) * f64::from(self.extent.height()),
        ];
        let depth = (p[2] - self.near) / (self.far - self.near);
        if !pixel.into_iter().chain([depth]).all(f64::is_finite) {
            return Err(CameraError::InvalidGeometry);
        }
        Ok((pixel, depth))
    }

    fn ray(&self, pixel: [f64; 2]) -> Result<Ray, CameraError> {
        let x = (pixel[0] / f64::from(self.extent.width()) - 0.5) * self.span[0];
        let y = (pixel[1] / f64::from(self.extent.height()) - 0.5) * self.span[1];
        let origin =
            std::array::from_fn(|j| self.center[j] + self.axes[0][j] * x + self.axes[1][j] * y);
        if !origin.into_iter().all(f64::is_finite) {
            return Err(CameraError::InvalidGeometry);
        }
        Ok(Ray {
            origin,
            direction: self.axes[2],
        })
    }

    fn clip_from_world(&self) -> Result<[[f32; 4]; 4], CameraError> {
        let z_scale = 1.0 / (self.far - self.near);
        clip_matrix(
            [
                [2.0 / self.span[0], 0.0, 0.0, 0.0],
                [0.0, -2.0 / self.span[1], 0.0, 0.0],
                [0.0, 0.0, z_scale, -self.near * z_scale],
                [0.0, 0.0, 0.0, 1.0],
            ],
            self.axes,
            self.center,
        )
    }
}

/// Immutable camera; derived matrices never rewrite the imported Ti.
#[derive(Clone, Debug)]
pub struct Camera {
    extent: ImageExtent,
    intrinsics: Intrinsics,
    center: [f64; 3],
    axes: [[f64; 3]; 3],
    near: f64,
    far: f64,
    source_ti: Option<Ti>,
}

impl Camera {
    /// Pose is row-major camera-to-world, acting on column vectors.
    pub fn from_pose(
        extent: ImageExtent,
        intrinsics: Intrinsics,
        pose: [[f64; 4]; 4],
        near: f64,
        far: f64,
    ) -> Result<Self, CameraError> {
        if !pose.iter().flatten().all(|v| v.is_finite()) || pose[3] != [0.0, 0.0, 0.0, 1.0] {
            return Err(CameraError::InvalidPose);
        }
        let axes = std::array::from_fn(|axis| std::array::from_fn(|row| pose[row][axis]));
        Self::new(
            extent,
            intrinsics,
            [pose[0][3], pose[1][3], pose[2][3]],
            axes,
            near,
            far,
        )
    }

    pub fn look_at(
        extent: ImageExtent,
        center: [f64; 3],
        target: [f64; 3],
        up: [f64; 3],
        vertical_radians: f64,
        near: f64,
        far: f64,
    ) -> Result<Self, CameraError> {
        let forward = unit(sub(target, center))?;
        let right = unit(cross(forward, up))?;
        let down = cross(forward, right);
        Self::new(
            extent,
            Intrinsics::from_fov(extent, vertical_radians)?,
            center,
            [right, down, forward],
            near,
            far,
        )
    }

    /// Decomposes S^-1 Ti A(C) into K and a proper optical frame.
    /// The sign of the derived matrix fixes handedness, not the stored Ti scale.
    pub fn from_ti(
        extent: ImageExtent,
        ti: Ti,
        center: [f64; 3],
        near: f64,
        far: f64,
    ) -> Result<Self, CameraError> {
        if !center.into_iter().all(f64::is_finite) || center[2] == 0.0 {
            return Err(CameraError::InvalidGeometry);
        }
        let h = ti.rows().map(|r| {
            [
                r[0],
                r[1],
                -(r[0] * center[0] + r[1] * center[1] + r[2]) / center[2],
            ]
        });
        let scale = (f64::from(extent.width()) * f64::from(extent.height())).sqrt();
        let mut m = [
            std::array::from_fn(|j| (scale * h[0][j] + f64::from(extent.width()) * h[2][j]) / 2.0),
            std::array::from_fn(|j| (scale * h[1][j] + f64::from(extent.height()) * h[2][j]) / 2.0),
            h[2],
        ];
        let magnitude = m.iter().flatten().map(|v| v.abs()).fold(0.0, f64::max);
        if !m.iter().flatten().all(|v| v.is_finite()) || magnitude == 0.0 {
            return Err(CameraError::InvalidGeometry);
        }
        m = m.map(|r| r.map(|v| v / magnitude));
        if dot(m[0], cross(m[1], m[2])) < 0.0 {
            m = m.map(|r| r.map(|v| -v));
        }
        let z = unit(m[2])?;
        let k33 = dot(m[2], z);
        let cy = dot(m[1], z);
        let y_component = sub(m[1], mul(z, cy));
        let y = unit(y_component)?;
        let fy = dot(y_component, y);
        let cx = dot(m[0], z);
        let skew = dot(m[0], y);
        let x_component = sub(sub(m[0], mul(z, cx)), mul(y, skew));
        let x = unit(x_component)?;
        let fx = dot(x_component, x);
        let k = Intrinsics {
            fx: fx / k33,
            fy: fy / k33,
            skew: skew / k33,
            cx: cx / k33,
            cy: cy / k33,
        };
        let mut camera = Self::new(extent, k, center, [x, y, z], near, far)?;
        camera.source_ti = Some(ti);
        Ok(camera)
    }

    fn new(
        extent: ImageExtent,
        k: Intrinsics,
        center: [f64; 3],
        axes: [[f64; 3]; 3],
        near: f64,
        far: f64,
    ) -> Result<Self, CameraError> {
        if ![k.fx, k.fy, k.skew, k.cx, k.cy]
            .into_iter()
            .all(f64::is_finite)
            || k.fx <= 0.0
            || k.fy <= 0.0
        {
            return Err(CameraError::InvalidIntrinsics);
        }
        validate_frame(center, axes, near, far)?;
        Ok(Self {
            extent,
            intrinsics: k,
            center,
            axes,
            near,
            far,
            source_ti: None,
        })
    }

    pub fn extent(&self) -> ImageExtent {
        self.extent
    }
    pub fn intrinsics(&self) -> Intrinsics {
        self.intrinsics
    }
    pub fn center(&self) -> [f64; 3] {
        self.center
    }
    pub(crate) fn depth_range(&self) -> [f64; 2] {
        [self.near, self.far]
    }
    pub fn source_ti(&self) -> Option<Ti> {
        self.source_ti
    }

    /// Returns ideal native pixels and wgpu depth. Image XY may lie outside the raster.
    pub fn project(&self, world: [f64; 3]) -> Result<([f64; 2], f64), CameraError> {
        let p = self.axes.map(|axis| dot(axis, sub(world, self.center)));
        if !p.into_iter().all(f64::is_finite) {
            return Err(CameraError::InvalidGeometry);
        }
        if p[2] < self.near || p[2] > self.far {
            return Err(CameraError::OutsideDepth);
        }
        let k = self.intrinsics;
        let pixel = [
            (k.fx * p[0] + k.skew * p[1]) / p[2] + k.cx,
            k.fy * p[1] / p[2] + k.cy,
        ];
        let depth = self.far / (self.far - self.near) * (1.0 - self.near / p[2]);
        if !pixel.into_iter().chain([depth]).all(f64::is_finite) {
            return Err(CameraError::InvalidGeometry);
        }
        Ok((pixel, depth))
    }

    pub fn ray(&self, pixel: [f64; 2]) -> Result<Ray, CameraError> {
        let k = self.intrinsics;
        let y = (pixel[1] - k.cy) / k.fy;
        let x = (pixel[0] - k.cx - k.skew * y) / k.fx;
        let direction = unit(std::array::from_fn(|j| {
            self.axes[0][j] * x + self.axes[1][j] * y + self.axes[2][j]
        }))?;
        Ok(Ray {
            origin: self.center,
            direction,
        })
    }

    /// Column-major f32 clip-from-world for WGSL. The image Y flip happens here.
    pub fn clip_from_world(&self) -> Result<[[f32; 4]; 4], CameraError> {
        let k = self.intrinsics;
        let w = f64::from(self.extent.width());
        let h = f64::from(self.extent.height());
        let z_scale = self.far / (self.far - self.near);
        let projection = [
            [2.0 * k.fx / w, 2.0 * k.skew / w, 2.0 * k.cx / w - 1.0, 0.0],
            [0.0, -2.0 * k.fy / h, 1.0 - 2.0 * k.cy / h, 0.0],
            [0.0, 0.0, z_scale, -self.near * z_scale],
            [0.0, 0.0, 1.0, 0.0],
        ];
        clip_matrix(projection, self.axes, self.center)
    }
}

impl sealed::Sealed for Camera {}

impl CameraView for Camera {
    fn extent(&self) -> ImageExtent {
        self.extent()
    }
    fn center(&self) -> [f64; 3] {
        self.center()
    }
    fn forward(&self) -> [f64; 3] {
        self.axes[2]
    }
    fn depth_range(&self) -> [f64; 2] {
        self.depth_range()
    }
    fn is_orthographic(&self) -> bool {
        false
    }
    fn project(&self, world: [f64; 3]) -> Result<([f64; 2], f64), CameraError> {
        self.project(world)
    }
    fn ray(&self, pixel: [f64; 2]) -> Result<Ray, CameraError> {
        self.ray(pixel)
    }
    fn clip_from_world(&self) -> Result<[[f32; 4]; 4], CameraError> {
        self.clip_from_world()
    }
}

fn validate_frame(
    center: [f64; 3],
    axes: [[f64; 3]; 3],
    near: f64,
    far: f64,
) -> Result<(), CameraError> {
    if !near.is_finite() || !far.is_finite() || near <= 0.0 || far <= near {
        return Err(CameraError::InvalidClip);
    }
    if !center.into_iter().all(f64::is_finite)
        || !axes.iter().flatten().all(|v| v.is_finite())
        || (0..3).any(|i| {
            (0..3).any(|j| (dot(axes[i], axes[j]) - if i == j { 1.0 } else { 0.0 }).abs() > 1e-9)
        })
        || (dot(axes[0], cross(axes[1], axes[2])) - 1.0).abs() > 1e-9
    {
        return Err(CameraError::InvalidPose);
    }
    Ok(())
}

fn clip_matrix(
    projection: [[f64; 4]; 4],
    axes: [[f64; 3]; 3],
    center: [f64; 3],
) -> Result<[[f32; 4]; 4], CameraError> {
    let mut view = [[0.0; 4]; 4];
    for (i, axis) in axes.iter().enumerate() {
        view[i][..3].copy_from_slice(axis);
        view[i][3] = -dot(*axis, center);
    }
    view[3][3] = 1.0;
    let columns: [[f32; 4]; 4] = std::array::from_fn(|column| {
        std::array::from_fn(|row| {
            (0..4)
                .map(|j| projection[row][j] * view[j][column])
                .sum::<f64>() as f32
        })
    });
    if !columns.iter().flatten().all(|v| v.is_finite()) {
        return Err(CameraError::InvalidGeometry);
    }
    Ok(columns)
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter().zip(b).map(|(x, y)| x * y).sum()
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn mul(a: [f64; 3], s: f64) -> [f64; 3] {
    a.map(|v| v * s)
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn unit(v: [f64; 3]) -> Result<[f64; 3], CameraError> {
    let scale = v.into_iter().map(f64::abs).fold(0.0, f64::max);
    if !v.into_iter().all(f64::is_finite) || scale == 0.0 {
        return Err(CameraError::InvalidGeometry);
    }
    let v = v.map(|component| component / scale);
    Ok(mul(v, 1.0 / dot(v, v).sqrt()))
}
