//! GPU simulation of native flags ([`Cloth`]). Each flag instance keeps two particle
//! buffers across frames; a frame advances them in fixed [`STEP`]s up to the cloth's
//! clock (`flag_cloth.wgsl`), then a surface pass writes the flag grid's surface that
//! the flag draws read, as the SimpleWave pass does (`flag.rs`).
//!
//! A frame's plan only records what to run; the particle state advances when the
//! plan is encoded, so a prepare without an encode loses nothing.

use std::{collections::HashMap, num::NonZeroU64};

use wgpu::util::DeviceExt;

use crate::{
    Gpu,
    scene::{
        CLOTH_COLUMNS, CLOTH_ROWS, Cloth, FLAG_VERTICES, GRAVITY, ITERATIONS, InstanceId,
        ModelMatrix, SIMULATED_HEIGHT, STEP, simulated_size, spacing,
    },
};

const PARTICLES: u32 = CLOTH_COLUMNS * CLOTH_ROWS;
/// Bytes of one particle: its position and previous position as `vec4<f32>`.
const PARTICLE_BYTES: u64 = 32;
/// Bytes of one step's settings (`Step` in `flag_cloth.wgsl`).
const STEP_BYTES: u64 = 64;
/// Bytes of a flag's surface parameters (`flag::PARAMS` floats).
const PARAMS_BYTES: u64 = 4 * super::flag::PARAMS as u64;
const WORKGROUP: u32 = 64;
/// Steps a cloth takes per frame at most: real time down to 15 fps. Simulated time
/// lost to a longer stall is skipped, not made up.
const FRAME_STEPS: u64 = 8;
/// Steps a cloth takes in its first frame at most, so a still of a late frame shows
/// a cloth that has met the wind (3 s), without stalling a live frame for longer.
const WARMUP_STEPS: u64 = 360;

/// A native flag of a frame: its instance, which identifies its simulation across
/// frames, its size in metres, its cloth and its flag-local gravity.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClothDraw {
    pub id: InstanceId,
    pub size: [f32; 2],
    pub cloth: Cloth,
    pub gravity: [f64; 3],
}

impl ClothDraw {
    /// A flag drawn with `model` (world from flag-local), whose gravity is world down
    /// (+Z) turned into the flag's axes.
    pub(crate) fn new(id: InstanceId, size: [f32; 2], cloth: Cloth, model: ModelMatrix) -> Self {
        let rows = model.rows();
        let down = [rows[2][0], rows[2][1], rows[2][2]];
        let length = down.iter().map(|x| x * x).sum::<f64>().sqrt();
        Self {
            id,
            size,
            cloth,
            gravity: down.map(|x| x / length * GRAVITY),
        }
    }
}

struct State {
    /// Width, height, slack and raise duration (0 unraised): what fixes the start shape.
    shape: [f32; 4],
    particles: [wgpu::Buffer; 2],
    settings: wgpu::Buffer,
    /// Steps `settings` holds room for.
    capacity: u64,
    /// Ping-pong groups: group `i` reads `particles[i]` and writes the other.
    groups: [wgpu::BindGroup; 2],
    /// Steps taken since the cloth hung in its rest arc.
    steps: u64,
    /// The particle buffer holding the latest state.
    current: usize,
}

/// One cloth's work for the prepared frame.
struct Run {
    id: InstanceId,
    /// Steps to take, whose settings `settings` holds in order; the last ends at `target`.
    steps: u64,
    target: u64,
    /// The surface pass, reading the particle buffer the steps end in.
    surface: wgpu::BindGroup,
}

pub(crate) struct ClothGpu {
    predict: wgpu::ComputePipeline,
    constrain: wgpu::ComputePipeline,
    surface: wgpu::ComputePipeline,
    solver_layout: wgpu::BindGroupLayout,
    surface_layout: wgpu::BindGroupLayout,
    /// Bytes between the settings of consecutive steps, a uniform offset alignment.
    stride: u64,
    states: HashMap<InstanceId, State>,
    runs: Vec<Run>,
}

fn storage(binding: u32, read_only: bool, size: u64) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: NonZeroU64::new(size),
        },
        count: None,
    }
}

fn uniform(binding: u32, dynamic: bool, size: u64) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: dynamic,
            min_binding_size: NonZeroU64::new(size),
        },
        count: None,
    }
}

