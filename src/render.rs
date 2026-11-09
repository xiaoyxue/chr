use crate::{gpu::Gpu, media::Media, scene::Scene};
use anyhow::{Result, ensure};
use bytemuck::{Pod, Zeroable};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Quad {
    rows: [[f32; 4]; 3],
    metadata: [u32; 4],
    bounds: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Parameters {
    width: u32,
    height: u32,
    source: u32,
    sequence: u32,
    opacity: f32,
    color: u32,
    timecode_overlay: u32,
    reserved: u32,
}

pub struct Renderer {
    pub y: wgpu::Texture,
    pub uv: wgpu::Texture,
    pub packed: wgpu::Buffer,
    parameters: wgpu::Buffer,
    quads: wgpu::Buffer,
    compose: wgpu::ComputePipeline,
    pack: wgpu::ComputePipeline,
    compose_bindings: wgpu::BindGroup,
    pack_bindings: wgpu::BindGroup,
    _assets: Vec<wgpu::Texture>,
}

pub struct RenderFrame {
    pub ordinal: usize,
    pub sequence: u64,
    pub color_mode: u32,
    pub overlay: bool,
    pub bypass: bool,
}

fn texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    layers: u32,
    levels: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("chr GPU image"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: layers,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn pipeline(
    device: &wgpu::Device,
    label: &'static str,
    code: &'static str,
) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(code.into()),
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    })
}

