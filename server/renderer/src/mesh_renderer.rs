use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU64,
    ops::Range,
};
use wgpu::util::DeviceExt;

use crate::{
    Gpu,
    camera::Camera,
    coordinates::ImageExtent,
    material_maps::{MaterialMapCache, fullscreen_pipeline, texture_entry},
    offscreen::BoxError,
    scene::{Light, MaterialId, MaterialMaps, MeshId, SceneView},
    shadow::{self, CascadeSettings, ShadowAtlas, ShadowMode, ShadowProjection, ShadowSettings},
};

#[derive(Clone, Copy, Debug, Default)]
pub enum ToneMap {
    Reinhard,
    #[default]
    Aces,
}

#[derive(Clone, Copy, Debug)]
pub struct PbrSettings {
    pub exposure: f32,
    pub tone_map: ToneMap,
    pub shadows: ShadowSettings,
}

impl Default for PbrSettings {
    fn default() -> Self {
        Self {
            exposure: 1.0,
            tone_map: ToneMap::Aces,
            shadows: ShadowSettings::default(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    tangent: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    clip: [[f32; 4]; 4],
    eye: [f32; 4],
    options: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LightUniform {
    position_kind: [f32; 4],
    direction_inner: [f32; 4],
    emission_outer: [f32; 4],
    shadow_matrix: [[[f32; 4]; 4]; 4],
    shadow_params: [f32; 4],
    shadow_depth: [[f32; 4]; 4],
    // Source size, search cap, filter cap, effective shadow mode.
    pcss: [f32; 4],
    pcss_samples: [f32; 4],
    cascade_splits: [f32; 4],
    cascade_params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawUniform {
    model: [[f32; 4]; 4],
    normal: [[f32; 4]; 3],
    color: [f32; 4],
    emissive_cutoff: [f32; 4],
    factors: [f32; 4],
    flags: [u32; 4],
}

struct MeshBuffers {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
}

struct Draw {
    mesh: MeshId,
    indices: Range<u32>,
    offset: u32,
    material: MaterialId,
}

#[derive(PartialEq)]
struct CascadeKey {
    view: [[f32; 4]; 4],
    settings: CascadeSettings,
    padding: f32,
}

struct ShadowCache {
    scene: SceneView,
    resolution: u32,
    filter_radius: f32,
    max_bytes: u64,
    active: Vec<usize>,
    cascades: Option<CascadeKey>,
}

/// Shared mesh/depth renderer with diagnostic unlit and textured PBR paths.
/// Prepare and submit a frame before preparing another frame with these uniforms.
pub struct MeshRenderer {
    pipeline: wgpu::RenderPipeline,
    pbr_pipeline: wgpu::RenderPipeline,
    camera: wgpu::Buffer,
    camera_group: wgpu::BindGroup,
    object_layout: wgpu::BindGroupLayout,
    objects: wgpu::Buffer,
    object_group: wgpu::BindGroup,
    stride: u64,
    capacity: u64,
    depth: Option<(ImageExtent, wgpu::TextureView)>,
    meshes: HashMap<MeshId, MeshBuffers>,
    draws: Vec<Draw>,
    maps: MaterialMapCache,
    lights: wgpu::Buffer,
    light_group: wgpu::BindGroup,
    light_layout: wgpu::BindGroupLayout,
    light_capacity: u64,
    tone_pipeline: wgpu::RenderPipeline,
    tone_layout: wgpu::BindGroupLayout,
    tone_settings: wgpu::Buffer,
    hdr: Option<(ImageExtent, wgpu::TextureView, wgpu::BindGroup)>,
    pbr_active: bool,
    camera_layout: wgpu::BindGroupLayout,
    shadow_pipeline: wgpu::RenderPipeline,
    point_shadow_pipeline: wgpu::RenderPipeline,
    shadow_atlas: ShadowAtlas,
    shadow_cameras: Vec<(usize, bool, wgpu::BindGroup)>,
    shadow_projections: Vec<Option<ShadowProjection>>,
    shadow_layers: Vec<Option<usize>>,
    shadow_cache: Option<ShadowCache>,
    shadow_dirty: bool,
    shadow_passes_last_frame: u32,
}

impl MeshRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let light_size = std::mem::size_of::<LightUniform>() as u64;
        let camera_layout = uniform_layout(device, false, 96);
        let object_layout = uniform_layout(device, true, 176);
        let camera = uniform_buffer(device, 96);
        let camera_group = uniform_group(device, &camera_layout, &camera, 96);
        let maps = MaterialMapCache::new(device);
        let light_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene lights"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(light_size),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let lights = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene lights"),
            size: light_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shadow_atlas = ShadowAtlas::new(device, 1, 1);
        let light_group = light_group(device, &light_layout, &lights, light_size, &shadow_atlas);
        let tone_settings = uniform_buffer(device, 16);
        let tone_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tone map"),
            entries: &[
                texture_entry(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(16),
                    },
                    count: None,
                },
            ],
        });
        let tone_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tone map"),
            bind_group_layouts: &[Some(&tone_layout)],
            immediate_size: 0,
        });
        let tone_shader = device.create_shader_module(wgpu::include_wgsl!("tone_map.wgsl"));
        let tone_pipeline =
            fullscreen_pipeline(device, &tone_pipeline_layout, &tone_shader, format);
        let stride = 176_u64.div_ceil(u64::from(
            device.limits().min_uniform_buffer_offset_alignment,
        )) * u64::from(device.limits().min_uniform_buffer_offset_alignment);
        let objects = uniform_buffer(device, stride);
        let object_group = uniform_group(device, &object_layout, &objects, 176);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&object_layout),
                Some(&maps.layout),
                Some(&light_layout),
            ],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("mesh.wgsl"));
        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&object_layout),
                Some(&maps.layout),
            ],
            immediate_size: 0,
        });
        let create_shadow_pipeline = |entry_point| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("alpha-masked shadow depth"),
                layout: Some(&shadow_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_shadow"),
                    compilation_options: Default::default(),
                    buffers: &[Some(vertex_layout())],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry_point),
                    compilation_options: Default::default(),
                    targets: &[],
                }),
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let shadow_pipeline = create_shadow_pipeline("fs_shadow");
        let point_shadow_pipeline = create_shadow_pipeline("fs_shadow_point");
        let create_pipeline = |format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("unlit indexed meshes"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(vertex_layout())],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(format.into())],
                }),
                // No culling in this diagnostic path; reflected instances remain visible.
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = create_pipeline(format);
        let pbr_pipeline = create_pipeline(wgpu::TextureFormat::Rgba16Float);
        Self {
            pipeline,
            camera,
            camera_group,
            object_layout,
            objects,
            object_group,
            stride,
            capacity: stride,
            depth: None,
            meshes: HashMap::new(),
            draws: Vec::new(),
            pbr_pipeline,
            maps,
            lights,
            light_group,
            light_layout,
            light_capacity: light_size,
            tone_pipeline,
            tone_layout,
            tone_settings,
            hdr: None,
            pbr_active: false,
            camera_layout,
            shadow_pipeline,
            point_shadow_pipeline,
            shadow_atlas,
            shadow_cameras: Vec::new(),
            shadow_projections: Vec::new(),
            shadow_layers: Vec::new(),
            shadow_cache: None,
            shadow_dirty: false,
            shadow_passes_last_frame: 0,
        }
    }

    /// The destination must have the camera's extent, the constructor's format and one sample.
    pub fn prepare(
        &mut self,
        gpu: &Gpu,
        scene: &SceneView,
        camera: &Camera,
    ) -> Result<(), BoxError> {
        self.prepare_mode(gpu, scene, camera, None)
    }

    /// Scene-linear HDR shading followed by one output exposure/tone-map pass.
    pub fn prepare_pbr(
        &mut self,
        gpu: &Gpu,
        scene: &SceneView,
        camera: &Camera,
        settings: PbrSettings,
    ) -> Result<(), BoxError> {
        if !settings.exposure.is_finite() || settings.exposure < 0.0 {
            return Err("exposure must be finite and nonnegative".into());
        }
        settings.shadows.validate()?;
        let quality = settings.shadows.pcss;
        let mut counts = [0_u64; 4];
        for light in scene.lighting().lights() {
            let blended = matches!(light, Light::Directional { .. })
                && settings.shadows.cascades.count > 1
                && settings.shadows.cascades.blend_fraction > 0.0;
            counts[light.effective_shadow_mode(settings.shadows.mode) as usize] +=
                if blended { 2 } else { 1 };
        }
        if counts[ShadowMode::Pcf as usize] > 0
            && ![1, 4, 9, 16, 25, 36, 49, 64].contains(&settings.shadows.pcf_samples)
        {
            return Err("PCF requires a square kernel of 1..=64 samples".into());
        }
        if counts[ShadowMode::Pcss as usize] > 0 {
            quality.validate()?;
        }
        let samples = counts[ShadowMode::Hard as usize]
            + counts[ShadowMode::Pcf as usize] * u64::from(settings.shadows.pcf_samples)
            + counts[ShadowMode::Pcss as usize]
                * (u64::from(quality.blocker_samples) + u64::from(quality.filter_samples));
        if samples > settings.shadows.max_samples_per_fragment {
            return Err(
                "shadow filtering exceeds the configured per-fragment sample budget".into(),
            );
        }
        if scene.lighting().sky().is_some() {
            return Err(
                "environment lighting is not implemented in the direct-light PBR path".into(),
            );
        }
        self.prepare_mode(gpu, scene, camera, Some(settings))
    }

    fn prepare_mode(
        &mut self,
        gpu: &Gpu,
        scene: &SceneView,
        camera: &Camera,
        settings: Option<PbrSettings>,
    ) -> Result<(), BoxError> {
        let matrix = camera.clip_from_world()?;
        let extent = camera.extent();
        let limits = gpu.device.limits();
        if extent.width() > limits.max_texture_dimension_2d
            || extent.height() > limits.max_texture_dimension_2d
        {
            return Err("mesh target exceeds texture limits".into());
        }
        let mut uniforms = Vec::new();
        let mut draws = Vec::new();
        let mut used = HashSet::new();
        let mut used_materials = HashSet::new();
        let unlit_maps = MaterialMaps::default();
        for (_, instance) in scene.instances().filter(|(_, i)| i.visible()) {
            let id = instance.mesh();
            let mesh = scene.mesh(id)?;
            let rows = instance.model().rows();
            let model: [[f32; 4]; 4] =
                std::array::from_fn(|column| std::array::from_fn(|row| rows[row][column] as f32));
            let n = instance.model().normal_matrix();
            let normal =
                std::array::from_fn(|c| [n[0][c] as f32, n[1][c] as f32, n[2][c] as f32, 0.0]);
            if !model
                .iter()
                .flatten()
                .chain(normal.iter().flatten())
                .all(|v| v.is_finite())
            {
                return Err("model exceeds GPU float range".into());
            }
            for (part, material) in mesh.parts().iter().zip(instance.materials()) {
                let factors = scene.material(*material)?;
                let maps = scene.material_maps(*material)?;
                self.maps.prepare(
                    gpu,
                    *material,
                    if settings.is_some() {
                        maps
                    } else {
                        &unlit_maps
                    },
                )?;
                used_materials.insert(*material);
                let offset = u32::try_from(self.stride * uniforms.len() as u64)?;
                draws.push(Draw {
                    mesh: id,
                    indices: part.clone(),
                    offset,
                    material: *material,
                });
                uniforms.push(DrawUniform {
                    model,
                    normal,
                    color: factors.base_color,
                    emissive_cutoff: [
                        factors.emissive[0],
                        factors.emissive[1],
                        factors.emissive[2],
                        factors.alpha_cutoff.unwrap_or(-1.0),
                    ],
                    factors: [
                        factors.metallic,
                        factors.roughness,
                        factors.normal_scale,
                        factors.occlusion_strength,
                    ],
                    flags: [
                        u32::from(maps.normal.is_some()),
                        u32::from(factors.unlit),
                        u32::from(factors.double_sided),
                        u32::from(instance.model().is_mirrored()),
                    ],
                });
            }
            let indices = bytemuck::cast_slice(mesh.indices());
            if indices.len() as u64 > limits.max_buffer_size {
                return Err("mesh exceeds device buffer limits".into());
            }
            if mesh.positions().len() as u64 * 64 > limits.max_buffer_size {
                return Err("mesh vertex attributes exceed GPU limits".into());
            }
            self.meshes.entry(id).or_insert_with(|| {
                let attrs = mesh.attributes();
                let vertices: Vec<_> = mesh
                    .positions()
                    .iter()
                    .enumerate()
                    .map(|(i, &position)| Vertex {
                        position,
                        normal: attrs.normals.get(i).copied().unwrap_or([0.0; 3]),
                        uv: attrs.tex_coords.get(i).copied().unwrap_or([0.0; 2]),
                        tangent: attrs
                            .tangents
                            .get(i)
                            .copied()
                            .unwrap_or([0.0, 0.0, 0.0, 1.0]),
                        color: attrs.colors.get(i).copied().unwrap_or([1.0; 4]),
                    })
                    .collect();
                MeshBuffers {
                    vertices: gpu
                        .device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("mesh surface vertices"),
                            contents: bytemuck::cast_slice(&vertices),
                            usage: wgpu::BufferUsages::VERTEX,
                        }),
                    indices: gpu
                        .device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("mesh indices"),
                            contents: indices,
                            usage: wgpu::BufferUsages::INDEX,
                        }),
                }
            });
            used.insert(id);
        }
        let bytes = self.stride * uniforms.len().max(1) as u64;
        if bytes > limits.max_buffer_size {
            return Err("mesh draw uniforms exceed device buffer limit".into());
        }
        if bytes > self.capacity {
            self.objects = uniform_buffer(&gpu.device, bytes);
            self.object_group = uniform_group(&gpu.device, &self.object_layout, &self.objects, 176);
            self.capacity = bytes;
        }
        if self.depth.as_ref().is_none_or(|(size, _)| *size != extent) {
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("mesh depth"),
                size: wgpu::Extent3d {
                    width: extent.width(),
                    height: extent.height(),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.depth = Some((extent, texture.create_view(&Default::default())));
        }
        if settings.is_some() && self.hdr.as_ref().is_none_or(|(size, _, _)| *size != extent) {
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("scene linear HDR"),
                size: wgpu::Extent3d {
                    width: extent.width(),
                    height: extent.height(),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("HDR output"),
                layout: &self.tone_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.tone_settings.as_entire_binding(),
                    },
                ],
            });
            self.hdr = Some((extent, view, group));
        }
        let mut lights: Vec<_> = if settings.is_some() {
            scene
                .lighting()
                .lights()
                .iter()
                .copied()
                .map(Self::pack_light)
                .collect::<Result<_, _>>()?
        } else {
            Vec::new()
        };
        let shadows = settings.map_or(ShadowSettings::default(), |s| s.shadows);
        let atlas_changed =
            self.prepare_shadows(gpu, scene, camera, settings.map(|s| s.shadows))?;
        for (index, light) in lights.iter_mut().enumerate() {
            light.pcss[3] =
                scene.lighting().lights()[index].effective_shadow_mode(shadows.mode) as u32 as f32;
            if let Some(layer) = self.shadow_layers.get(index).copied().flatten()
                && let Some(projection) = self.shadow_projections.get(layer).copied().flatten()
            {
                let count = if matches!(scene.lighting().lights()[index], Light::Directional { .. })
                {
                    shadows.cascades.count as usize
                } else {
                    1
                };
                for cascade in 0..count {
                    if let Some(map) = self.shadow_projections[layer + cascade] {
                        light.shadow_matrix[cascade] = map.matrix;
                        light.shadow_depth[cascade] = map.depth_scale;
                        light.cascade_splits[cascade] = map.split[1];
                    }
                }
                light.cascade_params = [
                    projection.split[0],
                    shadows.cascades.blend_fraction as f32,
                    0.0,
                    0.0,
                ];
                light.pcss[1] = shadows.pcss.max_search_radius;
                light.pcss[2] = shadows.pcss.max_filter_radius;
                light.pcss_samples = [
                    shadows.pcss.blocker_samples as f32,
                    shadows.pcss.filter_samples as f32,
                    shadows.pcf_samples as f32,
                    count as f32,
                ];
                light.shadow_params = [
                    layer as f32,
                    shadows.depth_bias,
                    shadows.normal_bias,
                    shadows.filter_radius,
                ];
            }
        }
        let bytes = std::mem::size_of::<LightUniform>() as u64 * lights.len().max(1) as u64;
        if bytes > limits.max_storage_buffer_binding_size {
            return Err("scene lights exceed storage binding limit".into());
        }
        let buffer_changed = bytes > self.light_capacity;
        if buffer_changed {
            self.lights = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("scene lights"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.light_capacity = bytes;
        }
        if buffer_changed || atlas_changed {
            self.light_group = light_group(
                &gpu.device,
                &self.light_layout,
                &self.lights,
                self.light_capacity,
                &self.shadow_atlas,
            );
        }
        if !lights.is_empty() {
            gpu.queue
                .write_buffer(&self.lights, 0, bytemuck::cast_slice(&lights));
        }
        let eye = camera.center().map(|v| v as f32);
        if !eye.into_iter().all(f32::is_finite) {
            return Err("camera exceeds GPU float range".into());
        }
        let camera_uniform = CameraUniform {
            clip: matrix,
            eye: [eye[0], eye[1], eye[2], 0.0],
            options: [u32::from(settings.is_some()), lights.len() as u32, 0, 0],
        };
        gpu.queue
            .write_buffer(&self.camera, 0, bytemuck::bytes_of(&camera_uniform));
        if let Some(settings) = settings {
            gpu.queue.write_buffer(
                &self.tone_settings,
                0,
                bytemuck::cast_slice(&[
                    settings.exposure,
                    match settings.tone_map {
                        ToneMap::Reinhard => 0.0,
                        ToneMap::Aces => 1.0,
                    },
                    0.0,
                    0.0,
                ]),
            );
        }
        for (index, uniform) in uniforms.iter().enumerate() {
            gpu.queue.write_buffer(
                &self.objects,
                index as u64 * self.stride,
                bytemuck::bytes_of(uniform),
            );
        }
        self.meshes.retain(|id, _| used.contains(id));
        self.maps.retain(&used_materials);
        self.draws = draws;
        self.pbr_active = settings.is_some();
        Ok(())
    }

    /// Record only after successful prepare; GPU buffers stay owned by the renderer.
    pub fn encode(&mut self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
        self.shadow_passes_last_frame = 0;
        let Some((_, depth)) = &self.depth else {
            return;
        };
        if self.shadow_dirty {
            for (layer, point, camera_group) in &self.shadow_cameras {
                self.shadow_passes_last_frame += 1;
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("light shadow map"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.shadow_atlas.layers[*layer],
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    ..Default::default()
                });
                pass.set_pipeline(if *point {
                    &self.point_shadow_pipeline
                } else {
                    &self.shadow_pipeline
                });
                pass.set_bind_group(0, camera_group, &[]);
                for draw in &self.draws {
                    let mesh = &self.meshes[&draw.mesh];
                    pass.set_bind_group(1, &self.object_group, &[draw.offset]);
                    pass.set_bind_group(2, self.maps.group(draw.material), &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(draw.indices.clone(), 0, 0..1);
                }
            }
            self.shadow_dirty = false;
        }
        let hdr = self.hdr.as_ref().filter(|_| self.pbr_active);
        let destination = hdr.map_or(view, |(_, texture, _)| texture);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mesh scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: destination,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.018,
                        g: 0.025,
                        b: 0.04,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        pass.set_pipeline(if self.pbr_active {
            &self.pbr_pipeline
        } else {
            &self.pipeline
        });
        pass.set_bind_group(0, &self.camera_group, &[]);
        pass.set_bind_group(3, &self.light_group, &[]);
        for draw in &self.draws {
            let mesh = &self.meshes[&draw.mesh];
            pass.set_bind_group(1, &self.object_group, &[draw.offset]);
            pass.set_bind_group(2, self.maps.group(draw.material), &[]);
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(draw.indices.clone(), 0, 0..1);
        }
        drop(pass);
        if let Some((_, _, group)) = hdr {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("single output transform"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.tone_pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    fn pack_light(light: Light) -> Result<LightUniform, BoxError> {
        let normalize = |v: [f64; 3]| {
            let scale = v.into_iter().map(f64::abs).fold(0.0, f64::max);
            let v = v.map(|x| x / scale);
            let len = v.into_iter().map(|x| x * x).sum::<f64>().sqrt();
            v.map(|x| (x / len) as f32)
        };
        let (position, kind, direction, emission, inner, outer) = match light {
            Light::Directional {
                direction,
                irradiance,
                ..
            } => ([0.0; 3], 0.0, normalize(direction), irradiance, 0.0, 0.0),
            Light::Point {
                position,
                intensity,
                ..
            } => (
                position.map(|v| v as f32),
                1.0,
                [0.0; 3],
                intensity,
                0.0,
                0.0,
            ),
            Light::Spot {
                position,
                direction,
                intensity,
                inner_angle,
                outer_angle,
                ..
            } => (
                position.map(|v| v as f32),
                2.0,
                normalize(direction),
                intensity,
                inner_angle.cos() as f32,
                outer_angle.cos() as f32,
            ),
        };
        let uniform = LightUniform {
            position_kind: [position[0], position[1], position[2], kind],
            direction_inner: [direction[0], direction[1], direction[2], inner],
            emission_outer: [emission[0], emission[1], emission[2], outer],
            shadow_matrix: [[[0.0; 4]; 4]; 4],
            shadow_params: [-1.0, 0.0, 0.0, 0.0],
            shadow_depth: [[0.0; 4]; 4],
            pcss: [
                match light {
                    Light::Directional { angular_radius, .. } => angular_radius.tan(),
                    Light::Point { source_radius, .. } | Light::Spot { source_radius, .. } => {
                        source_radius
                    }
                },
                0.0,
                0.0,
                0.0,
            ],
            pcss_samples: [0.0; 4],
            cascade_splits: [0.0; 4],
            cascade_params: [0.0; 4],
        };
        if !bytemuck::cast_slice::<_, f32>(&[uniform])
            .iter()
            .all(|v| v.is_finite())
        {
            return Err("light exceeds GPU float range".into());
        }

        Ok(uniform)
    }

    /// Depth passes encoded by the last frame; zero when cached maps were reused.
    pub fn shadow_passes_last_frame(&self) -> u32 {
        self.shadow_passes_last_frame
    }

    fn prepare_shadows(
        &mut self,
        gpu: &Gpu,
        scene: &SceneView,
        camera: &Camera,
        settings: Option<ShadowSettings>,
    ) -> Result<bool, BoxError> {
        let pbr = settings.is_some();
        let settings = settings.unwrap_or_default();
        let lights = scene.lighting().lights();
        let active: Vec<_> = lights
            .iter()
            .enumerate()
            .filter(|(_, light)| {
                pbr && light.effective_shadow_mode(settings.mode) != ShadowMode::Off
            })
            .map(|(index, _)| index)
            .collect();
        if active.is_empty() {
            self.shadow_cache = None;
            self.shadow_cameras.clear();
            self.shadow_projections.clear();
            self.shadow_layers.clear();
            self.shadow_dirty = false;
            return self.shadow_atlas.ensure(&gpu.device, 1, 1, u64::MAX);
        }
        let cascades = if settings.cascades.count > 1
            && active
                .iter()
                .any(|&index| matches!(lights[index], Light::Directional { .. }))
        {
            Some(CascadeKey {
                view: camera.clip_from_world()?,
                settings: settings.cascades,
                padding: shadow::cascade_padding(settings),
            })
        } else {
            None
        };
        if self.shadow_cache.as_ref().is_some_and(|cache| {
            cache.scene.same_snapshot(scene)
                && cache.resolution == settings.resolution
                && cache.filter_radius == settings.filter_radius
                && cache.max_bytes == settings.max_bytes
                && cache.active == active
                && cache.cascades == cascades
        }) {
            return Ok(false);
        }
        let bounds = shadow::scene_bounds(scene);
        let layers = u32::try_from(
            active
                .iter()
                .map(|&index| shadow::face_count(lights[index], settings.cascades))
                .sum::<usize>()
                .max(1),
        )?;
        let resolution = if bounds.is_some() && !lights.is_empty() {
            settings.resolution
        } else {
            1
        };
        let changed =
            self.shadow_atlas
                .ensure(&gpu.device, resolution, layers, settings.max_bytes)?;
        let mut projections = Vec::new();
        let mut light_layers = vec![None; lights.len()];
        for &index in &active {
            light_layers[index] = Some(projections.len());
            projections.extend(match bounds {
                Some(bounds) => shadow::projections(lights[index], bounds, settings, camera)?,
                None => vec![None; shadow::face_count(lights[index], settings.cascades)],
            });
        }
        let dirty = changed
            || self
                .shadow_cache
                .as_ref()
                .is_none_or(|cache| !cache.scene.same_snapshot(scene))
            || projections != self.shadow_projections
            || light_layers != self.shadow_layers;
        let mut cameras = Vec::new();
        for (index, matrix) in projections.iter().enumerate().filter(|_| dirty) {
            if let Some(matrix) = matrix {
                let data = CameraUniform {
                    clip: matrix.matrix,
                    eye: matrix
                        .point_position
                        .map_or([0.0; 4], |p| [p[0], p[1], p[2], matrix.depth_scale[1]]),
                    // Radial near distance is bit-packed in the shared camera options.
                    options: [0, matrix.depth_scale[0].to_bits(), 0, 0],
                };
                let buffer = gpu
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("shadow camera"),
                        contents: bytemuck::bytes_of(&data),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });
                cameras.push((
                    index,
                    matrix.point_position.is_some(),
                    uniform_group(&gpu.device, &self.camera_layout, &buffer, 96),
                ));
            }
        }
        if dirty {
            self.shadow_cameras = cameras;
        }
        self.shadow_projections = projections;
        self.shadow_layers = light_layers;
        self.shadow_cache = Some(ShadowCache {
            scene: scene.clone(),
            resolution: settings.resolution,
            filter_radius: settings.filter_radius,
            max_bytes: settings.max_bytes,
            active,
            cascades,
        });
        self.shadow_dirty |= dirty;
        Ok(changed)
    }
}

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 5] =
        wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Float32x2,3=>Float32x4,4=>Float32x4];
    wgpu::VertexBufferLayout {
        array_stride: 64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

fn light_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
    size: u64,
    atlas: &ShadowAtlas,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("lights and visibility"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer,
                    offset: 0,
                    size: NonZeroU64::new(size),
                }),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&atlas.view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&atlas.sampler),
            },
        ],
    })
}

fn uniform_layout(device: &wgpu::Device, dynamic: bool, size: u64) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mesh uniform layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: NonZeroU64::new(size),
            },
            count: None,
        }],
    })
}
fn uniform_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mesh uniforms"),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
fn uniform_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
    size: u64,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mesh uniforms"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: NonZeroU64::new(size),
            }),
        }],
    })
}