fn entry(binding: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource }
}

impl ClothGpu {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let particles = PARTICLE_BYTES * u64::from(PARTICLES);
        let solver_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flag cloth solver"),
            entries: &[
                storage(0, true, particles),
                storage(1, false, particles),
                uniform(2, true, STEP_BYTES),
            ],
        });
        let surface_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flag cloth surface"),
            entries: &[
                storage(0, true, particles),
                storage(3, false, 64 * u64::from(FLAG_VERTICES)),
                uniform(4, false, PARAMS_BYTES),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flag cloth"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "{}const CLOTH_COLUMNS: u32 = {CLOTH_COLUMNS}u;\nconst CLOTH_ROWS: u32 = {CLOTH_ROWS}u;\nconst SIMULATED_HEIGHT: f32 = {SIMULATED_HEIGHT:?};\n{}",
                    super::flag::prelude(),
                    include_str!("flag_cloth.wgsl")
                )
                .into(),
            ),
        });
        let pipeline = |layout: &wgpu::BindGroupLayout, entry: &str| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(entry),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let alignment = u64::from(device.limits().min_uniform_buffer_offset_alignment);
        Self {
            predict: pipeline(&solver_layout, "predict"),
            constrain: pipeline(&solver_layout, "constrain"),
            surface: pipeline(&surface_layout, "surface"),
            solver_layout,
            surface_layout,
            stride: STEP_BYTES.next_multiple_of(alignment),
            states: HashMap::new(),
            runs: Vec::new(),
        }
    }

    /// Starts a frame's plan.
    pub(crate) fn begin(&mut self) {
        self.runs.clear();
    }

    /// Plans a cloth for this frame: steps towards its clock, then its surface into
    /// `vertices` with the flag's surface `params`. A cloth whose size, slack or raise
    /// changed or whose clock went back restarts from its start shape, hanging or
    /// folded to be raised. Returns whether its surface
    /// changes.
    pub(crate) fn plan(
        &mut self,
        gpu: &Gpu,
        draw: ClothDraw,
        params: &wgpu::Buffer,
        vertices: &wgpu::Buffer,
    ) -> bool {
        let ClothDraw {
            id,
            size,
            cloth,
            gravity,
        } = draw;
        let target = (cloth.seconds / STEP).floor() as u64;
        let shape = [size[0], size[1], cloth.slack, cloth.raise.unwrap_or(0.0)];
        // The particles live at the simulated size; the surface pass scales them.
        let size = simulated_size(size[0], size[1]);
        let restart = self
            .states
            .get(&id)
            .is_none_or(|state| state.shape != shape || state.steps > target);
        if restart {
            let state = self.state(gpu, size, cloth, shape);
            self.states.insert(id, state);
        }
        let state = self.states.get_mut(&id).expect("state inserted above");
        let limit = if state.steps == 0 {
            WARMUP_STEPS
        } else {
            FRAME_STEPS
        };
        let steps = (target - state.steps).min(limit);
        if steps > state.capacity {
            state.capacity = steps.next_power_of_two();
            state.settings = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("flag cloth steps"),
                size: self.stride * state.capacity,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            state.groups = solver_groups(
                &gpu.device,
                &self.solver_layout,
                &state.particles,
                &state.settings,
            );
        }
        if steps > 0 {
            let [across, down] = spacing(size[0], size[1], cloth.slack);
            let mut bytes = vec![0_u8; (self.stride * steps) as usize];
            for (k, chunk) in bytes.chunks_exact_mut(self.stride as usize).enumerate() {
                let seconds = (target - steps + k as u64) as f64 * STEP;
                let (wind, ripple) = cloth.wind_at(seconds);
                let settings = [
                    wind[0],
                    wind[1],
                    wind[2],
                    ripple,
                    gravity[0],
                    gravity[1],
                    gravity[2],
                    seconds,
                    across,
                    down,
                    cloth.bending(),
                    STEP,
                    // The pinned top edge where this step leaves it.
                    cloth.top_at(size[1], seconds + STEP),
                    0.0,
                    0.0,
                    0.0,
                ]
                .map(|x| x as f32);
                chunk[..STEP_BYTES as usize].copy_from_slice(bytemuck::cast_slice(&settings));
            }
            gpu.queue.write_buffer(&state.settings, 0, &bytes);
        }
        // Every step is an odd number of dispatches, each swapping the buffers.
        let last = state.current ^ (steps % 2) as usize;
        let surface = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flag cloth surface"),
            layout: &self.surface_layout,
            entries: &[
                entry(0, state.particles[last].as_entire_binding()),
                entry(3, vertices.as_entire_binding()),
                entry(4, params.as_entire_binding()),
            ],
        });
        self.runs.push(Run {
            id,
            steps,
            target,
            surface,
        });
        restart || steps > 0
    }

    /// Ends a frame's plan: cloths not drawn this frame are forgotten and restart
    /// from rest when drawn again.
    pub(crate) fn finish(&mut self) {
        let runs = &self.runs;
        self.states
            .retain(|id, _| runs.iter().any(|run| run.id == *id));
    }

    fn state(&self, gpu: &Gpu, size: [f32; 2], cloth: Cloth, shape: [f32; 4]) -> State {
        let rest: Vec<[f32; 8]> = cloth
            .start_positions(size[0], size[1])
            .into_iter()
            .map(|p| {
                let p = [p[0] as f32, p[1] as f32, p[2] as f32, 1.0];
                [p[0], p[1], p[2], p[3], p[0], p[1], p[2], p[3]]
            })
            .collect();
        let usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC;
        let particles = [
            gpu.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("flag cloth particles"),
                    contents: bytemuck::cast_slice(&rest),
                    usage,
                }),
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("flag cloth particles"),
                size: PARTICLE_BYTES * u64::from(PARTICLES),
                usage,
                mapped_at_creation: false,
            }),
        ];
        let settings = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("flag cloth steps"),
            size: self.stride * FRAME_STEPS,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let groups = solver_groups(&gpu.device, &self.solver_layout, &particles, &settings);
        State {
            shape,
            particles,
            settings,
            capacity: FRAME_STEPS,
            groups,
            steps: 0,
            current: 0,
        }
    }

    /// Records the planned steps and surfaces, and advances the cloths to them.
    pub(crate) fn encode(&mut self, pass: &mut wgpu::ComputePass<'_>) {
        let workgroups = PARTICLES.div_ceil(WORKGROUP);
        for run in &mut self.runs {
            let state = self.states.get_mut(&run.id).expect("planned cloth state");
            for k in 0..run.steps {
                let offset = u32::try_from(k * self.stride).expect("bounded step settings");
                for pipeline in std::iter::once(&self.predict)
                    .chain(std::iter::repeat_n(&self.constrain, ITERATIONS as usize))
                {
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, &state.groups[state.current], &[offset]);
                    pass.dispatch_workgroups(workgroups, 1, 1);
                    state.current ^= 1;
                }
            }
            state.steps = run.target;
            run.steps = 0;
            pass.set_pipeline(&self.surface);
            pass.set_bind_group(0, &run.surface, &[]);
            pass.dispatch_workgroups(FLAG_VERTICES.div_ceil(WORKGROUP), 1, 1);
        }
    }
}

