//! Area-light shadow evaluation: one centre cubemap with PCSS sized by the emitter's
//! equivalent radius, against a ray-cast reference of the shared GGX/Burley material.
//! World coordinates: metres, ground Z=0, upwards -Z.

use chr_renderer::{
    Gpu, MeshRenderer, OffscreenTarget, PbrSettings, PcssSettings, ShadowMode, ShadowSettings,
    ToneMap,
    camera::Camera,
    coordinates::ImageExtent,
    scene::{AreaShape, Light, Lighting, MeshData, ModelMatrix, PbrMaterial, Scene},
};
use std::f64::consts::PI;

const WIDTH: u32 = 97;
const HEIGHT: u32 = 9;

#[derive(Clone, Copy, Debug)]
enum Occluder {
    /// Horizontal square plate: centre XY, height above ground, half size.
    Plate([f64; 2], f64, f64),
    /// Axis-aligned box resting on the ground: centre XY, half size, height.
    Box([f64; 2], f64, f64),
}

impl Occluder {
    fn mesh(self) -> MeshData {
        let (positions, indices): (Vec<[f32; 3]>, Vec<u32>) = match self {
            Self::Plate(c, h, s) => (
                [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
                    .map(|(x, y)| [(c[0] + x * s) as f32, (c[1] + y * s) as f32, -h as f32])
                    .to_vec(),
                vec![0, 1, 2, 0, 2, 3],
            ),
            Self::Box(c, s, h) => {
                let p = (0..8)
                    .map(|i| {
                        [
                            (c[0] + if i & 1 == 0 { -s } else { s }) as f32,
                            (c[1] + if i & 2 == 0 { -s } else { s }) as f32,
                            if i & 4 == 0 { 0.0 } else { -h as f32 },
                        ]
                    })
                    .collect();
                let faces = [
                    [0, 1, 3, 2],
                    [4, 5, 7, 6],
                    [0, 1, 5, 4],
                    [2, 3, 7, 6],
                    [0, 2, 6, 4],
                    [1, 3, 7, 5],
                ];
                let indices = faces
                    .iter()
                    .flat_map(|f| [f[0], f[1], f[2], f[0], f[2], f[3]])
                    .collect();
                (p, indices)
            }
        };
        let count = indices.len() as u32;
        MeshData::new(positions, indices, std::iter::once(0..count).collect()).unwrap()
    }

    /// Whether the open segment from `a` to `b` crosses the occluder.
    fn blocks(self, a: [f64; 3], b: [f64; 3]) -> bool {
        match self {
            Self::Plate(c, h, s) => {
                let t = (-h - a[2]) / (b[2] - a[2]);
                if !(1e-9..1.0 - 1e-9).contains(&t) {
                    return false;
                }
                let x = a[0] + t * (b[0] - a[0]);
                let y = a[1] + t * (b[1] - a[1]);
                (x - c[0]).abs() <= s && (y - c[1]).abs() <= s
            }
            Self::Box(c, s, h) => {
                let lo = [c[0] - s, c[1] - s, -h];
                let hi = [c[0] + s, c[1] + s, 0.0];
                let (mut t0, mut t1): (f64, f64) = (1e-6, 1.0 - 1e-9);
                for axis in 0..3 {
                    let d = b[axis] - a[axis];
                    if d.abs() < 1e-15 {
                        if a[axis] < lo[axis] || a[axis] > hi[axis] {
                            return false;
                        }
                        continue;
                    }
                    let (u, v) = ((lo[axis] - a[axis]) / d, (hi[axis] - a[axis]) / d);
                    t0 = t0.max(u.min(v));
                    t1 = t1.min(u.max(v));
                }
                t0 < t1
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Case {
    name: &'static str,
    shape: AreaShape,
    /// Emitter centre height above the ground; planar emitters face down.
    height: f64,
    occluders: Vec<Occluder>,
    eye: [f64; 3],
    roughness: f32,
    metallic: f32,
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
fn unit(v: [f64; 3]) -> [f64; 3] {
    let l = dot(v, v).sqrt();
    v.map(|x| x / l)
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}

/// Shared material BRDF times the receiver cosine for a ground receiver (normal -Z).
fn brdf_cos(case: &Case, v: [f64; 3], l: [f64; 3]) -> f64 {
    let n = [0.0, 0.0, -1.0];
    let (nl, nv) = (dot(n, l), dot(n, v).max(1e-4));
    if nl <= 0.0 {
        return 0.0;
    }
    let h = unit(std::array::from_fn(|i| v[i] + l[i]));
    let (rough, metal) = (f64::from(case.roughness), f64::from(case.metallic));
    let a2 = rough.powi(4);
    let nh = dot(n, h);
    let d = a2 / (PI * (nh * nh * (a2 - 1.0) + 1.0).powi(2));
    let vis =
        0.5 / (nl * (nv * nv * (1.0 - a2) + a2).sqrt() + nv * (nl * nl * (1.0 - a2) + a2).sqrt());
    let vh = dot(v, h).max(0.0);
    let f0 = 0.04 * (1.0 - metal) + 0.7 * metal;
    let fresnel = f0 + (1.0 - f0) * (1.0 - vh).powi(5);
    let fd90 = 0.5 + 2.0 * rough * vh * vh;
    let diffuse =
        (1.0 + (fd90 - 1.0) * (1.0 - nl).powi(5)) * (1.0 + (fd90 - 1.0) * (1.0 - nv).powi(5)) / PI;
    nl * (0.7 * (1.0 - metal) * (1.0 - fresnel) * diffuse + fresnel * d * vis)
}

/// (shadowed, unshadowed) reference radiance at a ground receiver for unit radiance.
fn reference(case: &Case, receiver: [f64; 3], n: usize) -> (f64, f64) {
    let v = unit(sub(case.eye, receiver));
    let center = [0.0, 0.0, -case.height];
    let blocked = |target: [f64; 3]| case.occluders.iter().any(|o| o.blocks(receiver, target));
    let (mut lit, mut open) = (0.0, 0.0);
    if let AreaShape::Sphere { radius } = case.shape {
        let to = sub(center, receiver);
        let d = dot(to, to).sqrt();
        let w = to.map(|x| x / d);
        let u = unit([w[2], 0.0, -w[0]]);
        let b = [
            w[1] * u[2] - w[2] * u[1],
            w[2] * u[0] - w[0] * u[2],
            w[0] * u[1] - w[1] * u[0],
        ];
        let r = f64::from(radius);
        let cos_max = (1.0 - (r / d).powi(2)).sqrt();
        let weight = 2.0 * PI * (1.0 - cos_max) / (n * n) as f64;
        for i in 0..n {
            let c = 1.0 - (1.0 - cos_max) * (i as f64 + 0.5) / n as f64;
            let s = (1.0 - c * c).sqrt();
            for j in 0..n {
                let phi = 2.0 * PI * (j as f64 + 0.5) / n as f64;
                let l: [f64; 3] =
                    std::array::from_fn(|k| w[k] * c + s * (u[k] * phi.cos() + b[k] * phi.sin()));
                // Nearest hit on the sphere along l.
                let t = d * c - (r * r - d * d * (1.0 - c * c)).max(0.0).sqrt();
                let f = brdf_cos(case, v, l) * weight;
                open += f;
                if !blocked(std::array::from_fn(|k| receiver[k] + t * l[k])) {
                    lit += f;
                }
            }
        }
        return (lit, open);
    }
    let area = case.shape.area();
    for i in 0..n {
        for j in 0..n {
            let (a, b) = ((i as f64 + 0.5) / n as f64, (j as f64 + 0.5) / n as f64);
            let (x, y) = match case.shape {
                AreaShape::Rectangle {
                    half_width,
                    half_height,
                } => (
                    (2.0 * a - 1.0) * f64::from(half_width),
                    (2.0 * b - 1.0) * f64::from(half_height),
                ),
                AreaShape::Disk { radius } => {
                    let r = f64::from(radius) * a.sqrt();
                    (r * (2.0 * PI * b).cos(), r * (2.0 * PI * b).sin())
                }
                AreaShape::Sphere { .. } => unreachable!(),
            };
            // Tangent +X; normal +Z (down, towards the ground) gives height axis -Y.
            let p = [x, -y, -case.height];
            let to = sub(p, receiver);
            let r2 = dot(to, to);
            let l = to.map(|c| c / r2.sqrt());
            let emitter = (-l[2]).max(0.0);
            let f = brdf_cos(case, v, l) * emitter / r2 * area / (n * n) as f64;
            open += f;
            if !blocked(p) {
                lit += f;
            }
        }
    }
    (lit, open)
}

struct Harness {
    gpu: Gpu,
    renderer: MeshRenderer,
}

impl Harness {
    fn new() -> Self {
        let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
        assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
        let renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
        Self { gpu, renderer }
    }

    fn scene(case: &Case) -> Scene {
        let mut scene = Scene::new().unwrap();
        let ground = scene
            .add_mesh(
                MeshData::new(
                    vec![
                        [-30.0, -30.0, 0.0],
                        [30.0, -30.0, 0.0],
                        [30.0, 30.0, 0.0],
                        [-30.0, 30.0, 0.0],
                    ],
                    vec![0, 2, 1, 0, 3, 2],
                    std::iter::once(0..6).collect(),
                )
                .unwrap(),
            )
            .unwrap();
        let material = scene
            .add_material(PbrMaterial {
                base_color: [0.7, 0.7, 0.7, 1.0],
                metallic: case.metallic,
                roughness: case.roughness,
                double_sided: true,
                ..Default::default()
            })
            .unwrap();
        scene
            .add_instance(ground, vec![material], ModelMatrix::IDENTITY)
            .unwrap();
        for occluder in &case.occluders {
            let mesh = scene.add_mesh(occluder.mesh()).unwrap();
            scene
                .add_instance(mesh, vec![material], ModelMatrix::IDENTITY)
                .unwrap();
        }
        scene
            .set_lighting(
                Lighting::new(
                    vec![Light::Area {
                        position: [0.0, 0.0, -case.height],
                        // Planar emitters face the ground (+Z); spheres ignore it.
                        normal: [0.0, 0.0, 1.0],
                        tangent: [1.0, 0.0, 0.0],
                        shape: case.shape,
                        radiance: [1.0; 3],
                        two_sided: false,
                        shadow_mode: None,
                    }],
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        scene
    }

    fn camera(case: &Case) -> Camera {
        Camera::look_at(
            ImageExtent::new(WIDTH, HEIGHT).unwrap(),
            case.eye,
            [0.0; 3],
            [0.0, 0.0, -1.0],
            0.15,
            0.1,
            100.0,
        )
        .unwrap()
    }

    /// Decoded scene-linear middle row.
    fn render(&mut self, scene: &Scene, camera: &Camera, shadows: ShadowSettings) -> Vec<f64> {
        self.renderer
            .prepare_pbr(
                &self.gpu,
                &scene.snapshot(),
                camera,
                PbrSettings {
                    tone_map: ToneMap::Reinhard,
                    exposure: 4.0,
                    shadows,
                    ..Default::default()
                },
            )
            .unwrap();
        let target = OffscreenTarget::new(&self.gpu, camera.extent()).unwrap();
        let mut pixels = Vec::new();
        target
            .render_rgba(
                &self.gpu,
                |encoder, view| self.renderer.encode(encoder, view),
                &mut pixels,
            )
            .unwrap();
        let row = (HEIGHT / 2 * WIDTH * 4) as usize;
        (0..WIDTH as usize)
            .map(|x| decode(pixels[row + 4 * x]) / 4.0)
            .collect()
    }
}

fn decode(code: u8) -> f64 {
    let value = f64::from(code) / 255.0;
    let linear = if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    };
    linear / (1.0 - linear).max(1e-9)
}

fn cases() -> Vec<Case> {
    let rectangle = AreaShape::Rectangle {
        half_width: 1.0,
        half_height: 0.6,
    };
    let disk = AreaShape::Disk { radius: 1.0 };
    let sphere = AreaShape::Sphere { radius: 0.8 };
    let elongated = AreaShape::Rectangle {
        half_width: 2.0,
        half_height: 0.25,
    };
    let eye = [0.0, -7.0, -6.0];
    let grazing = [0.0, -9.0, -2.5];
    let mut cases = Vec::new();
    for (shape_name, shape) in [("rectangle", rectangle), ("disk", disk), ("sphere", sphere)] {
        let base = Case {
            name: shape_name,
            shape,
            height: 3.0,
            occluders: vec![Occluder::Plate([0.0, 0.0], 1.2, 0.6)],
            eye,
            roughness: 0.6,
            metallic: 0.0,
        };
        cases.push(Case {
            name: "plate",
            ..base.clone()
        });
        cases.push(Case {
            name: "contact_box",
            occluders: vec![Occluder::Box([0.0, 0.0], 0.5, 0.8)],
            ..base.clone()
        });
        cases.push(Case {
            name: "large_penumbra",
            occluders: vec![Occluder::Plate([0.0, 0.0], 2.2, 0.5)],
            ..base.clone()
        });
        cases.push(Case {
            name: "multiple_blockers",
            occluders: vec![
                Occluder::Plate([-0.6, 0.0], 0.8, 0.4),
                Occluder::Plate([0.7, 0.0], 1.8, 0.3),
            ],
            ..base.clone()
        });
        cases.push(Case {
            name: "grazing",
            eye: grazing,
            ..base.clone()
        });
        cases.push(Case {
            name: "glossy_metal",
            // The emitter's mirror reflection lands near the measured row.
            eye: [0.0, -1.0, -6.0],
            roughness: 0.35,
            metallic: 1.0,
            ..base.clone()
        });
        // Outside the camera's narrow view, its shadow crosses the measured row.
        cases.push(Case {
            name: "offscreen_occluder",
            occluders: vec![Occluder::Plate([1.5, 0.0], 1.5, 0.4)],
            ..base.clone()
        });
    }
    let base = Case {
        name: "elongated",
        shape: elongated,
        height: 3.0,
        occluders: vec![Occluder::Plate([0.0, 0.0], 1.2, 0.6)],
        eye,
        roughness: 0.6,
        metallic: 0.0,
    };
    cases.push(base.clone());
    cases.push(Case {
        name: "elongated_rotated_view",
        eye: [-7.0, 0.0, -6.0],
        ..base
    });
    cases
}

#[derive(Default)]
struct Stats {
    max: f64,
    sum: f64,
    count: usize,
}

/// Combined (max, mean penumbra) visibility-ratio error for one shadow configuration.
/// With `trace`, prints each case and enforces the per-case `area-shadow-pcss-v1` limit.
fn evaluate(harness: &mut Harness, shadows: ShadowSettings, trace: bool) -> Stats {
    let mut all = Stats::default();
    for case in cases() {
        let scene = Harness::scene(&case);
        let camera = Harness::camera(&case);
        let shadowed = harness.render(&scene, &camera, shadows);
        let open = harness.render(
            &scene,
            &camera,
            ShadowSettings {
                mode: ShadowMode::Off,
                ..shadows
            },
        );
        let mut stats = Stats::default();
        for x in 0..WIDTH as usize {
            let ray = camera.ray([x as f64, f64::from(HEIGHT / 2)]).unwrap();
            let t = -ray.origin[2] / ray.direction[2];
            let receiver: [f64; 3] = std::array::from_fn(|i| ray.origin[i] + t * ray.direction[i]);
            // Skip pixels that see an occluder instead of the ground.
            if case
                .occluders
                .iter()
                .any(|o| o.blocks(receiver, ray.origin))
            {
                continue;
            }
            let (lit, unoccluded) = reference(&case, receiver, 96);
            if unoccluded < 0.01 || open[x] < 0.01 {
                continue;
            }
            let reference_visibility = lit / unoccluded;
            let error = (shadowed[x] / open[x] - reference_visibility).abs();
            stats.max = stats.max.max(error);
            if (0.02..0.98).contains(&reference_visibility) {
                stats.sum += error;
                stats.count += 1;
            }
        }
        if trace {
            println!(
                "  {},{:?},max={:.4},mean_penumbra={:.4},penumbra_pixels={}",
                case.name,
                case.shape,
                stats.max,
                stats.sum / stats.count.max(1) as f64,
                stats.count
            );
        }
        assert!(
            stats.count >= 3,
            "{} {:?} must cross a penumbra",
            case.name,
            case.shape
        );
        if trace {
            let mean = stats.sum / stats.count as f64;
            assert!(
                mean <= 0.35,
                "{} {:?}: penumbra mean {mean}",
                case.name,
                case.shape
            );
        }
        all.max = all.max.max(stats.max);
        all.sum += stats.sum;
        all.count += stats.count;
    }
    all
}

#[test]
#[ignore = "requires a hardware GPU"]
fn area_shadow_pcss_visibility_error_against_ray_cast_reference() {
    let mut harness = Harness::new();
    println!("adapter: {:?}", harness.gpu.adapter.get_info().name);
    // The default 64-texel caps clip area-light penumbrae; recorded for contrast only.
    let default = evaluate(
        &mut harness,
        ShadowSettings {
            mode: ShadowMode::Pcss,
            ..Default::default()
        },
        false,
    );
    println!(
        "default settings: max={:.4} mean_penumbra={:.4}",
        default.max,
        default.sum / default.count as f64
    );
    let stats = evaluate(&mut harness, recommended(), true);
    let mean = stats.sum / stats.count as f64;
    println!(
        "recommended settings: max={:.4} mean_penumbra={mean:.4} penumbra_pixels={}",
        stats.max, stats.count
    );
    // Owner-approved area-shadow-pcss-v1 limits (2026-11-12), set above measured values.
    assert!(stats.max <= 0.65, "max visibility error {}", stats.max);
    assert!(mean <= 0.15, "mean penumbra visibility error {mean}");
}

fn with_samples(samples: u32) -> ShadowSettings {
    let mut settings = recommended();
    settings.pcss.blocker_samples = samples;
    settings.pcss.filter_samples = samples;
    settings
}

/// Proposed area-light settings: 1024 maps (24 MiB per six-face cubemap, so eight
/// area lights fit the default 256 MiB budget), search/filter caps of 512 texels
/// (about 1 rad on a cubemap face) and 16 + 16 PCSS samples.
fn recommended() -> ShadowSettings {
    ShadowSettings {
        mode: ShadowMode::Pcss,
        resolution: 1024,
        pcss: PcssSettings {
            blocker_samples: 16,
            filter_samples: 16,
            max_search_radius: 512.0,
            max_filter_radius: 512.0,
        },
        ..Default::default()
    }
}

#[test]
#[ignore = "requires a hardware GPU"]
fn area_shadows_follow_moving_occluders_and_skip_emitter_meshes() {
    let mut harness = Harness::new();
    let case = cases().into_iter().find(|c| c.name == "plate").unwrap();
    let camera = Harness::camera(&case);
    let mut scene = Harness::scene(&case);
    let before = harness.render(&scene, &camera, recommended());
    assert!(harness.renderer.shadow_passes_last_frame() > 0);
    harness.render(&scene, &camera, recommended());
    assert_eq!(
        harness.renderer.shadow_passes_last_frame(),
        0,
        "unchanged scene reuses maps"
    );
    // Moving the occluder must invalidate the cached cubemap.
    let moved = Case {
        occluders: vec![Occluder::Plate([1.0, 0.0], 1.2, 0.6)],
        ..case.clone()
    };
    scene = Harness::scene(&moved);
    let after = harness.render(&scene, &camera, recommended());
    assert!(harness.renderer.shadow_passes_last_frame() > 0);
    assert_ne!(before, after);
    let mut fresh = Harness::new();
    assert_eq!(fresh.render(&scene, &camera, recommended()), after);

    // Emitter meshes: inside a sphere emitter, or in a planar emitter's plane.
    for (shape, emitter) in [
        (
            AreaShape::Sphere { radius: 0.8 },
            Occluder::Box([0.0, 0.0], 0.5, 3.5),
        ),
        (
            AreaShape::Rectangle {
                half_width: 1.0,
                half_height: 0.6,
            },
            Occluder::Plate([0.0, 0.0], 3.0, 0.55),
        ),
    ] {
        let open = Case {
            shape,
            occluders: Vec::new(),
            ..case.clone()
        };
        let shadowed = |occluders| Case {
            occluders,
            ..open.clone()
        };
        let lit = harness.render(&Harness::scene(&open), &camera, recommended());
        let mut mesh_scene = Harness::scene(&shadowed(vec![]));
        let mesh = mesh_scene.add_mesh(emitter_mesh(emitter)).unwrap();
        let material = mesh_scene.add_material(PbrMaterial::default()).unwrap();
        mesh_scene
            .add_instance(mesh, vec![material], ModelMatrix::IDENTITY)
            .unwrap();
        let with_mesh = harness.render(&mesh_scene, &camera, recommended());
        let visible = (0..WIDTH as usize).filter(|&x| lit[x] > 0.01);
        for x in visible {
            assert!(
                (with_mesh[x] - lit[x]).abs() <= 0.02 * lit[x] + 1e-3,
                "{shape:?} emitter mesh shadows pixel {x}: {} vs {}",
                with_mesh[x],
                lit[x]
            );
        }
    }
}

/// An emitter stand-in: a box from the ground cannot sit inside a raised sphere, so
/// shrink it to a cube around the sphere centre; plates stay in the emitter plane.
fn emitter_mesh(occluder: Occluder) -> MeshData {
    match occluder {
        Occluder::Box(c, s, _) => {
            let p: Vec<[f32; 3]> = (0..8)
                .map(|i| {
                    [
                        (c[0] + if i & 1 == 0 { -s } else { s }) as f32,
                        (c[1] + if i & 2 == 0 { -s } else { s }) as f32,
                        (-3.0 + if i & 4 == 0 { -s } else { s }) as f32,
                    ]
                })
                .collect();
            let faces = [
                [0, 1, 3, 2],
                [4, 5, 7, 6],
                [0, 1, 5, 4],
                [2, 3, 7, 6],
                [0, 2, 6, 4],
                [1, 3, 7, 5],
            ];
            let indices: Vec<u32> = faces
                .iter()
                .flat_map(|f| [f[0], f[1], f[2], f[0], f[2], f[3]])
                .collect();
            let count = indices.len() as u32;
            MeshData::new(p, indices, std::iter::once(0..count).collect()).unwrap()
        }
        plate => plate.mesh(),
    }
}

fn timed_gpu() -> (Gpu, wgpu::QuerySet, wgpu::Buffer, wgpu::Buffer) {
    let instance = Gpu::instance();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .unwrap();
    let features =
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    assert!(
        adapter.features().contains(features),
        "timestamp queries required"
    );
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .unwrap();
    let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: None,
        ty: wgpu::QueryType::Timestamp,
        count: 2,
    });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    (
        Gpu {
            adapter,
            device,
            queue,
        },
        queries,
        resolve,
        readback,
    )
}

/// Median GPU time of whole 1080p frames (shadow passes, shading, tone map). Cold frames
/// rebuild the cubemaps; warm frames reuse them. Diagnostic only, not a frame budget.
#[test]
#[ignore = "requires a hardware GPU with timestamp queries"]
fn area_light_frame_cost() {
    let (gpu, queries, resolve, readback) = timed_gpu();
    println!("adapter: {:?}", gpu.adapter.get_info().name);
    let extent = ImageExtent::new(1920, 1080).unwrap();
    let camera = Camera::look_at(
        extent,
        [0.0, -9.0, -6.0],
        [0.0; 3],
        [0.0, 0.0, -1.0],
        0.8,
        0.1,
        100.0,
    )
    .unwrap();
    let target = OffscreenTarget::new(&gpu, extent).unwrap();
    println!("lights,shadows,cold_ms,warm_ms");
    for lights in [1_usize, 4, 8] {
        let case = cases()
            .into_iter()
            .find(|c| c.name == "multiple_blockers")
            .unwrap();
        let mut scene = Harness::scene(&case);
        let shapes = [
            AreaShape::Rectangle {
                half_width: 1.0,
                half_height: 0.6,
            },
            AreaShape::Disk { radius: 1.0 },
            AreaShape::Sphere { radius: 0.8 },
        ];
        let area: Vec<_> = (0..lights)
            .map(|i| {
                let angle = i as f64 * std::f64::consts::TAU / lights as f64;
                Light::Area {
                    position: [2.0 * angle.cos(), 2.0 * angle.sin(), -3.0],
                    normal: [0.0, 0.0, 1.0],
                    tangent: [1.0, 0.0, 0.0],
                    shape: shapes[i % 3],
                    radiance: [1.0; 3],
                    two_sided: false,
                    shadow_mode: None,
                }
            })
            .collect();
        scene
            .set_lighting(Lighting::new(area, None).unwrap())
            .unwrap();
        for shadows in [
            ShadowSettings {
                mode: ShadowMode::Off,
                ..recommended()
            },
            recommended(),
            with_samples(32),
        ] {
            let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
            let mut measure = |renderer: &mut MeshRenderer, rebuild: bool| {
                let mut samples = Vec::new();
                for _ in 0..8 {
                    if rebuild {
                        // A new snapshot invalidates cached shadow maps.
                        scene
                            .set_lighting(scene.snapshot().lighting().clone())
                            .unwrap();
                    }
                    renderer
                        .prepare_pbr(
                            &gpu,
                            &scene.snapshot(),
                            &camera,
                            PbrSettings {
                                shadows,
                                ..Default::default()
                            },
                        )
                        .unwrap();
                    let mut pixels = Vec::new();
                    target
                        .render_rgba(
                            &gpu,
                            |encoder, view| {
                                encoder.write_timestamp(&queries, 0);
                                renderer.encode(encoder, view);
                                encoder.write_timestamp(&queries, 1);
                                encoder.resolve_query_set(&queries, 0..2, &resolve, 0);
                                encoder.copy_buffer_to_buffer(&resolve, 0, &readback, 0, 16);
                            },
                            &mut pixels,
                        )
                        .unwrap();
                    readback
                        .slice(..)
                        .map_async(wgpu::MapMode::Read, |r| r.unwrap());
                    gpu.device
                        .poll(wgpu::PollType::Wait {
                            submission_index: None,
                            timeout: None,
                        })
                        .unwrap();
                    let ticks = {
                        let bytes = readback.slice(..).get_mapped_range().unwrap();
                        u64::from_le_bytes(bytes[8..16].try_into().unwrap())
                            - u64::from_le_bytes(bytes[0..8].try_into().unwrap())
                    };
                    readback.unmap();
                    if rebuild && shadows.mode != ShadowMode::Off {
                        assert!(renderer.shadow_passes_last_frame() > 0);
                    }
                    samples.push(ticks as f64 * f64::from(gpu.queue.get_timestamp_period()) / 1e6);
                }
                // Drop the priming frame, then take the median of seven.
                let mut retained = samples[1..].to_vec();
                retained.sort_by(f64::total_cmp);
                retained[3]
            };
            let cold = measure(&mut renderer, true);
            let warm = measure(&mut renderer, false);
            println!(
                "{lights},{:?}/{},{cold:.3},{warm:.3}",
                shadows.mode, shadows.pcss.filter_samples
            );
        }
    }
}
