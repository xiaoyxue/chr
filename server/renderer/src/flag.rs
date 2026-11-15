//! GPU evaluation of SimpleWave flags: one compute dispatch per visible flag writes
//! its flag-local surface into a storage buffer that the main and shadow passes
//! read by vertex index (`flag_surface.wgsl`).

use std::num::NonZeroU64;

use wgpu::util::DeviceExt;

use crate::{
    Gpu,
    scene::{FLAG_COLUMNS, FLAG_ROWS, FLAG_VERTICES, Flag},
};

/// Bytes of one surface vertex: position, normal and tangent as `vec4<f32>`.
const VERTEX_BYTES: u64 = 48;
const WORKGROUP: u32 = 64;

/// WGSL constants shared by the compute and draw shaders.
pub(crate) fn prelude() -> String {
    format!("const FLAG_COLUMNS: u32 = {FLAG_COLUMNS}u;\nconst FLAG_ROWS: u32 = {FLAG_ROWS}u;\n")
}

/// Compute parameters of one flag: size and wave, then its phases at the frame time.
/// A flat flag keeps fixed phases, so its unchanged surface keeps cached shadows.
pub(crate) fn params(flag: Flag, seconds: f64) -> [f32; 8] {
    let seconds = if flag.amplitude == 0.0 { 0.0 } else { seconds };
    let [a, b, c] = flag.phases(seconds).map(|phase| phase as f32);
    [
        flag.width,
        flag.height,
        flag.amplitude,
        flag.frequency,
        a,
        b,
        c,
        0.0,
    ]
}

struct Slot {
    params: wgpu::Buffer,
    vertices: wgpu::Buffer,
    compute: wgpu::BindGroup,
}

pub(crate) struct FlagGpu {
    pipeline: wgpu::ComputePipeline,
    compute_layout: wgpu::BindGroupLayout,
    /// Group 1 of flag draws: the dynamic-offset object uniform and the surface.
    pub(crate) draw_layout: wgpu::BindGroupLayout,
    object_bytes: u64,
    slots: Vec<Slot>,
    /// Draw groups of this frame's flags, in draw order.
    draws: Vec<wgpu::BindGroup>,
    last: Vec<[f32; 8]>,
}