impl Renderer {
    pub fn new(gpu: &Gpu, scene: &Scene) -> Result<Self> {
        let device = &gpu.device;
        ensure!(
            scene.frames.len() <= device.limits().max_texture_array_layers as usize,
            "Mask array exceeds the adapter layer limit"
        );
        let sampled = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        let y = texture(
            device,
            scene.width,
            scene.height,
            1,
            1,
            wgpu::TextureFormat::R8Unorm,
            sampled,
        );
        let uv = texture(
            device,
            scene.width / 2,
            scene.height / 2,
            1,
            1,
            wgpu::TextureFormat::Rg8Unorm,
            sampled,
        );
        let rgba = texture(
            device,
            scene.width,
            scene.height,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let masks = texture(
            device,
            scene.width,
            scene.height,
            scene.frames.len() as u32,
            1,
            wgpu::TextureFormat::R8Unorm,
            sampled,
        );
        let mut mask_file = File::open(&scene.mask_path)?;
        let mut mask = vec![0; (scene.width * scene.height) as usize];
        for layer in 0..scene.frames.len() as u32 {
            mask_file.read_exact(&mut mask)?;
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &masks,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &mask,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(scene.width),
                    rows_per_image: Some(scene.height),
                },
                wgpu::Extent3d {
                    width: scene.width,
                    height: scene.height,
                    depth_or_array_layers: 1,
                },
            );
            if layer % 16 == 15 {
                gpu.queue.submit([]);
                gpu.wait()?;
            }
        }
        let mut logos = Vec::new();
        for artwork in &scene.textures {
            let first = &artwork.levels[0];
            let image = texture(
                device,
                first.width,
                first.height,
                1,
                artwork.levels.len() as u32,
                wgpu::TextureFormat::Rgba32Float,
                sampled,
            );
            let mut file = File::open(&artwork.path)?;
            for (index, level) in artwork.levels.iter().enumerate() {
                let mut bytes = vec![0; (level.width * level.height * 16) as usize];
                file.seek(SeekFrom::Start(level.offset))?;
                file.read_exact(&mut bytes)?;
                gpu.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &image,
                        mip_level: index as u32,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(level.width * 16),
                        rows_per_image: Some(level.height),
                    },
                    wgpu::Extent3d {
                        width: level.width,
                        height: level.height,
                        depth_or_array_layers: 1,
                    },
                );
            }
            logos.push(image);
            gpu.queue.submit([]);
            gpu.wait()?;
        }
        let parameters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame metadata"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let quads = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame quad transforms"),
            size: 6 * std::mem::size_of::<Quad>() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let packed = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GPU-only packed NV12 output"),
            size: u64::from(scene.width) * u64::from(scene.height) * 3 / 2,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let compose = pipeline(
            device,
            "projective logo compositor",
            include_str!("compose.wgsl"),
        );
        let pack = pipeline(device, "GPU NV12 packing", include_str!("pack.wgsl"));
        let y_view = y.create_view(&Default::default());
        let uv_view = uv.create_view(&Default::default());
        let rgba_view = rgba.create_view(&Default::default());
        let mask_view = masks.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let logo_views: Vec<_> = logos
            .iter()
            .map(|image| image.create_view(&Default::default()))
            .collect();
        let compose_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compositor resources"),
            layout: &compose.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uv_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&rgba_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&logo_views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&logo_views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&logo_views[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: quads.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: parameters.as_entire_binding(),
                },
            ],
        });
        let pack_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("NV12 packing resources"),
            layout: &pack.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&rgba_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&uv_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: packed.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: parameters.as_entire_binding(),
                },
            ],
        });
        let mut initial = device.create_command_encoder(&Default::default());
        initial.clear_texture(&y, &Default::default());
        initial.clear_texture(&uv, &Default::default());
        // Consume first-use initialization before native Vulkan writes video pixels.
        // Zero-initialized dimensions make this dispatch a no-op on frame data.
        {
            let mut pass = initial.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pack);
            pass.set_bind_group(0, &pack_bindings, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        initial.transition_resources(
            std::iter::empty(),
            [&y, &uv]
                .into_iter()
                .map(|texture| wgpu::TextureTransition {
                    texture,
                    selector: None,
                    state: wgpu::TextureUses::COPY_DST,
                }),
        );
        gpu.queue.submit([initial.finish()]);
        gpu.wait()?;
        logos.extend([masks, rgba]);
        Ok(Self {
            y,
            uv,
            packed,
            parameters,
            quads,
            compose,
            pack,
            compose_bindings,
            pack_bindings,
            _assets: logos,
        })
    }

    pub fn render(
        &self,
        gpu: &Gpu,
        media: &Media<'_>,
        scene: &Scene,
        frame: RenderFrame,
    ) -> Result<()> {
        let parameters = Parameters {
            width: scene.width,
            height: scene.height,
            source: frame.ordinal as u32,
            sequence: (frame.sequence % (24 * 60 * 60 * 30)) as u32,
            opacity: if frame.bypass { 0.0 } else { scene.opacity },
            color: frame.color_mode,
            timecode_overlay: u32::from(frame.overlay),
            reserved: 0,
        };
        let quads: Vec<_> = scene.frames[frame.ordinal]
            .quads
            .iter()
            .map(|quad| {
                let mut rows = [[0.0; 4]; 3];
                for (row, values) in rows.iter_mut().zip(quad.inverse.chunks_exact(3)) {
                    row[..3].copy_from_slice(values);
                }
                Quad {
                    rows,
                    metadata: [quad.texture, u32::from(quad.valid), 0, 0],
                    bounds: quad.bounds.map(|value| value.max(0) as u32),
                }
            })
            .collect();
        gpu.queue
            .write_buffer(&self.parameters, 0, bytemuck::bytes_of(&parameters));
        gpu.queue
            .write_buffer(&self.quads, 0, bytemuck::cast_slice(&quads));
        let mut command = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = command.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.compose);
            pass.set_bind_group(0, &self.compose_bindings, &[]);
            pass.dispatch_workgroups(scene.width.div_ceil(8), scene.height.div_ceil(8), 1);
        }
        {
            let mut pass = command.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pack);
            pass.set_bind_group(0, &self.pack_bindings, &[]);
            pass.dispatch_workgroups(
                (scene.width / 4).div_ceil(8),
                (scene.height / 2).div_ceil(8),
                1,
            );
        }
        command.transition_resources(
            [wgpu::BufferTransition {
                buffer: &self.packed,
                state: wgpu::BufferUses::COPY_SRC,
            }]
            .into_iter(),
            [&self.y, &self.uv]
                .into_iter()
                .map(|texture| wgpu::TextureTransition {
                    texture,
                    selector: None,
                    state: wgpu::TextureUses::COPY_DST,
                }),
        );
        media.submit(gpu, command.finish());
        gpu.wait()
    }
}
