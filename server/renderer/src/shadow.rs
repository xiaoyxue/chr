use crate::{
    camera::{Camera, CameraView},
    coordinates::ImageExtent,
    offscreen::BoxError,
    scene::{Aabb, AreaShape, Light, SceneView},
};

pub use crate::scene::ShadowMode;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CascadeSettings {
    /// One keeps the scene-fitted map; 2..=4 enable view-frustum cascades.
    pub count: u32,
    /// Maximum optical depth in world metres; ignored for the single-map path.
    pub max_distance: f64,
    /// Linear/logarithmic split interpolation in [0, 1].
    pub split_lambda: f64,
    /// Fraction of each interval used to blend into the next (or fade to lit).
    pub blend_fraction: f64,
}

impl Default for CascadeSettings {
    fn default() -> Self {
        Self {
            count: 1,
            max_distance: 200.0,
            split_lambda: 0.5,
            blend_fraction: 0.1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PcssSettings {
    /// Each stage supports 1..=64 deterministic disk samples.
    pub blocker_samples: u32,
    pub filter_samples: u32,
    /// Independent search/filter caps in shadow texels, not physical source sizes.
    pub max_search_radius: f32,
    pub max_filter_radius: f32,
}

impl Default for PcssSettings {
    fn default() -> Self {
        Self {
            blocker_samples: 32,
            filter_samples: 32,
            max_search_radius: 64.0,
            max_filter_radius: 64.0,
        }
    }
}

impl PcssSettings {
    pub(crate) fn validate(self) -> Result<(), BoxError> {
        if !(1..=64).contains(&self.blocker_samples)
            || !(1..=64).contains(&self.filter_samples)
            || [self.max_search_radius, self.max_filter_radius]
                .into_iter()
                .any(|radius| !radius.is_finite() || radius <= 0.0)
        {
            return Err("invalid PCSS sample counts or radius limits".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowSettings {
    /// Default for lights whose shadow_mode is None.
    pub mode: ShadowMode,
    pub resolution: u32,
    /// Half-width in shadow texels of the square PCF footprint.
    pub filter_radius: f32,
    /// Square kernel: 1, 4, 9, 16, 25, 36, 49 or 64 bilinear comparisons.
    pub pcf_samples: u32,
    /// Bias in normalized shadow depth.
    pub depth_bias: f32,
    /// Receiver offset along its unperturbed surface normal, in world metres.
    pub normal_bias: f32,
    pub max_bytes: u64,
    /// Worst-case logical shadow samples across effective Hard, PCF and PCSS lights.
    pub max_samples_per_fragment: u64,
    pub pcss: PcssSettings,
    pub cascades: CascadeSettings,
}

impl Default for ShadowSettings {
    fn default() -> Self {
        Self {
            mode: ShadowMode::Off,
            resolution: 2048,
            filter_radius: 1.5,
            pcf_samples: 9,
            depth_bias: 0.0005,
            normal_bias: 0.02,
            max_bytes: 256 * 1024 * 1024,
            max_samples_per_fragment: 1024,
            pcss: PcssSettings::default(),
            cascades: CascadeSettings::default(),
        }
    }
}

impl ShadowSettings {
    pub(crate) fn validate(self) -> Result<(), BoxError> {
        if !(1..=4).contains(&self.cascades.count)
            || !self.cascades.max_distance.is_finite()
            || self.cascades.max_distance <= 0.0
            || !(0.0..=1.0).contains(&self.cascades.split_lambda)
            || !(0.0..=0.5).contains(&self.cascades.blend_fraction)
        {
            return Err("invalid directional cascade settings".into());
        }
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
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct ShadowProjection {
    pub matrix: [[f32; 4]; 4],
    /// Near/far and planar UV scales; point maps use z for the nearest scene-bound radius.
    pub depth_scale: [f32; 4],
    pub point_position: Option<[f32; 3]>,
    pub split: [f32; 2],
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
        Light::Area { .. } => {
            return Err("area lights require six face projections, not one planar map".into());
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
            split: [0.0; 2],
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
        split: [0.0; 2],
    }))
}

pub(crate) fn face_count(light: Light, cascades: CascadeSettings) -> usize {
    match light {
        Light::Point { .. } => 6,
        Light::Directional { .. } => cascades.count as usize,
        Light::Spot { .. } => 1,
        Light::Area { .. } => 6,
    }
}

pub(crate) fn cascade_padding(settings: ShadowSettings) -> f32 {
    settings
        .filter_radius
        .max(settings.pcss.max_search_radius)
        .max(settings.pcss.max_filter_radius)
}

fn directional_cascades(
    light: Light,
    bounds: Aabb,
    settings: ShadowSettings,
    camera: &dyn CameraView,
) -> Result<Vec<Option<ShadowProjection>>, BoxError> {
    let [near, camera_far] = camera.depth_range();
    let far = camera_far.min(settings.cascades.max_distance);
    if far <= near {
        return Err("cascade distance must exceed the camera near plane".into());
    }
    let extent = camera.extent();
    let forward = camera.forward();
    let rays = [
        [0.0, 0.0],
        [f64::from(extent.width()), 0.0],
        [0.0, f64::from(extent.height())],
        [f64::from(extent.width()), f64::from(extent.height())],
    ]
    .map(|pixel| camera.ray(pixel))
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    let Light::Directional { direction, .. } = light else {
        return Err("cascades require a directional light".into());
    };
    let direction = unit(direction);
    let up = if direction[2].abs() > 0.99 {
        [0.0, 1.0, 0.0]
    } else {
        [0.0, 0.0, -1.0]
    };
    let right = unit(cross(direction, up));
    let axes = [right, cross(direction, right)];
    let mut result = Vec::new();
    let mut previous = near;
    let mut previous_begin = near;
    for index in 0..settings.cascades.count {
        let fraction = f64::from(index + 1) / f64::from(settings.cascades.count);
        let split = settings.cascades.split_lambda * near * (far / near).powf(fraction)
            + (1.0 - settings.cascades.split_lambda) * (near + (far - near) * fraction);
        let begin = previous - settings.cascades.blend_fraction * (previous - previous_begin);
        let corners: [[f64; 3]; 8] = std::array::from_fn(|i| {
            let ray = rays[i % 4];
            let depth = if i < 4 { begin } else { split };
            let distance = depth / dot(ray.direction, forward);
            std::array::from_fn(|axis| ray.origin[axis] + ray.direction[axis] * distance)
        });
        let center = std::array::from_fn(|axis| corners.iter().map(|p| p[axis]).sum::<f64>() / 8.0);
        let radius = corners
            .iter()
            .map(|p| {
                let d = sub(*p, center);
                dot(d, d).sqrt()
            })
            .fold(0.0, f64::max)
            * (1.0
                + 2.0 * (f64::from(cascade_padding(settings)) + 1.0)
                    / f64::from(settings.resolution));
        // A frustum-slice sphere keeps its extent under rotation; snap its world-space centre.
        let texel = 2.0 * radius / f64::from(settings.resolution);
        let Some(mut map) = projection(light, bounds, settings)? else {
            unreachable!()
        };
        for (row, axis) in axes.iter().enumerate() {
            let scale = if row == 0 {
                1.0 / radius
            } else {
                -1.0 / radius
            };
            let snapped = (dot(*axis, center) / texel).round() * texel;
            for (column, value) in axis.iter().enumerate() {
                map.matrix[column][row] = (value * scale) as f32;
            }
            map.matrix[3][row] = (-snapped * scale) as f32;
        }
        // Retain the full scene's light-depth interval so offscreen casters are not clipped.
        map.depth_scale[2] = (0.5 / radius) as f32;
        map.depth_scale[3] = (0.5 / radius) as f32;
        map.split = [previous as f32, split as f32];
        if !map
            .matrix
            .iter()
            .flatten()
            .chain(map.depth_scale.iter())
            .all(|v| v.is_finite())
        {
            return Err("cascade projection exceeds GPU range".into());
        }
        result.push(Some(map));
        previous_begin = previous;
        previous = split;
    }
    Ok(result)
}

pub(crate) fn projections(
    light: Light,
    bounds: Aabb,
    settings: ShadowSettings,
    camera: &dyn CameraView,
) -> Result<Vec<Option<ShadowProjection>>, BoxError> {
    if matches!(light, Light::Directional { .. }) && settings.cascades.count > 1 {
        return directional_cascades(light, bounds, settings, camera);
    }
    // Spherical exclusion in metres; the face near plane includes its corners. A sphere
    // emitter excludes its own volume so an emitter mesh casts no shadow. Planar emitters
    // keep the small exclusion: occluders close in front of them still cast shadows,
    // and a mesh in the emitter plane does not block the directions it emits into.
    let (position, near) = match light {
        Light::Point { position, .. } => (position, 0.01),
        Light::Area {
            position,
            shape: AreaShape::Sphere { radius },
            ..
        } => (position, f64::from(radius).max(0.01)),
        Light::Area { position, .. } => (position, 0.01),
        _ => return Ok(vec![projection(light, bounds, settings)?]),
    };
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
            split: [0.0; 2],
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
    fn orthographic_cascades_cover_parallel_ray_origins_and_overlap() {
        use crate::camera::OrthographicCamera;
        let camera = OrthographicCamera::from_pose(
            ImageExtent::new(640, 480).unwrap(),
            [40.0, 20.0],
            [
                [1.0, 0.0, 0.0, 5.0],
                [0.0, 1.0, 0.0, 2.0],
                [0.0, 0.0, 1.0, -10.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            2.0,
            42.0,
        )
        .unwrap();
        let settings = ShadowSettings {
            cascades: CascadeSettings {
                count: 4,
                max_distance: 42.0,
                split_lambda: 0.0,
                blend_fraction: 0.1,
            },
            ..Default::default()
        };
        let maps = projections(
            Light::Directional {
                direction: [0.0, 0.0, 1.0],
                irradiance: [1.0; 3],
                angular_radius: 0.0,
                shadow_mode: None,
            },
            Aabb::new([-30.0, -30.0, -10.0], [30.0, 30.0, 40.0]).unwrap(),
            settings,
            &camera,
        )
        .unwrap();
        let mut previous_begin = 2.0;
        for (index, map) in maps.into_iter().enumerate() {
            let map = map.unwrap();
            assert_eq!(map.split[1], 12.0 + index as f32 * 10.0);
            let begin = f64::from(map.split[0]) - 0.1 * (f64::from(map.split[0]) - previous_begin);
            for depth in [begin, f64::from(map.split[1])] {
                for pixel in [[0.0, 0.0], [640.0, 0.0], [0.0, 480.0], [640.0, 480.0]] {
                    let ray = camera.ray(pixel).unwrap();
                    let point = [ray.origin[0], ray.origin[1], ray.origin[2] + depth, 1.0];
                    for row in 0..2 {
                        let clip: f64 = (0..4)
                            .map(|c| f64::from(map.matrix[c][row]) * point[c])
                            .sum();
                        assert!(
                            clip.abs() < 1.0,
                            "all parallel corner origins must be covered"
                        );
                    }
                }
            }
            previous_begin = f64::from(map.split[0]);
        }
    }

    #[test]
    fn cascades_cover_overlapping_slices_and_keep_world_texels_stable() {
        let extent = ImageExtent::new(640, 480).unwrap();
        let camera = Camera::look_at(
            extent,
            [0.0, 20.0, -4.0],
            [0.0, 0.0, -4.0],
            [0.0, 0.0, -1.0],
            1.0,
            0.1,
            100.0,
        )
        .unwrap();
        let moved = Camera::look_at(
            extent,
            [0.000001, 20.0, -4.0],
            [0.000001, 0.0, -4.0],
            [0.0, 0.0, -1.0],
            1.0,
            0.1,
            100.0,
        )
        .unwrap();
        let light = Light::Directional {
            direction: [0.2, -0.4, 1.0],
            irradiance: [1.0; 3],
            angular_radius: 0.01,
            shadow_mode: None,
        };
        let bounds = Aabb::new([-30.0, -30.0, -10.0], [30.0, 30.0, 0.0]).unwrap();
        let settings = ShadowSettings {
            cascades: CascadeSettings {
                count: 4,
                max_distance: 80.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let maps = projections(light, bounds, settings, &camera).unwrap();
        let shifted = projections(light, bounds, settings, &moved).unwrap();
        let forward = camera
            .ray([camera.intrinsics().cx, camera.intrinsics().cy])
            .unwrap()
            .direction;
        let mut previous_begin = 0.1;
        for (map, shifted) in maps.iter().zip(shifted) {
            let map = map.unwrap();
            let shifted = shifted.unwrap();
            for (a, b) in map
                .matrix
                .iter()
                .flatten()
                .zip(shifted.matrix.iter().flatten())
            {
                assert!(
                    (a - b).abs() < 1e-6,
                    "subtexel motion should preserve the snapped grid"
                );
            }
            let begin = f64::from(map.split[0])
                - settings.cascades.blend_fraction * (f64::from(map.split[0]) - previous_begin);
            for depth in [begin, f64::from(map.split[1])] {
                for pixel in [[0.0, 0.0], [640.0, 0.0], [0.0, 480.0], [640.0, 480.0]] {
                    let ray = camera.ray(pixel).unwrap();
                    let distance = depth / dot(ray.direction, forward);
                    let point = std::array::from_fn::<_, 4, _>(|i| {
                        if i == 3 {
                            1.0
                        } else {
                            ray.origin[i] + ray.direction[i] * distance
                        }
                    });
                    for row in 0..2 {
                        let clip: f64 = (0..4)
                            .map(|c| f64::from(map.matrix[c][row]) * point[c])
                            .sum();
                        assert!(
                            clip.abs() < 1.0,
                            "cascade must include its overlap and frustum corners"
                        );
                    }
                }
            }
            previous_begin = f64::from(map.split[0]);
        }
        let invalid = ShadowSettings {
            cascades: CascadeSettings {
                count: 4,
                max_distance: 0.05,
                ..Default::default()
            },
            ..settings
        };
        assert!(projections(light, bounds, invalid, &camera).is_err());
    }

    #[test]
    fn area_cubemaps_exclude_only_sphere_emitter_volumes() {
        let camera = Camera::look_at(
            ImageExtent::new(8, 8).unwrap(),
            [0.0, -10.0, -5.0],
            [0.0; 3],
            [0.0, 0.0, -1.0],
            1.0,
            0.1,
            100.0,
        )
        .unwrap();
        let bounds = Aabb::new([-5.0, -5.0, -1.0], [5.0, 5.0, 0.0]).unwrap();
        for (shape, exclusion) in [
            (AreaShape::Sphere { radius: 0.8 }, 0.8),
            (
                AreaShape::Rectangle {
                    half_width: 2.0,
                    half_height: 0.25,
                },
                0.01,
            ),
            (AreaShape::Disk { radius: 1.0 }, 0.01),
        ] {
            let light = Light::Area {
                position: [0.0, 0.0, -3.0],
                normal: [0.0, 0.0, 1.0],
                tangent: [1.0, 0.0, 0.0],
                shape,
                radiance: [1.0; 3],
                two_sided: false,
                shadow_mode: None,
            };
            assert_eq!(face_count(light, CascadeSettings::default()), 6);
            let faces = projections(light, bounds, ShadowSettings::default(), &camera).unwrap();
            assert_eq!(faces.len(), 6);
            for face in faces {
                let face = face.unwrap();
                assert!((f64::from(face.depth_scale[0]) - exclusion).abs() < 1e-6);
                assert_eq!(face.point_position, Some([0.0, 0.0, -3.0]));
            }
        }
    }

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
                shadow_mode: None,
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
            shadow_mode: None,
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
                    shadow_mode: None,
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