impl FlagGpu {
    pub(crate) fn new(device: &wgpu::Device, object_bytes: u64) -> Self {
        let storage = |binding, read_only, visibility| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: NonZeroU64::new(VERTEX_BYTES * u64::from(FLAG_VERTICES)),
            },
            count: None,
        };
        let uniform = |dynamic, size, visibility| wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: NonZeroU64::new(size),
            },
            count: None,
        };
        let compute_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flag surface evaluation"),
            entries: &[
                uniform(false, 32, wgpu::ShaderStages::COMPUTE),
                storage(1, false, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let draw_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flag object and surface"),
            entries: &[
                uniform(true, object_bytes, wgpu::ShaderStages::VERTEX_FRAGMENT),
                storage(1, true, wgpu::ShaderStages::VERTEX),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flag surface evaluation"),
            source: wgpu::ShaderSource::Wgsl(
                format!("{}{}", prelude(), include_str!("flag.wgsl")).into(),
            ),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("flag surface evaluation"),
            bind_group_layouts: &[Some(&compute_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("flag surface evaluation"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("cs_flag"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            pipeline,
            compute_layout,
            draw_layout,
            object_bytes,
            slots: Vec::new(),
            draws: Vec::new(),
            last: Vec::new(),
        }
    }

    /// Uploads this frame's flags in draw order; slot `i` serves `flags[i]`. Draw
    /// groups bind `objects`, which the caller may have reallocated. Returns whether
    /// any surface differs from the previous frame's.
    pub(crate) fn prepare(
        &mut self,
        gpu: &Gpu,
        flags: &[[f32; 8]],
        objects: &wgpu::Buffer,
    ) -> bool {
        while self.slots.len() < flags.len() {
            let params = gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("flag parameters"),
                    contents: &[0; 32],
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                });
            let vertices = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("flag surface"),
                size: VERTEX_BYTES * u64::from(FLAG_VERTICES),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let compute = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("flag surface evaluation"),
                layout: &self.compute_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: vertices.as_entire_binding(),
                    },
                ],
            });
            self.slots.push(Slot {
                params,
                vertices,
                compute,
            });
        }
        for (slot, params) in self.slots.iter().zip(flags) {
            gpu.queue
                .write_buffer(&slot.params, 0, bytemuck::cast_slice(params));
        }
        self.draws = self.slots[..flags.len()]
            .iter()
            .map(|slot| self.draw_group(&gpu.device, objects, &slot.vertices))
            .collect();
        let moved = self.last != flags;
        self.last = flags.to_vec();
        moved
    }

    fn draw_group(
        &self,
        device: &wgpu::Device,
        objects: &wgpu::Buffer,
        vertices: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flag object and surface"),
            layout: &self.draw_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: objects,
                        offset: 0,
                        size: NonZeroU64::new(self.object_bytes),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: vertices.as_entire_binding(),
                },
            ],
        })
    }

    /// Records the surface evaluation; it must precede every pass that draws flags.
    pub(crate) fn encode(&self, encoder: &mut wgpu::CommandEncoder) {
        if self.draws.is_empty() {
            return;
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("flag surfaces"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        for slot in &self.slots[..self.draws.len()] {
            pass.set_bind_group(0, &slot.compute, &[]);
            pass.dispatch_workgroups(FLAG_VERTICES.div_ceil(WORKGROUP), 1, 1);
        }
    }

    pub(crate) fn group(&self, slot: usize) -> &wgpu::BindGroup {
        &self.draws[slot]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::FlagShading;

    #[test]
    #[ignore = "requires a hardware GPU"]
    fn the_compute_surface_matches_the_reference() {
        let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
        assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
        let flag = Flag {
            width: 3.2,
            height: 2.0,
            amplitude: 0.7,
            frequency: 3.0,
            speed: 2.5,
            shading: FlagShading::Simple,
        };
        // A day of media time: phases are reduced on the CPU before reaching f32.
        let seconds = 86_400.25;
        let objects = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: false,
        });
        let mut flags = FlagGpu::new(&gpu.device, 176);
        assert!(flags.prepare(&gpu, &[params(flag, seconds)], &objects));
        assert!(!flags.prepare(&gpu, &[params(flag, seconds)], &objects));
        let bytes = VERTEX_BYTES * u64::from(FLAG_VERTICES);
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        flags.encode(&mut encoder);
        encoder.copy_buffer_to_buffer(&flags.slots[0].vertices, 0, &readback, 0, bytes);
        gpu.queue.submit([encoder.finish()]);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.unwrap());
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let data = readback.slice(..).get_mapped_range().unwrap();
        let vertices: &[[f32; 12]] = bytemuck::cast_slice(&data);
        let phases = flag.phases(seconds);
        let columns = FLAG_COLUMNS + 1;
        for (index, vertex) in (0..).zip(vertices) {
            let u = f64::from(index % columns) / f64::from(FLAG_COLUMNS);
            let v = f64::from(index / columns) / f64::from(FLAG_ROWS);
            let (position, normal) = flag.sample(u, v, phases);
            for axis in 0..3 {
                let error = (f64::from(vertex[axis]) - position[axis]).abs();
                assert!(error < 1e-4, "vertex {index} axis {axis} is {error} m off");
            }
            let cosine: f64 = (0..3).map(|i| f64::from(vertex[4 + i]) * normal[i]).sum();
            assert!(cosine > 0.9999, "vertex {index} normal is off: {cosine}");
        }
        let centre = vertices[(FLAG_COLUMNS / 2) as usize];
        let pinned = [1.6, 0.0, 0.0];
        assert!(
            (0..3).all(|i| (centre[i] - pinned[i]).abs() < 1e-6),
            "{centre:?}"
        );
    }
}
