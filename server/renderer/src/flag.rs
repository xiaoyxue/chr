//! GPU evaluation of flag surfaces: one compute dispatch per visible SimpleWave flag
//! writes its flag-local surface into a storage buffer that the main and shadow
//! passes read by vertex index (`flag_surface.wgsl`); native flags write the same
//! surface from their cloth simulation (`flag_cloth.rs`).

use std::num::NonZeroU64;

use wgpu::util::DeviceExt;

use crate::{
    Gpu,
    flag_cloth::{ClothDraw, ClothGpu},
    mesh_renderer,
    scene::{FLAG_COLUMNS, FLAG_ROWS, FLAG_VERTICES, Flag, FlagMotion, InstanceId, ModelMatrix},
};

/// Bytes of one surface vertex: position, normal, tangent and artwork UV as `vec4<f32>`.
const VERTEX_BYTES: u64 = 64;
/// Floats of one flag's compute parameters ([`params`]).
pub(crate) const PARAMS: usize = 12;
const WORKGROUP: u32 = 64;

/// WGSL constants and helpers shared by the surface passes and the flag draws.
pub(crate) fn prelude() -> String {
    format!(
        "const FLAG_COLUMNS: u32 = {FLAG_COLUMNS}u;\nconst FLAG_ROWS: u32 = {FLAG_ROWS}u;\n{}\n",
        include_str!("flag_shared.wgsl")
    )
}

/// Surface parameters of one flag: size and wave, its phases at the frame time, then
/// its revealed fraction, artwork turns and reveal direction code. A flat flag keeps
/// fixed phases, so its unchanged surface keeps cached shadows; a native flag has no
/// wave.
pub(crate) fn params(flag: Flag, seconds: f64) -> [f32; PARAMS] {
    let (wave, phases) = match flag.motion {
        FlagMotion::Wave(wave) => {
            let seconds = if wave.amplitude == 0.0 { 0.0 } else { seconds };
            (
                [wave.amplitude, wave.frequency],
                wave.phases(seconds).map(|phase| phase as f32),
            )
        }
        FlagMotion::Cloth(_) => ([0.0; 2], [0.0; 3]),
    };
    [
        flag.width,
        flag.height,
        wave[0],
        wave[1],
        phases[0],
        phases[1],
        phases[2],
        0.0,
        flag.revealed,
        f32::from(flag.artwork_turns),
        f32::from(flag.reveal_direction as u8),
        0.0,
    ]
}

/// One visible flag of a frame, in draw order.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FlagDraw {
    params: [f32; PARAMS],
    cloth: Option<ClothDraw>,
}

impl FlagDraw {
    /// Flag `flag` of instance `id`, drawn with `model` at animation time `seconds`.
    pub(crate) fn new(id: InstanceId, flag: Flag, model: ModelMatrix, seconds: f64) -> Self {
        let cloth = match flag.motion {
            FlagMotion::Wave(_) => None,
            FlagMotion::Cloth(cloth) => {
                Some(ClothDraw::new(id, [flag.width, flag.height], cloth, model))
            }
        };
        Self {
            params: params(flag, seconds),
            cloth,
        }
    }
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
    slots: Vec<Slot>,
    /// Draw groups of this frame's flags, in draw order.
    draws: Vec<wgpu::BindGroup>,
    /// This frame's SimpleWave slots; the cloth passes write the others.
    waves: Vec<usize>,
    last: Vec<[f32; PARAMS]>,
    cloth: ClothGpu,
}

