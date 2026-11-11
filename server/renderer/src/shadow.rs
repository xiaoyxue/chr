use crate::{
    camera::Camera,
    coordinates::ImageExtent,
    offscreen::BoxError,
    scene::{Aabb, Light, SceneView},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum ShadowMode {
    #[default]
    Off = 0,
    Hard = 1,
    Pcf = 2,
    Pcss = 3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PcssSettings {
    /// Each stage supports 1..=64 deterministic disk samples.
    pub blocker_samples: u32,
    pub filter_samples: u32,
    /// Independent search/filter caps in shadow texels, not physical source sizes.
    pub max_search_radius: f32,
    pub max_filter_radius: f32,
    /// Worst-case blocker plus filter samples across all lights per fragment.
    pub max_samples_per_fragment: u64,
}

impl Default for PcssSettings {
    fn default() -> Self {
        Self {
            blocker_samples: 32,
            filter_samples: 32,
            max_search_radius: 64.0,
            max_filter_radius: 64.0,
            max_samples_per_fragment: 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowSettings {
    pub mode: ShadowMode,
    pub resolution: u32,
    /// Radius in shadow texels for a 3x3 kernel of bilinear comparisons.
    pub filter_radius: f32,
    /// Bias in normalized shadow depth.
    pub depth_bias: f32,
    /// Receiver offset along its unperturbed surface normal, in world metres.
    pub normal_bias: f32,
    pub max_bytes: u64,
    pub pcss: PcssSettings,
}

impl Default for ShadowSettings {
    fn default() -> Self {
        Self {
            mode: ShadowMode::Off,
            resolution: 2048,
            filter_radius: 1.5,
            depth_bias: 0.0005,
            normal_bias: 0.02,
            max_bytes: 256 * 1024 * 1024,
            pcss: PcssSettings::default(),
        }
    }
}

impl ShadowSettings {
    pub(crate) fn validate(self) -> Result<(), BoxError> {
        if self.resolution == 0
            || !self.filter_radius.is_finite()
            || self.filter_radius < 0.0
            || !self.depth_bias.is_finite()
            || !(0.0..=1.0).contains(&self.depth_bias)
            || !self.normal_bias.is_finite()
            || self.normal_bias < 0.0
        {
            return Err("invalid shadow resolution, filter radius or bias".into());
        }
        if self.mode == ShadowMode::Pcss
            && (!(1..=64).contains(&self.pcss.blocker_samples)
                || !(1..=64).contains(&self.pcss.filter_samples)
                || [self.pcss.max_search_radius, self.pcss.max_filter_radius]
                    .into_iter()
                    .any(|radius| !radius.is_finite() || radius <= 0.0))
        {
            return Err("invalid PCSS sample counts or radius limits".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ShadowProjection {
    pub matrix: [[f32; 4]; 4],
    /// Near/far and planar UV scales; point maps use z for the nearest scene-bound radius.
    pub depth_scale: [f32; 4],
    pub point_position: Option<[f32; 3]>,
}

pub(crate) struct ShadowAtlas {
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    pub layers: Vec<wgpu::TextureView>,
    resolution: u32,
}

impl ShadowAtlas {
    pub fn new(device: &wgpu::Device, resolution: u32, layers: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow maps"),
            size: wgpu::Extent3d {
                width: resolution,
                height: resolution,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let layers = (0..layers)
            .map(|layer| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            compare: Some(wgpu::CompareFunction::LessEqual),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            view,
            sampler,
            layers,
            resolution,
        }
    }

    pub fn ensure(
        &mut self,
        device: &wgpu::Device,
        resolution: u32,
        layers: u32,
        max_bytes: u64,
    ) -> Result<bool, BoxError> {
        if resolution > device.limits().max_texture_dimension_2d
            || layers > device.limits().max_texture_array_layers
        {
            return Err("shadow maps exceed texture limits".into());
        }
        let bytes = u64::from(resolution) * u64::from(resolution) * 4 * u64::from(layers);
        if bytes > max_bytes {
            return Err("shadow maps exceed the configured memory budget".into());
        }
        if self.resolution == resolution && self.layers.len() == layers as usize {
            return Ok(false);
        }
        *self = Self::new(device, resolution, layers);
        Ok(true)
    }
}

pub(crate) fn scene_bounds(scene: &SceneView) -> Option<Aabb> {
    let mut bounds = scene
        .instances()
        .filter(|(_, i)| i.visible())
        .map(|(_, i)| i.bounds());
    let first = bounds.next()?;
    let mut lo = first.min();
    let mut hi = first.max();
    for next in bounds {
        lo = std::array::from_fn(|i| lo[i].min(next.min()[i]));
        hi = std::array::from_fn(|i| hi[i].max(next.max()[i]));
    }
    // Extending validated AABBs preserves their ordering and finiteness.
    Some(Aabb::new(lo, hi).expect("union of valid bounds"))
}

pub(crate) fn projection(
    light: Light,
    bounds: Aabb,
    settings: ShadowSettings,
) -> Result<Option<ShadowProjection>, BoxError> {
    let lo = bounds.min();
    let hi = bounds.max();
    let center = std::array::from_fn::<_, 3, _>(|i| (lo[i] + hi[i]) * 0.5);
    let diagonal = (0..3)
        .map(|i| (hi[i] - lo[i]).powi(2))
        .sum::<f64>()
        .sqrt()
        .max(0.01);
    let padding = diagonal
        * (0.01 + 2.0 * (f64::from(settings.filter_radius) + 1.0) / f64::from(settings.resolution));
    let corners: [[f64; 3]; 8] = std::array::from_fn(|index| {
        std::array::from_fn(|axis| {
            if index & (1 << axis) == 0 {
                lo[axis]
            } else {
                hi[axis]
            }
        })
    });
    let direction = match light {
        Light::Directional { direction, .. } | Light::Spot { direction, .. } => unit(direction),
        Light::Point { .. } => {
            return Err("point lights require six face projections, not one planar map".into());
        }
    };
    let up = if direction[2].abs() > 0.99 {
        [0.0, 1.0, 0.0]
    } else {
        [0.0, 0.0, -1.0]
    };
    if let Light::Spot {
        position,
        outer_angle,
        ..
    } = light
    {
        let depths = corners.map(|p| dot(direction, sub(p, position)));
        let min = depths.into_iter().fold(f64::INFINITY, f64::min);
        let max = depths.into_iter().fold(f64::NEG_INFINITY, f64::max);
        if max <= 0.0 {
            return Ok(None);
        }
        let near = (min - padding).max(0.01);
        let far = (max + padding).max(near + 0.01);
        let target = std::array::from_fn(|i| position[i] + direction[i]);
        let matrix = Camera::look_at(
            ImageExtent::new(settings.resolution, settings.resolution)?,
            position,
            target,
            up,
            2.0 * outer_angle,
            near,
            far,
        )?
        .clip_from_world()?;
        let scale = (0.5 / outer_angle.tan()) as f32;
        return Ok(Some(ShadowProjection {
            matrix,
            depth_scale: [near as f32, far as f32, scale, scale],
            point_position: None,
        }));
    }
    let right = unit(cross(direction, up));
    let down = cross(direction, right);
    let axes = [right, down, direction];
    let points = corners.map(|p| axes.map(|axis| dot(axis, sub(p, center))));
    let min: [f64; 3] = std::array::from_fn(|i| {
        points.iter().map(|p| p[i]).fold(f64::INFINITY, f64::min) - padding
    });
    let max: [f64; 3] = std::array::from_fn(|i| {
        points
            .iter()
            .map(|p| p[i])
            .fold(f64::NEG_INFINITY, f64::max)
            + padding
    });
    let span: [f64; 3] = std::array::from_fn(|i| max[i] - min[i]);
    let snapped: [f64; 2] = std::array::from_fn(|i| {
        let texel = span[i] / f64::from(settings.resolution);
        let origin = dot(axes[i], center);
        // Anchor the texel grid in world space, not at the moving bounds centre.
        (((min[i] + max[i]) * 0.5 + origin) / texel).round() * texel - origin
    });
    let scales = [2.0 / span[0], -2.0 / span[1], 1.0 / span[2]];
    let offsets = [
        -scales[0] * snapped[0],
        -scales[1] * snapped[1],
        -min[2] / span[2],
    ];
    let mut rows = [[0.0; 4]; 4];
    for i in 0..3 {
        for j in 0..3 {
            rows[i][j] = axes[i][j] * scales[i];
        }
        rows[i][3] = offsets[i] - dot(axes[i], center) * scales[i];
    }
    rows[3][3] = 1.0;
    let columns: [[f32; 4]; 4] =
        std::array::from_fn(|c| std::array::from_fn(|r| rows[r][c] as f32));
    if !columns.iter().flatten().all(|v| v.is_finite()) {
        return Err("shadow projection exceeds GPU range".into());
    }
    Ok(Some(ShadowProjection {
        matrix: columns,
        depth_scale: [
            0.0,
            span[2] as f32,
            (1.0 / span[0]) as f32,
            (1.0 / span[1]) as f32,
        ],
        point_position: None,
    }))
}

pub(crate) fn face_count(light: Light) -> usize {
    if matches!(light, Light::Point { .. }) {
        6
    } else {
        1
    }
}

pub(crate) fn projections(
    light: Light,
    bounds: Aabb,
    settings: ShadowSettings,
) -> Result<Vec<Option<ShadowProjection>>, BoxError> {
    let Light::Point { position, .. } = light else {
        return Ok(vec![projection(light, bounds, settings)?]);
    };
    // Spherical exclusion in metres; the face near plane includes its corners.
    let near: f64 = 0.01;
    let closest =
        std::array::from_fn(|axis| position[axis].clamp(bounds.min()[axis], bounds.max()[axis]));
    let delta = sub(closest, position);
    let search_near = dot(delta, delta).sqrt().max(near);
    let far = (0..8)
        .map(|corner| {
            let point = std::array::from_fn(|axis| {
                if corner & (1 << axis) == 0 {
                    bounds.min()[axis]
                } else {
                    bounds.max()[axis]
                }
            });
            let delta = sub(point, position);
            dot(delta, delta).sqrt()
        })
        .fold(near * 2.0, f64::max)
        * 1.01;
    // +X, -X, +Y, -Y, +Z, -Z; shared with cube_coordinates/cube_direction in WGSL.
    [
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, -1.0],
    ]
    .into_iter()
    .map(|direction: [f64; 3]| {
        let up = if direction[2] == 0.0 {
            [0.0, 0.0, -1.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        let matrix = Camera::look_at(
            ImageExtent::new(settings.resolution, settings.resolution)?,
            position,
            std::array::from_fn(|i| position[i] + direction[i]),
            up,
            std::f64::consts::FRAC_PI_2,
            near / 3.0_f64.sqrt(),
            far,
        )?
        .clip_from_world()?;
        Ok(Some(ShadowProjection {
            matrix,
            depth_scale: [near as f32, far as f32, search_near as f32, 0.0],
            point_position: Some(position.map(|v| v as f32)),
        }))
    })
    .collect()
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter().zip(b).map(|(x, y)| x * y).sum()
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn unit(v: [f64; 3]) -> [f64; 3] {
    let scale = v.into_iter().map(f64::abs).fold(0.0, f64::max);
    let v = v.map(|x| x / scale);
    let len = dot(v, v).sqrt();
    v.map(|x| x / len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spot_depth_metadata_reconstructs_linear_distances_at_both_clip_planes() {
        let p = projection(
            Light::Spot {
                position: [0.0; 3],
                direction: [0.0, 0.0, 1.0],
                intensity: [1.0; 3],
                inner_angle: 0.3,
                outer_angle: 0.8,
                source_radius: 0.2,
            },
            Aabb::new([-1.0, -1.0, -1.0], [1.0, 1.0, 10.0]).unwrap(),
            ShadowSettings::default(),
        )
        .unwrap()
        .unwrap();
        let [near, far, ..] = p.depth_scale;
        for distance in [near, near * 2.0, 1.0, far] {
            let depth = (p.matrix[2][2] * distance + p.matrix[3][2]) / distance;
            let reconstructed = near * far / (far - depth * (far - near));
            assert!((reconstructed - distance).abs() / distance < 0.0002);
        }
    }

    #[test]
    fn directional_texel_grid_survives_subtexel_bounds_motion() {
        let settings = ShadowSettings {
            resolution: 128,
            ..Default::default()
        };
        let light = Light::Directional {
            direction: [0.0, 0.0, 1.0],
            irradiance: [1.0; 3],
            angular_radius: 0.0,
        };
        let matrix = |offset: f64| {
            projection(
                light,
                Aabb::new([-1.0 + offset, -1.0, -1.0], [1.0 + offset, 1.0, 0.0]).unwrap(),
                settings,
            )
            .unwrap()
            .unwrap()
            .matrix
        };
        let initial = matrix(0.0);
        let shifted = matrix(0.001);
        for column in 0..4 {
            for row in 0..2 {
                assert!((initial[column][row] - shifted[column][row]).abs() < 1e-6);
            }
        }
        assert!((initial[3][0] - matrix(0.1)[3][0]).abs() > 0.01);
    }

    #[test]
    fn directional_fit_contains_the_scene_and_handles_flat_ground() {
        for bounds in [
            Aabb::new([-50.0, -30.0, -10.0], [50.0, 30.0, 0.0]).unwrap(),
            Aabb::new([-1.0, -1.0, 0.0], [1.0, 1.0, 0.0]).unwrap(),
        ] {
            let matrix = projection(
                Light::Directional {
                    direction: [0.2, -0.4, 1.0],
                    irradiance: [1.0; 3],
                    angular_radius: 0.0,
                },
                bounds,
                ShadowSettings::default(),
            )
            .unwrap()
            .unwrap()
            .matrix;
            for corner in 0..8 {
                let point: [f64; 4] = std::array::from_fn(|i| {
                    if i == 3 {
                        1.0
                    } else if corner & (1 << i) == 0 {
                        bounds.min()[i]
                    } else {
                        bounds.max()[i]
                    }
                });
                let clip: [f64; 4] = std::array::from_fn(|r| {
                    (0..4).map(|c| f64::from(matrix[c][r]) * point[c]).sum()
                });
                assert!(clip[0].abs() < 1.0 && clip[1].abs() < 1.0);
                assert!((0.0..1.0).contains(&clip[2]));
            }
        }
    }
}