fn solver_groups(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    particles: &[wgpu::Buffer; 2],
    settings: &wgpu::Buffer,
) -> [wgpu::BindGroup; 2] {
    [0, 1].map(|i| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flag cloth solver"),
            layout,
            entries: &[
                entry(0, particles[i].as_entire_binding()),
                entry(1, particles[1 - i].as_entire_binding()),
                entry(
                    2,
                    wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: settings,
                        offset: 0,
                        size: NonZeroU64::new(STEP_BYTES),
                    }),
                ),
            ],
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{
        FLAG_COLUMNS, FLAG_ROWS, Flag, FlagMotion, FlagShading, MeshData, PbrMaterial,
        RevealDirection, Scene, initial_positions,
    };

    const SIZE: [f32; 2] = [3.2, 4.8];

    fn cloth(wind_speed: f32, wind_direction: f32, tension: f32) -> Cloth {
        Cloth {
            wind_speed,
            wind_direction,
            auto_direction: false,
            tension,
            slack: 0.1,
            raise: None,
            seconds: 0.0,
        }
    }

    struct Harness {
        gpu: Gpu,
        cloth: ClothGpu,
        params: wgpu::Buffer,
        vertices: wgpu::Buffer,
        ids: Vec<InstanceId>,
        size: [f32; 2],
    }

    impl Harness {
        fn new() -> Self {
            Self::sized(SIZE)
        }

        /// Cloths of flags `size` metres.
        fn sized(size: [f32; 2]) -> Self {
            let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
            assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
            let mut scene = Scene::new().unwrap();
            let grid = scene.add_mesh(MeshData::flag_grid()).unwrap();
            let material = scene.add_material(PbrMaterial::default()).unwrap();
            let ids = (0..3)
                .map(|_| {
                    scene
                        .add_instance(grid, vec![material], ModelMatrix::IDENTITY)
                        .unwrap()
                })
                .collect();
            let flag = Flag {
                width: size[0],
                height: size[1],
                ..flag()
            };
            let params = gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: bytemuck::cast_slice(&crate::flag::params(flag, 0.0)),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let vertices = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 64 * u64::from(FLAG_VERTICES),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            Self {
                cloth: ClothGpu::new(&gpu.device),
                gpu,
                params,
                vertices,
                ids,
                size,
            }
        }

        /// Plans, encodes and submits one frame of these cloths; whether any moved.
        fn frame(&mut self, cloths: &[(usize, Cloth)]) -> bool {
            self.cloth.begin();
            let mut moved = false;
            for &(index, cloth) in cloths {
                // Upright: flag-local Y (down the flag) along world down, +Z.
                let model = ModelMatrix::new([
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 0.0, -1.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 0.0, 1.0],
                ])
                .unwrap();
                let draw = ClothDraw::new(self.ids[index], self.size, cloth, model);
                assert_eq!(draw.gravity, [0.0, GRAVITY, 0.0]);
                moved |= self
                    .cloth
                    .plan(&self.gpu, draw, &self.params, &self.vertices);
            }
            self.cloth.finish();
            let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
            self.cloth
                .encode(&mut encoder.begin_compute_pass(&Default::default()));
            self.gpu.queue.submit([encoder.finish()]);
            moved
        }

        /// `seconds` of 50 fps frames of each cloth from its rest arc.
        fn run(&mut self, cloths: &[(usize, Cloth)], seconds: f64) {
            for frame in 0..=(seconds * 50.0).round() as u32 {
                let at = |&(index, cloth): &(usize, Cloth)| {
                    (
                        index,
                        Cloth {
                            seconds: f64::from(frame) / 50.0,
                            ..cloth
                        },
                    )
                };
                self.frame(&cloths.iter().map(at).collect::<Vec<_>>());
            }
        }

        fn read(&self, buffer: &wgpu::Buffer) -> Vec<f32> {
            let readback = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: buffer.size(),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
            encoder.copy_buffer_to_buffer(buffer, 0, &readback, 0, buffer.size());
            self.gpu.queue.submit([encoder.finish()]);
            readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, |r| r.unwrap());
            self.gpu
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            let data = readback.slice(..).get_mapped_range().unwrap();
            bytemuck::cast_slice(&data).to_vec()
        }

        /// Positions of cloth `index`'s particles.
        fn positions(&self, index: usize) -> Vec<[f32; 3]> {
            let state = &self.cloth.states[&self.ids[index]];
            self.read(&state.particles[state.current])
                .as_chunks::<8>()
                .0
                .iter()
                .map(|p| [p[0], p[1], p[2]])
                .collect()
        }
    }

    fn flag() -> Flag {
        Flag {
            width: SIZE[0],
            height: SIZE[1],
            motion: FlagMotion::Cloth(cloth(0.0, 0.0, 0.5)),
            shading: FlagShading::Pbr,
            artwork_turns: 0,
            revealed: 1.0,
            reveal_direction: RevealDirection::CenterHorizontal,
        }
    }

    fn rest() -> Vec<[f32; 3]> {
        initial_positions(SIZE[0], SIZE[1], 0.1)
            .into_iter()
            .map(|p| p.map(|x| x as f32))
            .collect()
    }

    /// Mean flag-local displacement of the free particles from rest.
    fn drift(positions: &[[f32; 3]]) -> [f64; 3] {
        let free = CLOTH_COLUMNS as usize..(PARTICLES - CLOTH_COLUMNS) as usize;
        let rest = rest();
        let count = free.len() as f64;
        [0, 1, 2].map(|axis| {
            free.clone()
                .map(|i| f64::from(positions[i][axis] - rest[i][axis]))
                .sum::<f64>()
                / count
        })
    }

    #[test]
    #[ignore = "requires a hardware GPU"]
    fn strong_wind_keeps_the_pins_finite_and_in_bounds_at_either_tension() {
        let mut harness = Harness::new();
        let gusty = |tension| Cloth {
            auto_direction: true,
            ..cloth(18.0, 25.0, tension)
        };
        harness.run(&[(0, gusty(0.0)), (1, gusty(1.0))], 6.0);
        let bounds = flag().local_bounds().unwrap();
        let rest = rest();
        let pinned =
            |i: usize| i < CLOTH_COLUMNS as usize || i >= (PARTICLES - CLOTH_COLUMNS) as usize;
        for index in [0, 1] {
            let positions = harness.positions(index);
            for (i, p) in positions.iter().enumerate() {
                assert!(p.iter().all(|x| x.is_finite()), "particle {i}: {p:?}");
                for axis in 0..3 {
                    let x = f64::from(p[axis]);
                    assert!(
                        bounds.min()[axis] <= x && x <= bounds.max()[axis],
                        "particle {i}: {p:?}"
                    );
                }
                if pinned(i) {
                    assert_eq!(*p, rest[i], "pinned particle {i} moved");
                }
            }
            let [x, _, z] = drift(&positions);
            assert!(x.hypot(z) > 0.2, "the wind moved cloth {index}: {x} {z}");
        }
        assert_ne!(
            harness.positions(0),
            harness.positions(1),
            "tension matters"
        );
    }

    #[test]
    #[ignore = "requires a hardware GPU"]
    fn the_wind_blows_the_cloth_along_its_direction() {
        let mut harness = Harness::new();
        let blown = |harness: &mut Harness, direction| {
            harness.run(&[(0, cloth(9.0, direction, 0.65))], 3.0);
            drift(&harness.positions(0))
        };
        let right = blown(&mut harness, 0.0);
        let left = blown(&mut harness, 180.0);
        assert!(right[0] > 0.05 && left[0] < -0.05, "{right:?} {left:?}");
        // Towards the side that reads the artwork (-Z), and away from it.
        let front = blown(&mut harness, 90.0);
        let back = blown(&mut harness, -90.0);
        assert!(front[2] < back[2] - 0.2, "{front:?} {back:?}");
    }

    #[test]
    #[ignore = "requires a hardware GPU"]
    fn the_clock_restarts_warms_up_and_caps_each_frame() {
        let mut harness = Harness::new();
        let breeze = cloth(7.0, 25.0, 0.65);
        let at = |seconds| Cloth { seconds, ..breeze };
        // A cloth first seen late catches up at most its warm-up; the same frames
        // simulate the same cloth.
        assert!(harness.frame(&[(0, at(10.0)), (1, at(10.0))]));
        assert_eq!(harness.cloth.states[&harness.ids[0]].steps, 1200);
        assert_eq!(harness.positions(0), harness.positions(1), "deterministic");
        // The same time again takes no step and leaves the surface as it was.
        assert!(!harness.frame(&[(0, at(10.0))]));
        assert!(
            !harness.cloth.states.contains_key(&harness.ids[1]),
            "forgotten"
        );
        // A stall is skipped after eight steps; the clock stays the scene's.
        harness.frame(&[(0, at(20.0))]);
        assert_eq!(harness.cloth.states[&harness.ids[0]].steps, 2400);
        // Time going back restarts from the rest arc.
        assert!(harness.frame(&[(0, at(0.0))]));
        assert_eq!(harness.positions(0), rest());
        // So does a new slack, from its own rest shape: without slack it hangs flat.
        let taut = Cloth {
            slack: 0.0,
            ..at(0.0)
        };
        assert!(harness.frame(&[(0, taut)]));
        assert!(harness.positions(0).iter().all(|p| p[2] == 0.0));
    }

    #[test]
    #[ignore = "requires a hardware GPU"]
    fn a_raised_cloth_unfolds_as_its_top_edge_rises_and_then_holds() {
        let mut harness = Harness::new();
        let raised = Cloth {
            raise: Some(2.0),
            ..cloth(12.0, 25.0, 0.65)
        };
        let top = |positions: &[[f32; 3]]| positions[..CLOTH_COLUMNS as usize].to_vec();
        let bottom =
            |positions: &[[f32; 3]]| positions[(PARTICLES - CLOTH_COLUMNS) as usize..].to_vec();
        harness.frame(&[(0, raised)]);
        let folded = harness.positions(0);
        let start: Vec<[f32; 3]> = raised
            .start_positions(SIZE[0], SIZE[1])
            .into_iter()
            .map(|p| p.map(|x| x as f32))
            .collect();
        assert_eq!(folded, start, "it starts folded");
        let bounds = flag().local_bounds().unwrap();
        for frame in 1..=150_u32 {
            let seconds = f64::from(frame) / 50.0;
            harness.frame(&[(0, Cloth { seconds, ..raised })]);
            if frame != 50 && frame != 150 {
                continue;
            }
            let positions = harness.positions(0);
            let expected = raised.top_at(SIZE[1], seconds) as f32;
            for (column, p) in top(&positions).into_iter().enumerate() {
                assert_eq!(p[1], expected, "{seconds} s: top row at its anchor");
                assert!((p[0] - column as f32 * SIZE[0] / 64.0).abs() < 1e-5 && p[2] == 0.0);
            }
            assert_eq!(bottom(&positions), bottom(&start), "the bottom edge stays");
            for p in &positions {
                assert!(p.iter().all(|x| x.is_finite()));
                assert!((0..3).all(|i| {
                    bounds.min()[i] <= f64::from(p[i]) && f64::from(p[i]) <= bounds.max()[i]
                }));
            }
        }
        assert_eq!(
            top(&harness.positions(0))[0][1],
            0.0,
            "raised and held at Y = 0"
        );
    }

    #[test]
    #[ignore = "requires a hardware GPU"]
    fn the_surface_samples_the_particles_onto_the_flag_grid() {
        let mut harness = Harness::new();
        harness.run(&[(0, cloth(12.0, 60.0, 0.3))], 2.0);
        let particles = harness.positions(0);
        let vertices = harness.read(&harness.vertices);
        let vertex = |column: u32, row: u32| {
            let i = 16 * (row * (FLAG_COLUMNS + 1) + column) as usize;
            &vertices[i..i + 16]
        };
        let particle = |column: u32, row: u32| particles[(row * CLOTH_COLUMNS + column) as usize];
        // Grid points that fall on particles: the corners and the centre.
        for ((gc, gr), (pc, pr)) in [
            ((0, 0), (0, 0)),
            (
                (FLAG_COLUMNS, FLAG_ROWS),
                (CLOTH_COLUMNS - 1, CLOTH_ROWS - 1),
            ),
            (
                (FLAG_COLUMNS / 2, FLAG_ROWS / 2),
                (CLOTH_COLUMNS / 2, CLOTH_ROWS / 2),
            ),
        ] {
            let v = vertex(gc, gr);
            let p = particle(pc, pr);
            assert!((0..3).all(|i| (v[i] - p[i]).abs() < 1e-5), "{v:?} vs {p:?}");
        }
        for v in vertices.as_chunks::<16>().0 {
            assert!(v.iter().all(|x| x.is_finite()));
            let length = (v[4] * v[4] + v[5] * v[5] + v[6] * v[6]).sqrt();
            assert!((length - 1.0).abs() < 1e-3, "unit normal: {length}");
        }
    }

    #[test]
    #[ignore = "requires a hardware GPU"]
    fn a_larger_flag_folds_as_the_simulated_cloth_scaled_to_it() {
        let gusty = Cloth {
            auto_direction: true,
            ..cloth(16.0, -13.0, 0.24)
        };
        let surface = |size: [f32; 2]| {
            let mut harness = Harness::sized(size);
            harness.run(&[(0, gusty)], 1.0);
            harness.read(&harness.vertices)
        };
        // A power-of-two scale keeps the simulated size, and so the simulation, exact.
        let simulated = surface(SIZE);
        let large = surface(SIZE.map(|x| 4.0 * x));
        let pairs = simulated
            .as_chunks::<16>()
            .0
            .iter()
            .zip(large.as_chunks::<16>().0);
        for (s, l) in pairs {
            assert!((0..3).all(|i| (l[i] - 4.0 * s[i]).abs() <= 1e-5 * (1.0 + l[i].abs())));
            assert!(
                (4..16).all(|i| (l[i] - s[i]).abs() <= 1e-5),
                "{s:?} vs {l:?}"
            );
        }
    }
}
