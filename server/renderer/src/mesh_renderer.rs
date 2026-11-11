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
    offscreen::BoxError,
    scene::{MeshId, SceneView},
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawUniform {
    model: [[f32; 4]; 4],
    color: [f32; 4],
}

struct MeshBuffers {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
}

struct Draw {
    mesh: MeshId,
    indices: Range<u32>,
    offset: u32,
}

/// Shared opaque, unlit mesh/depth baseline. PBR/light evaluation is a later path.
/// Prepare and submit a frame before preparing another frame with these uniforms.
pub struct MeshRenderer {
    pipeline: wgpu::RenderPipeline,
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
}

impl MeshRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let camera_layout = uniform_layout(device, false, 64);
        let object_layout = uniform_layout(device, true, 80);
        let camera = uniform_buffer(device, 64);
        let camera_group = uniform_group(device, &camera_layout, &camera, 64);
        let stride = 80_u64.div_ceil(u64::from(
            device.limits().min_uniform_buffer_offset_alignment,
        )) * u64::from(device.limits().min_uniform_buffer_offset_alignment);
        let objects = uniform_buffer(device, stride);
        let object_group = uniform_group(device, &object_layout, &objects, 80);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&object_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("mesh.wgsl"));
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("unlit indexed meshes"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 12,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3],
                })],
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
        });
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
        }
    }

    /// The destination must have the camera's extent, the constructor's format and one sample.
    pub fn prepare(
        &mut self,
        gpu: &Gpu,
        scene: &SceneView,
        camera: &Camera,
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
        for (_, instance) in scene.instances().filter(|(_, i)| i.visible()) {
            let id = instance.mesh();
            let mesh = scene.mesh(id)?;
            let rows = instance.model().rows();
            let model: [[f32; 4]; 4] =
                std::array::from_fn(|column| std::array::from_fn(|row| rows[row][column] as f32));
            if !model.iter().flatten().all(|v| v.is_finite()) {
                return Err("model exceeds GPU float range".into());
            }
            for (part, material) in mesh.parts().iter().zip(instance.materials()) {
                let mut color = scene.material(*material)?.base_color;
                color[3] = 1.0;
                let offset = u32::try_from(self.stride * uniforms.len() as u64)?;
                draws.push(Draw {
                    mesh: id,
                    indices: part.clone(),
                    offset,
                });
                uniforms.push(DrawUniform { model, color });
            }
            let positions = bytemuck::cast_slice(mesh.positions());
            let indices = bytemuck::cast_slice(mesh.indices());
            if positions.len() as u64 > limits.max_buffer_size
                || indices.len() as u64 > limits.max_buffer_size
            {
                return Err("mesh exceeds device buffer limits".into());
            }
            self.meshes.entry(id).or_insert_with(|| MeshBuffers {
                vertices: gpu
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("mesh positions"),
                        contents: positions,
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
                indices: gpu
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("mesh indices"),
                        contents: indices,
                        usage: wgpu::BufferUsages::INDEX,
                    }),
            });
            used.insert(id);
        }
        let bytes = self.stride * uniforms.len().max(1) as u64;
        if bytes > limits.max_buffer_size {
            return Err("mesh draw uniforms exceed device buffer limit".into());
        }
        if bytes > self.capacity {
            self.objects = uniform_buffer(&gpu.device, bytes);
            self.object_group = uniform_group(&gpu.device, &self.object_layout, &self.objects, 80);
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
        gpu.queue
            .write_buffer(&self.camera, 0, bytemuck::cast_slice(&matrix));
        for (index, uniform) in uniforms.iter().enumerate() {
            gpu.queue.write_buffer(
                &self.objects,
                index as u64 * self.stride,
                bytemuck::bytes_of(uniform),
            );
        }
        self.meshes.retain(|id, _| used.contains(id));
        self.draws = draws;
        Ok(())
    }

    /// Record only after successful prepare; GPU buffers stay owned by the renderer.
    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
        let Some((_, depth)) = &self.depth else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mesh scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.camera_group, &[]);
        for draw in &self.draws {
            let mesh = &self.meshes[&draw.mesh];
            pass.set_bind_group(1, &self.object_group, &[draw.offset]);
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(draw.indices.clone(), 0, 0..1);
        }
    }
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
