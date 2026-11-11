use wgpu::util::DeviceExt;

use crate::coordinates::{ImageExtent, Ti};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    snc: [f32; 2],
    color: [f32; 3],
}

/// The same pipeline draws to a window surface or an offscreen texture.
pub struct TriangleRenderer {
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    viewport: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl TriangleRenderer {
    /// The target must have this format and a sample count of one.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let ti = Ti::new([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]])
            .expect("synthetic identity Ti is invertible");
        let vertices = [
            ([0.0, -0.55], [1.0, 0.0, 0.0]),
            ([-0.55, 0.55], [0.0, 1.0, 0.0]),
            ([0.55, 0.55], [0.0, 0.0, 1.0]),
        ]
        .map(|(ground, color)| Vertex {
            snc: ti
                .project_ground(ground)
                .expect("synthetic ground point has a finite projection")
                .map(|v| v as f32),
            color,
        });
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("triangle vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let viewport = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("SNC to NDC scale"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("viewport layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("viewport"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: viewport.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("triangle layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("triangle.wgsl"));
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("triangle pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x3],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(format.into())],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            vertices,
            viewport,
            bind_group,
        }
    }

    /// Update before encoding, and submit that frame before updating again.
    pub fn resize(&self, queue: &wgpu::Queue, extent: ImageExtent) {
        let [x, y] = extent.snc_to_ndc_scale();
        queue.write_buffer(
            &self.viewport,
            0,
            bytemuck::cast_slice(&[x as f32, y as f32, 0.0, 0.0]),
        );
    }

    /// Clears the target to black and records one RGB triangle.
    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("hello triangle"),
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..3, 0..1);
    }
}
