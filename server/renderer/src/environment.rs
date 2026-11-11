use std::{num::NonZeroU64, sync::Arc};
use wgpu::util::DeviceExt;

use crate::{
    Gpu,
    camera::Camera,
    offscreen::BoxError,
    scene::{EnvironmentMap, SkyLight},
};

const ROUGHNESS_LAYERS: u32 = 6;
const LUT_SIZE: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvironmentSettings {
    /// Width of each 2:1 roughness-prefiltered panorama; power of two, 64..=1024.
    pub resolution: u32,
    /// Deterministic GGX samples per prefiltered texel, 1..=1024.
    pub samples: u32,
    pub max_bytes: u64,
}

impl Default for EnvironmentSettings {
    fn default() -> Self {
        Self {
            resolution: 256,
            samples: 64,
            max_bytes: 256 * 1024 * 1024,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EnvironmentUniform {
    sh: [[f32; 4]; 9],
    info: [f32; 4],
    flags: [f32; 4],
    rays: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BakeUniform {
    roughness: f32,
    samples: u32,
    kind: u32,
    padding: u32,
}

struct CachedEnvironment {
    source: Arc<EnvironmentMap>,
    resolution: u32,
    samples: u32,
    scale: f32,
    sh: [[f32; 4]; 9],
}

pub(crate) struct EnvironmentGpu {
    pub source: wgpu::TextureView,
    pub specular: wgpu::TextureView,
    pub lut: wgpu::TextureView,
    pub uniform: wgpu::Buffer,
    pub visible: bool,
    pub preprocessed: bool,
    cache: Option<CachedEnvironment>,
    lut_ready: bool,
    bake_layout: wgpu::BindGroupLayout,
    bake_pipeline: wgpu::ComputePipeline,
}

pub(crate) fn shader_source(main: &str) -> wgpu::ShaderSource<'static> {
    wgpu::ShaderSource::Wgsl(
        format!("{}\n{main}", include_str!("environment_sampling.wgsl")).into(),
    )
}

pub(crate) fn texture_entry(
    binding: u32,
    array: bool,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: if array {
                wgpu::TextureViewDimension::D2Array
            } else {
                wgpu::TextureViewDimension::D2
            },
            multisampled: false,
        },
        count: None,
    }
}

pub(crate) fn layout_entries() -> [wgpu::BindGroupLayoutEntry; 4] {
    [
        texture_entry(3, false, wgpu::ShaderStages::FRAGMENT),
        texture_entry(4, true, wgpu::ShaderStages::FRAGMENT),
        texture_entry(5, false, wgpu::ShaderStages::FRAGMENT),
        wgpu::BindGroupLayoutEntry {
            binding: 6,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: NonZeroU64::new(std::mem::size_of::<EnvironmentUniform>() as u64),
            },
            count: None,
        },
    ]
}

