use crate::{Gpu, offscreen::BoxError, scene::SceneView};
use wgpu::util::DeviceExt;

/// Inspection output, not physical scene-linear radiance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum DiagnosticView {
    #[default]
    Shaded = 0,
    Wireframe = 1,
    /// World-space geometric normals, encoded as `(normal + 1) / 2`.
    Normals = 2,
    /// Normalized GPU depth: near = 0, far = 1 (not metric distance).
    Depth = 3,
}

/// Always-on-top, single-pixel display overlays, drawn after the output transform.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticOverlays {
    /// One-metre world axes: +X red, +Y green, -Z (up) blue.
    pub axes: bool,
    /// Yellow world-axis-aligned bounds of each visible instance.
    pub bounds: bool,
}

/// Shared raster configuration. Defaults preserve the single-sample shaded path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderSettings {
    /// Bounded portable choices: 1 (off) or 4. Unsupported requests are errors.
    pub sample_count: u32,
    pub view: DiagnosticView,
    pub overlays: DiagnosticOverlays,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            sample_count: 1,
            view: DiagnosticView::Shaded,
            overlays: DiagnosticOverlays::default(),
        }
    }
}

impl RenderSettings {
    pub(crate) fn validate(self, gpu: &Gpu, output: wgpu::TextureFormat) -> Result<(), BoxError> {
        if !matches!(self.sample_count, 1 | 4) {
            return Err("renderer sample count must be 1 or 4; no automatic downgrade".into());
        }
        for format in [
            output,
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureFormat::Depth32Float,
        ] {
            let features = gpu.adapter.get_texture_format_features(format);
            if !features
                .allowed_usages
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
                || !features.flags.sample_count_supported(self.sample_count)
                || (self.sample_count > 1
                    && format != wgpu::TextureFormat::Depth32Float
                    && !features
                        .flags
                        .contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE))
            {
                return Err(
                    format!("unsupported {format:?} at {} samples", self.sample_count).into(),
                );
            }
        }
        Ok(())
    }
}

/// Preserve triangle order so each material part maps to twice its index range.
pub(crate) fn line_indices(indices: &[u32]) -> Vec<u32> {
    indices
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|t| [t[0], t[1], t[1], t[2], t[2], t[0]])
        .collect()
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayVertex {
    position: [f32; 3],
    color: [f32; 3],
}

pub(crate) struct Overlays {
    pipeline: wgpu::RenderPipeline,
    vertices: Option<wgpu::Buffer>,
    count: u32,
}

impl Overlays {
    pub(crate) fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        camera: &wgpu::BindGroupLayout,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("diagnostics.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("display overlays"),
            bind_group_layouts: &[Some(camera)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("display overlays"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 24,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(format.into())],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            vertices: None,
            count: 0,
        }
    }

    pub(crate) fn prepare(
        &mut self,
        gpu: &Gpu,
        scene: &SceneView,
        settings: DiagnosticOverlays,
    ) -> Result<(), BoxError> {
        let mut vertices = Vec::new();
        let mut line = |a: [f64; 3], b: [f64; 3], color| {
            for position in [a, b] {
                vertices.push(OverlayVertex {
                    position: position.map(|v| v as f32),
                    color,
                });
            }
        };
        if settings.axes {
            line([0.0; 3], [1.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
            line([0.0; 3], [0.0, 1.0, 0.0], [0.0, 1.0, 0.0]);
            line([0.0; 3], [0.0, 0.0, -1.0], [0.0, 0.0, 1.0]);
        }
        if settings.bounds {
            for (_, instance) in scene.instances().filter(|(_, i)| i.visible()) {
                let bounds = instance.bounds();
                let corners: [[f64; 3]; 8] = std::array::from_fn(|corner| {
                    std::array::from_fn(|axis| {
                        if corner & (1 << axis) == 0 {
                            bounds.min()[axis]
                        } else {
                            bounds.max()[axis]
                        }
                    })
                });
                for corner in 0..8 {
                    for axis in 0..3 {
                        if corner & (1 << axis) == 0 {
                            line(
                                corners[corner],
                                corners[corner | (1 << axis)],
                                [1.0, 1.0, 0.0],
                            );
                        }
                    }
                }
            }
        }
        self.count = u32::try_from(vertices.len())?;
        if std::mem::size_of_val(vertices.as_slice()) as u64 > gpu.device.limits().max_buffer_size
            || vertices
                .iter()
                .any(|v| !v.position.into_iter().all(f32::is_finite))
        {
            return Err("diagnostic overlays exceed GPU limits".into());
        }
        self.vertices = if vertices.is_empty() {
            None
        } else {
            Some(
                gpu.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("derived diagnostic overlays"),
                        contents: bytemuck::cast_slice(&vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
            )
        };
        Ok(())
    }

    pub(crate) fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        camera: &wgpu::BindGroup,
    ) {
        let Some(vertices) = &self.vertices else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("display diagnostic overlays"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, camera, &[]);
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.draw(0..self.count, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_wireframe_retains_shared_vertices_and_part_ranges() {
        let indices = [2, 0, 3, 3, 0, 1];
        let lines = line_indices(&indices);
        assert_eq!(lines, [2, 0, 0, 3, 3, 2, 3, 0, 0, 1, 1, 3]);
        assert_eq!(&lines[6..12], line_indices(&indices[3..6]));
    }
}
