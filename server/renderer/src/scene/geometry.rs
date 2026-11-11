use std::{ops::Range, sync::Arc};

use super::SceneError;

/// A finite axis-aligned box in metres. In chr world space, max Z faces the ground.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    min: [f64; 3],
    max: [f64; 3],
}

impl Aabb {
    pub fn new(min: [f64; 3], max: [f64; 3]) -> Result<Self, SceneError> {
        if !min.into_iter().chain(max).all(f64::is_finite)
            || min.into_iter().zip(max).any(|(lo, hi)| lo > hi)
        {
            return Err(SceneError::InvalidBounds);
        }
        Ok(Self { min, max })
    }

    pub fn min(self) -> [f64; 3] {
        self.min
    }

    pub fn max(self) -> [f64; 3] {
        self.max
    }

    fn include(&mut self, point: [f64; 3]) {
        for (axis, value) in point.into_iter().enumerate() {
            self.min[axis] = self.min[axis].min(value);
            self.max[axis] = self.max[axis].max(value);
        }
    }

    /// Conservative transformed bounds; all eight corners participate.
    pub fn transformed(self, model: ModelMatrix) -> Result<Self, SceneError> {
        let first = model.transform_point(self.min)?;
        let mut bounds = Self {
            min: first,
            max: first,
        };
        for corner in 1..8 {
            let point = std::array::from_fn(|axis| {
                if corner & (1 << axis) == 0 {
                    self.min[axis]
                } else {
                    self.max[axis]
                }
            });
            bounds.include(model.transform_point(point)?);
        }
        Ok(bounds)
    }
}

/// Forward model-local to world, row-major, acting on homogeneous column vectors.
/// The affine last row is exactly [0, 0, 0, 1]. Singular matrices are rejected;
/// visibility, not zero scale, hides an instance. No coefficient is renormalized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelMatrix {
    rows: [[f64; 4]; 4],
    normal: [[f64; 3]; 3],
    mirrored: bool,
}

impl ModelMatrix {
    pub const IDENTITY: Self = Self {
        rows: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
        normal: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        mirrored: false,
    };

    pub fn new(rows: [[f64; 4]; 4]) -> Result<Self, SceneError> {
        if !rows.iter().flatten().all(|v| v.is_finite()) {
            return Err(SceneError::NonFinite);
        }
        if rows[3] != [0.0, 0.0, 0.0, 1.0] {
            return Err(SceneError::NonAffineModel);
        }
        let [[a, b, c, _], [d, e, f, _], [g, h, i, _], _] = rows;
        let cofactors = [
            [e * i - f * h, f * g - d * i, d * h - e * g],
            [c * h - b * i, a * i - c * g, b * g - a * h],
            [b * f - c * e, c * d - a * f, a * e - b * d],
        ];
        let determinant = a * cofactors[0][0] + b * cofactors[0][1] + c * cofactors[0][2];
        if !determinant.is_finite() {
            return Err(SceneError::NonFinite);
        }
        if determinant == 0.0 {
            return Err(SceneError::SingularModel);
        }
        let normal = cofactors.map(|row| row.map(|v| v / determinant));
        if !normal.iter().flatten().all(|v| v.is_finite()) {
            return Err(SceneError::NonFinite);
        }
        Ok(Self {
            rows,
            normal,
            mirrored: determinant < 0.0,
        })
    }

    pub fn rows(self) -> [[f64; 4]; 4] {
        self.rows
    }

    /// Rasterization must account for reversed winding separately from normals.
    pub fn is_mirrored(self) -> bool {
        self.mirrored
    }

    /// Row-major inverse transpose of the model's linear part.
    pub fn normal_matrix(self) -> [[f64; 3]; 3] {
        self.normal
    }

    pub fn transform_point(self, point: [f64; 3]) -> Result<[f64; 3], SceneError> {
        let result = std::array::from_fn(|axis| {
            let row = self.rows[axis];
            row[0] * point[0] + row[1] * point[1] + row[2] * point[2] + row[3]
        });
        if !result.into_iter().all(f64::is_finite) {
            return Err(SceneError::NonFinite);
        }
        Ok(result)
    }

    pub fn transform_normal(self, normal: [f64; 3]) -> Result<[f64; 3], SceneError> {
        let result = self
            .normal
            .map(|row| row.into_iter().zip(normal).map(|(a, b)| a * b).sum::<f64>());
        unit_vector(result)
    }

    /// Moves the transformed box's ground-facing max Z to zero, keeping XY.
    pub fn grounded(self, local_bounds: Aabb) -> Result<Self, SceneError> {
        let mut rows = self.rows;
        rows[2][3] -= local_bounds.transformed(self)?.max[2];
        Self::new(rows)
    }
}

pub(super) fn unit_vector(vector: [f64; 3]) -> Result<[f64; 3], SceneError> {
    if !vector.into_iter().all(f64::is_finite) {
        return Err(SceneError::NonFinite);
    }
    let scale = vector.into_iter().map(f64::abs).fold(0.0, f64::max);
    if scale == 0.0 {
        return Err(SceneError::ZeroDirection);
    }
    let scaled = vector.map(|v| v / scale);
    let length = scaled.into_iter().map(|v| v * v).sum::<f64>().sqrt();
    Ok(scaled.map(|v| v / length))
}

/// Indexed triangle data in model-local metres. Parts partition the index array.
/// Geometry is immutable and shared; neither transforms nor materials live here.
#[derive(Clone, Debug)]
pub struct MeshData {
    positions: Arc<[[f32; 3]]>,
    indices: Arc<[u32]>,
    parts: Arc<[Range<u32>]>,
    bounds: Aabb,
}

impl MeshData {
    pub fn new(
        positions: Vec<[f32; 3]>,
        indices: Vec<u32>,
        parts: Vec<Range<u32>>,
    ) -> Result<Self, SceneError> {
        let count = indices.len();
        if positions.is_empty()
            || count == 0
            || !count.is_multiple_of(3)
            || indices
                .iter()
                .any(|&index| index as usize >= positions.len())
            || !positions.iter().flatten().all(|v| v.is_finite())
        {
            return Err(SceneError::InvalidMesh);
        }
        let mut end = 0;
        for part in &parts {
            if part.start != end
                || part.end <= part.start
                || part.end as usize > count
                || !(part.end - part.start).is_multiple_of(3)
            {
                return Err(SceneError::InvalidMeshParts);
            }
            end = part.end;
        }
        if end as usize != count {
            return Err(SceneError::InvalidMeshParts);
        }
        let first = positions[indices[0] as usize].map(f64::from);
        let mut bounds = Aabb {
            min: first,
            max: first,
        };
        for &index in &indices {
            bounds.include(positions[index as usize].map(f64::from));
        }
        Ok(Self {
            positions: positions.into(),
            indices: indices.into(),
            parts: parts.into(),
            bounds,
        })
    }

    pub fn positions(&self) -> &[[f32; 3]] {
        &self.positions
    }

    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    pub fn parts(&self) -> &[Range<u32>] {
        &self.parts
    }

    pub fn bounds(&self) -> Aabb {
        self.bounds
    }
}