impl EnvironmentGpu {
    pub fn new(device: &wgpu::Device) -> Self {
        let placeholder = texture(device, 1, 1, 1, "empty environment");
        let bake_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("environment prefilter"),
            entries: &[
                texture_entry(0, false, wgpu::ShaderStages::COMPUTE),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(16),
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("environment prefilter"),
            bind_group_layouts: &[Some(&bake_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("environment prefilter"),
            source: shader_source(include_str!("environment_bake.wgsl")),
        });
        let bake_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("GGX environment prefilter"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            source: placeholder.create_view(&Default::default()),
            specular: placeholder.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            }),
            lut: placeholder.create_view(&Default::default()),
            uniform: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("environment state"),
                size: std::mem::size_of::<EnvironmentUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            visible: false,
            preprocessed: false,
            cache: None,
            lut_ready: false,
            bake_layout,
            bake_pipeline,
        }
    }

    pub fn entries(&self) -> [wgpu::BindGroupEntry<'_>; 4] {
        [
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&self.source),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&self.specular),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(&self.lut),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: self.uniform.as_entire_binding(),
            },
        ]
    }

    pub fn prepare(
        &mut self,
        gpu: &Gpu,
        sky: Option<&SkyLight>,
        camera: &Camera,
        settings: EnvironmentSettings,
    ) -> Result<bool, BoxError> {
        self.preprocessed = false;
        self.visible = sky.is_some_and(|sky| sky.visible);
        let mut data = EnvironmentUniform::zeroed();
        let mut changed = false;
        if let Some(sky) = sky {
            let extent = sky.environment.extent();
            if !(64..=1024).contains(&settings.resolution)
                || !settings.resolution.is_power_of_two()
                || !(1..=1024).contains(&settings.samples)
            {
                return Err("invalid environment prefilter quality".into());
            }
            if extent.width().max(extent.height()).max(settings.resolution)
                > gpu.device.limits().max_texture_dimension_2d
            {
                return Err("environment exceeds texture limits".into());
            }
            let bytes = (u64::from(extent.width()) * u64::from(extent.height())
                + u64::from(settings.resolution)
                    * u64::from(settings.resolution / 2)
                    * u64::from(ROUGHNESS_LAYERS)
                + u64::from(LUT_SIZE) * u64::from(LUT_SIZE))
                * 16;
            if bytes > settings.max_bytes {
                return Err("environment exceeds the configured memory budget".into());
            }
            if self.cache.as_ref().is_none_or(|cache| {
                !Arc::ptr_eq(&cache.source, &sky.environment)
                    || cache.resolution != settings.resolution
                    || cache.samples != settings.samples
            }) {
                self.rebuild(gpu, Arc::clone(&sky.environment), settings);
                changed = true;
                self.preprocessed = true;
            }
            let cache = self.cache.as_ref().expect("prepared environment");
            data.sh = cache.sh;
            data.info = [
                sky.gain,
                sky.rotation.cos() as f32,
                sky.rotation.sin() as f32,
                (ROUGHNESS_LAYERS - 1) as f32,
            ];
            data.flags = [1.0, cache.scale, u32::from(sky.visible) as f32, 0.0];
            let extent = camera.extent();
            let k = camera.intrinsics();
            let forward = camera.ray([k.cx, k.cy])?.direction;
            let ray = |pixel| -> Result<[f64; 3], BoxError> {
                let direction = camera.ray(pixel)?.direction;
                let depth: f64 = direction.iter().zip(forward).map(|(a, b)| a * b).sum();
                Ok(direction.map(|v| v / depth))
            };
            let corner = ray([0.0, 0.0])?;
            let right = ray([f64::from(extent.width()), 0.0])?;
            let bottom = ray([0.0, f64::from(extent.height())])?;
            for i in 0..3 {
                data.rays[0][i] = corner[i] as f32;
                data.rays[1][i] = ((right[i] - corner[i]) / f64::from(extent.width())) as f32;
                data.rays[2][i] = ((bottom[i] - corner[i]) / f64::from(extent.height())) as f32;
            }
            if !data.rays.iter().flatten().all(|v| v.is_finite()) {
                return Err("sky rays exceed GPU range".into());
            }
        } else if self.cache.take().is_some() {
            let empty = texture(&gpu.device, 1, 1, 1, "empty environment");
            self.source = empty.create_view(&Default::default());
            self.specular = empty.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            });
            changed = true;
        }
        gpu.queue
            .write_buffer(&self.uniform, 0, bytemuck::bytes_of(&data));
        Ok(changed)
    }

    fn rebuild(&mut self, gpu: &Gpu, source: Arc<EnvironmentMap>, settings: EnvironmentSettings) {
        let scale = source
            .pixels()
            .iter()
            .flatten()
            .copied()
            .fold(1.0_f32, f32::max);
        let pixels: Vec<[f32; 4]> = source
            .pixels()
            .iter()
            .map(|p| [p[0] / scale, p[1] / scale, p[2] / scale, 1.0])
            .collect();
        let extent = source.extent();
        let input = texture(
            &gpu.device,
            extent.width(),
            extent.height(),
            1,
            "scene-linear environment",
        );
        gpu.queue.write_texture(
            input.as_image_copy(),
            bytemuck::cast_slice(&pixels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(extent.width() * 16),
                rows_per_image: Some(extent.height()),
            },
            input.size(),
        );
        self.source = input.create_view(&Default::default());
        let filtered = texture(
            &gpu.device,
            settings.resolution,
            settings.resolution / 2,
            ROUGHNESS_LAYERS,
            "GGX roughness layers",
        );
        self.specular = filtered.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        for layer in 0..ROUGHNESS_LAYERS {
            let view = filtered.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer,
                array_layer_count: Some(1),
                ..Default::default()
            });
            self.bake(
                &gpu.device,
                &mut encoder,
                &view,
                settings.resolution,
                settings.resolution / 2,
                BakeUniform {
                    roughness: layer as f32 / (ROUGHNESS_LAYERS - 1) as f32,
                    samples: settings.samples,
                    kind: 0,
                    padding: 0,
                },
            );
        }
        if !self.lut_ready {
            let lut = texture(&gpu.device, LUT_SIZE, LUT_SIZE, 1, "GGX split-sum BRDF");
            self.lut = lut.create_view(&Default::default());
            self.bake(
                &gpu.device,
                &mut encoder,
                &self.lut,
                LUT_SIZE,
                LUT_SIZE,
                BakeUniform {
                    roughness: 0.0,
                    samples: 256,
                    kind: 1,
                    padding: 0,
                },
            );
            self.lut_ready = true;
        }
        gpu.queue.submit([encoder.finish()]);
        self.cache = Some(CachedEnvironment {
            sh: diffuse_sh(&source, scale),
            source,
            resolution: settings.resolution,
            samples: settings.samples,
            scale,
        });
    }

    fn bake(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        width: u32,
        height: u32,
        params: BakeUniform,
    ) {
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("prefilter parameters"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("environment bake"),
            layout: &self.bake_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(target),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&self.bake_pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
    }
}