impl FlagGpu {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
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
        let uniform = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: NonZeroU64::new(4 * PARAMS as u64),
            },
            count: None,
        };
        let compute_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flag surface evaluation"),
            entries: &[uniform, storage(1, false, wgpu::ShaderStages::COMPUTE)],
        });
        let draw_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flag object, surface and instances"),
            entries: &[
                mesh_renderer::object_entry(),
                storage(1, true, wgpu::ShaderStages::VERTEX),
                mesh_renderer::instance_entry(2),
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
            slots: Vec::new(),
            draws: Vec::new(),
            waves: Vec::new(),
            last: Vec::new(),
            cloth: ClothGpu::new(device),
        }
    }

    /// Uploads this frame's flags in draw order; slot `i` serves `flags[i]`. Draw
    /// groups bind `objects` and `instances`, which the caller may have reallocated.
    /// Returns whether any surface differs from the previous frame's.
    pub(crate) fn prepare(
        &mut self,
        gpu: &Gpu,
        flags: &[FlagDraw],
        objects: &wgpu::Buffer,
        instances: &wgpu::Buffer,
    ) -> bool {
        while self.slots.len() < flags.len() {
            let params = gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("flag parameters"),
                    contents: &[0; 4 * PARAMS],
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
        let params: Vec<_> = flags.iter().map(|flag| flag.params).collect();
        let mut moved = self.last != params;
        self.waves.clear();
        self.cloth.begin();
        for (index, (slot, flag)) in self.slots.iter().zip(flags).enumerate() {
            gpu.queue
                .write_buffer(&slot.params, 0, bytemuck::cast_slice(&flag.params));
            match flag.cloth {
                None => self.waves.push(index),
                Some(cloth) => moved |= self.cloth.plan(gpu, cloth, &slot.params, &slot.vertices),
            }
        }
        self.cloth.finish();
        self.draws = self.slots[..flags.len()]
            .iter()
            .map(|slot| self.draw_group(&gpu.device, objects, &slot.vertices, instances))
            .collect();
        self.last = params;
        moved
    }

    fn draw_group(
        &self,
        device: &wgpu::Device,
        objects: &wgpu::Buffer,
        vertices: &wgpu::Buffer,
        instances: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flag object, surface and instances"),
            layout: &self.draw_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: mesh_renderer::object_binding(objects),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: vertices.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: instances.as_entire_binding(),
                },
            ],
        })
    }

    /// Records the surface evaluation, including the native flags' simulation steps;
    /// it must precede every pass that draws flags.
    pub(crate) fn encode(&mut self, encoder: &mut wgpu::CommandEncoder) {
        if self.draws.is_empty() {
            return;
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("flag surfaces"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        for &index in &self.waves {
            pass.set_bind_group(0, &self.slots[index].compute, &[]);
            pass.dispatch_workgroups(FLAG_VERTICES.div_ceil(WORKGROUP), 1, 1);
        }
        self.cloth.encode(&mut pass);
    }

    pub(crate) fn group(&self, slot: usize) -> &wgpu::BindGroup {
        &self.draws[slot]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{FlagShading, Wave};

    #[test]
    #[ignore = "requires a hardware GPU"]
    fn the_compute_surface_matches_the_reference() {
        use crate::scene::RevealDirection::*;
        let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
        assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
        let wave = Wave {
            amplitude: 0.7,
            frequency: 3.0,
            speed: 2.5,
        };
        let flag = Flag {
            width: 3.2,
            height: 2.0,
            motion: FlagMotion::Wave(wave),
            shading: FlagShading::Simple,
            artwork_turns: 1,
            revealed: 0.6,
            reveal_direction: CenterHorizontal,
        };
        // A day of media time: phases are reduced on the CPU before reaching f32.
        let seconds = 86_400.25;
        let draw = |flag| {
            [FlagDraw {
                params: params(flag, seconds),
                cloth: None,
            }]
        };
        let buffer = |size, usage| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let objects = buffer(mesh_renderer::OBJECT_BYTES, wgpu::BufferUsages::UNIFORM);
        let instances = buffer(256, wgpu::BufferUsages::STORAGE);
        let mut flags = FlagGpu::new(&gpu.device);
        assert!(flags.prepare(&gpu, &draw(flag), &objects, &instances));
        assert!(!flags.prepare(&gpu, &draw(flag), &objects, &instances));
        let bytes = VERTEX_BYTES * u64::from(FLAG_VERTICES);
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        for direction in [
            CenterHorizontal,
            CenterVertical,
            LeftToRight,
            RightToLeft,
            TopToBottom,
            BottomToTop,
        ] {
            let flag = Flag {
                reveal_direction: direction,
                ..flag
            };
            flags.prepare(&gpu, &draw(flag), &objects, &instances);
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
            let vertices: &[[f32; 16]] = bytemuck::cast_slice(&data);
            let phases = wave.phases(seconds);
            let columns = FLAG_COLUMNS + 1;
            for (index, vertex) in (0..).zip(vertices) {
                let [u, v] = flag.revealed_uv(
                    f64::from(index % columns) / f64::from(FLAG_COLUMNS),
                    f64::from(index / columns) / f64::from(FLAG_ROWS),
                );
                let (position, normal) = wave.sample(flag.width, flag.height, u, v, phases);
                let uv = flag.artwork_uv(u, v);
                for axis in 0..2 {
                    let error = (f64::from(vertex[12 + axis]) - uv[axis]).abs();
                    assert!(
                        error < 1e-6,
                        "{direction:?}: vertex {index} uv {axis} is {error} off"
                    );
                }
                for axis in 0..3 {
                    let error = (f64::from(vertex[axis]) - position[axis]).abs();
                    assert!(
                        error < 1e-4,
                        "{direction:?}: vertex {index} axis {axis} is {error} m off"
                    );
                }
                let cosine: f64 = (0..3).map(|i| f64::from(vertex[4 + i]) * normal[i]).sum();
                assert!(
                    cosine > 0.9999,
                    "{direction:?}: vertex {index} normal is off: {cosine}"
                );
            }
            if direction == CenterHorizontal {
                let centre = vertices[(FLAG_COLUMNS / 2) as usize];
                let pinned = [1.6, 0.0, 0.0];
                assert!(
                    (0..3).all(|i| (centre[i] - pinned[i]).abs() < 1e-6),
                    "{centre:?}"
                );
            }
            drop(data);
            readback.unmap();
        }
    }
}
