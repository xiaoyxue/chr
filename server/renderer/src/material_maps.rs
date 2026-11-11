use crate::{
    Gpu,
    coordinates::ImageExtent,
    offscreen::BoxError,
    scene::{Filter, MaterialId, MaterialMaps, TextureData, Wrap},
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

type TextureKey = (usize, bool);
struct BoundTexture {
    _source: Arc<TextureData>,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}
struct BoundMaterial {
    keys: [TextureKey; 5],
    group: wgpu::BindGroup,
}

pub(crate) struct MaterialMapCache {
    pub layout: wgpu::BindGroupLayout,
    textures: HashMap<TextureKey, BoundTexture>,
    materials: HashMap<MaterialId, BoundMaterial>,
    white: Arc<TextureData>,
    mip_layout: wgpu::BindGroupLayout,
    mip_srgb: wgpu::RenderPipeline,
    mip_linear: wgpu::RenderPipeline,
    mip_sampler: wgpu::Sampler,
}

impl MaterialMapCache {
    pub fn new(device: &wgpu::Device) -> Self {
        let entries: Vec<_> = (0..5)
            .flat_map(|i| {
                [
                    texture_entry(i * 2),
                    wgpu::BindGroupLayoutEntry {
                        binding: i * 2 + 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ]
            })
            .collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("material maps"),
            entries: &entries,
        });
        let mip_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mipmap source"),
            entries: &entries[..2],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mipmap layout"),
            bind_group_layouts: &[Some(&mip_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("mipmap.wgsl"));
        let mip_srgb = fullscreen_pipeline(
            device,
            &pipeline_layout,
            &shader,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        let mip_linear = fullscreen_pipeline(
            device,
            &pipeline_layout,
            &shader,
            wgpu::TextureFormat::Rgba8Unorm,
        );
        let mip_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            min_filter: wgpu::FilterMode::Linear,
            mag_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let white = Arc::new(
            TextureData::new(ImageExtent::new(1, 1).expect("one pixel"), vec![255; 4])
                .expect("RGBA pixel"),
        );
        Self {
            layout,
            textures: HashMap::new(),
            materials: HashMap::new(),
            white,
            mip_layout,
            mip_srgb,
            mip_linear,
            mip_sampler,
        }
    }

    pub fn prepare(
        &mut self,
        gpu: &Gpu,
        id: MaterialId,
        maps: &MaterialMaps,
    ) -> Result<(), BoxError> {
        let sources = [
            &maps.base_color,
            &maps.metallic_roughness,
            &maps.normal,
            &maps.emissive,
            &maps.occlusion,
        ];
        let keys = std::array::from_fn(|i| {
            (
                Arc::as_ptr(sources[i].as_ref().unwrap_or(&self.white)) as usize,
                i == 0 || i == 3,
            )
        });
        if self
            .materials
            .get(&id)
            .is_some_and(|entry| entry.keys == keys)
        {
            return Ok(());
        }
        for (i, source) in sources.into_iter().enumerate() {
            if !self.textures.contains_key(&keys[i]) {
                let texture = Arc::clone(source.as_ref().unwrap_or(&self.white));
                let bound = self.upload(gpu, texture, keys[i].1)?;
                self.textures.insert(keys[i], bound);
            }
        }
        let entries: Vec<_> = keys
            .iter()
            .enumerate()
            .flat_map(|(i, key)| {
                let texture = &self.textures[key];
                [
                    wgpu::BindGroupEntry {
                        binding: i as u32 * 2,
                        resource: wgpu::BindingResource::TextureView(&texture.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: i as u32 * 2 + 1,
                        resource: wgpu::BindingResource::Sampler(&texture.sampler),
                    },
                ]
            })
            .collect();
        let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("PBR maps"),
            layout: &self.layout,
            entries: &entries,
        });
        self.materials.insert(id, BoundMaterial { keys, group });
        Ok(())
    }
    pub fn group(&self, id: MaterialId) -> &wgpu::BindGroup {
        &self.materials[&id].group
    }
    pub fn retain(&mut self, used: &HashSet<MaterialId>) {
        self.materials.retain(|id, _| used.contains(id));
        let keys: HashSet<_> = self.materials.values().flat_map(|m| m.keys).collect();
        self.textures.retain(|key, _| keys.contains(key));
    }
    fn upload(
        &self,
        gpu: &Gpu,
        source: Arc<TextureData>,
        srgb: bool,
    ) -> Result<BoundTexture, BoxError> {
        let extent = source.extent();
        if extent.width() > gpu.device.limits().max_texture_dimension_2d
            || extent.height() > gpu.device.limits().max_texture_dimension_2d
        {
            return Err("material texture exceeds GPU limits".into());
        }
        let mip_count = extent.width().max(extent.height()).ilog2() + 1;
        let format = if srgb {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("material texture"),
            size: wgpu::Extent3d {
                width: extent.width(),
                height: extent.height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            texture.as_image_copy(),
            source.pixels(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(extent.width() * 4),
                rows_per_image: Some(extent.height()),
            },
            texture.size(),
        );
        if mip_count > 1 {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            for mip in 1..mip_count {
                let previous = texture.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: mip - 1,
                    mip_level_count: Some(1),
                    ..Default::default()
                });
                let target = texture.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: mip,
                    mip_level_count: Some(1),
                    ..Default::default()
                });
                let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("mipmap input"),
                    layout: &self.mip_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&previous),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.mip_sampler),
                        },
                    ],
                });
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(if srgb {
                    &self.mip_srgb
                } else {
                    &self.mip_linear
                });
                pass.set_bind_group(0, &group, &[]);
                pass.draw(0..3, 0..1);
            }
            gpu.queue.submit([encoder.finish()]);
        }
        let config = source.sampler();
        let address = |wrap| match wrap {
            Wrap::Repeat => wgpu::AddressMode::Repeat,
            Wrap::Clamp => wgpu::AddressMode::ClampToEdge,
            Wrap::Mirror => wgpu::AddressMode::MirrorRepeat,
        };
        let filter = |value| match value {
            Filter::Linear => wgpu::FilterMode::Linear,
            Filter::Nearest => wgpu::FilterMode::Nearest,
        };
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: address(config.wrap_u),
            address_mode_v: address(config.wrap_v),
            mag_filter: filter(config.mag),
            min_filter: filter(config.min),
            mipmap_filter: match config.mip.unwrap_or(Filter::Nearest) {
                Filter::Nearest => wgpu::MipmapFilterMode::Nearest,
                Filter::Linear => wgpu::MipmapFilterMode::Linear,
            },
            lod_max_clamp: if config.mip.is_some() {
                mip_count as f32
            } else {
                0.0
            },
            ..Default::default()
        });
        Ok(BoundTexture {
            view: texture.create_view(&Default::default()),
            sampler,
            _source: source,
        })
    }
}

pub(crate) fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

pub(crate) fn fullscreen_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("fullscreen pass"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(format.into())],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}