fn texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    layers: u32,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

// Integrate each lat-long pixel's SH basis exactly; even a 1x1 constant map stays isotropic.
fn diffuse_sh(source: &EnvironmentMap, scale: f32) -> [[f32; 4]; 9] {
    use std::f64::consts::PI;
    let width = source.extent().width() as usize;
    let height = source.extent().height() as usize;
    let mut sh = [[0.0_f64; 3]; 9];
    let phi: Vec<_> = (0..width)
        .map(|x| {
            let a = 2.0 * PI * x as f64 / width as f64 - PI;
            let b = 2.0 * PI * (x + 1) as f64 / width as f64 - PI;
            [
                b - a,
                b.sin() - a.sin(),
                a.cos() - b.cos(),
                0.5 * (b.sin().powi(2) - a.sin().powi(2)),
                0.5 * ((2.0 * b).sin() - (2.0 * a).sin()),
            ]
        })
        .collect();
    for y in 0..height {
        let a = PI * y as f64 / height as f64;
        let b = PI * (y + 1) as f64 / height as f64;
        let c0 = a.cos();
        let c1 = b.cos();
        let s2 = (b - a) * 0.5 - ((2.0 * b).sin() - (2.0 * a).sin()) * 0.25;
        let s3 = c0 - c1 + (c1.powi(3) - c0.powi(3)) / 3.0;
        let zs2 = (a.sin().powi(3) - b.sin().powi(3)) / 3.0;
        for (x, p) in phi.iter().enumerate() {
            let basis = [
                0.2820947918 * p[0] * (c0 - c1),
                0.4886025119 * p[1] * s2,
                0.4886025119 * p[2] * s2,
                0.4886025119 * p[0] * (c1 * c1 - c0 * c0) * 0.5,
                1.0925484306 * p[3] * s3,
                1.0925484306 * p[2] * zs2,
                1.0925484306 * p[1] * zs2,
                0.3153915653 * p[0] * (c0.powi(3) - c1.powi(3) - c0 + c1),
                0.5462742153 * p[4] * s3,
            ];
            for (coefficient, value) in sh.iter_mut().zip(basis) {
                for (channel, radiance) in
                    coefficient.iter_mut().zip(source.pixels()[y * width + x])
                {
                    *channel += value * f64::from(radiance) / f64::from(scale);
                }
            }
        }
    }
    std::array::from_fn(|i| {
        let kernel = if i == 0 {
            1.0
        } else if i < 4 {
            2.0 / 3.0
        } else {
            0.25
        };
        [
            (sh[i][0] * kernel) as f32,
            (sh[i][1] * kernel) as f32,
            (sh[i][2] * kernel) as f32,
            0.0,
        ]
    })
}

use bytemuck::Zeroable;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinates::ImageExtent;

    #[test]
    fn exact_pixel_sh_integrals_preserve_constant_hdr_at_any_resolution() {
        for (width, height) in [(1, 1), (2, 1), (4, 2), (64, 32)] {
            let source = EnvironmentMap::new(
                ImageExtent::new(width, height).unwrap(),
                vec![[4.0, 2.0, 1.0]; (width * height) as usize],
            )
            .unwrap();
            let sh = diffuse_sh(&source, 4.0);
            for (value, expected) in sh[0][..3].iter().zip([4.0, 2.0, 1.0]) {
                assert!((value * 0.2820948 * 4.0 - expected).abs() < 1e-5);
            }
            assert!(
                sh[1..].iter().flatten().all(|value| value.abs() < 1e-6),
                "constant radiance cannot acquire directional SH terms"
            );
        }
    }
}
