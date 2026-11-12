//! Experimental `ltc-plausible-v1` area lights through the shared mesh renderer.
//! Fixtures are written in a local shading frame (receiver normal +Z, view in +X) and
//! mapped to world coordinates by (x, y, z) -> (x, -y, -z), so world up is -Z.

use chr_renderer::{
    Gpu, MeshRenderer, OffscreenTarget, PbrSettings, ToneMap,
    camera::Camera,
    coordinates::ImageExtent,
    scene::{AreaShape, Light, Lighting, MeshData, ModelMatrix, PbrMaterial, Scene},
};
use std::f64::consts::PI;

const SIZE: u32 = 33;

fn world(v: [f64; 3]) -> [f64; 3] {
    [v[0], -v[1], -v[2]]
}

#[derive(Clone, Copy)]
struct Fixture {
    shape: AreaShape,
    /// Local emitter centre and planar front normal.
    center: [f64; 3],
    normal: [f64; 3],
    two_sided: bool,
    radiance: f32,
    roughness: f32,
    metallic: f32,
    nv: f64,
}

fn fixture(shape: AreaShape, center: [f64; 3], roughness: f32, metallic: f32, nv: f64) -> Fixture {
    Fixture {
        shape,
        center,
        normal: [0.0, 0.0, -1.0],
        two_sided: false,
        radiance: 1.0,
        roughness,
        metallic,
        nv,
    }
}

struct Harness {
    gpu: Gpu,
    renderer: MeshRenderer,
}

impl Harness {
    fn new() -> Self {
        let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
        assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
        println!("adapter: {:?}", gpu.adapter.get_info());
        let renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
        Self { gpu, renderer }
    }

    /// Scene-linear radiance at the receiver (the centre pixel), decoded from Reinhard/sRGB.
    fn render(&mut self, f: Fixture) -> (f64, f64) {
        let mut scene = Scene::new().unwrap();
        let mesh = scene
            .add_mesh(
                MeshData::new(
                    vec![
                        [-20.0, -20.0, 0.0],
                        [20.0, -20.0, 0.0],
                        [20.0, 20.0, 0.0],
                        [-20.0, 20.0, 0.0],
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
                metallic: f.metallic,
                roughness: f.roughness,
                ..Default::default()
            })
            .unwrap();
        scene
            .add_instance(mesh, vec![material], ModelMatrix::IDENTITY)
            .unwrap();
        scene
            .set_lighting(
                Lighting::new(
                    vec![Light::Area {
                        position: world(f.center),
                        normal: world(f.normal),
                        tangent: [1.0, 0.0, 0.0],
                        shape: f.shape,
                        radiance: [f.radiance; 3],
                        two_sided: f.two_sided,
                        shadow_mode: None,
                    }],
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        let view = world([(1.0 - f.nv * f.nv).sqrt(), 0.0, f.nv]);
        let camera = Camera::look_at(
            ImageExtent::new(SIZE, SIZE).unwrap(),
            view.map(|v| 6.0 * v),
            [0.0; 3],
            [0.0, 0.0, -1.0],
            0.05,
            0.1,
            100.0,
        )
        .unwrap();
        self.renderer
            .prepare_pbr(
                &self.gpu,
                &scene.snapshot(),
                &camera,
                PbrSettings {
                    tone_map: ToneMap::Reinhard,
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
        let center = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
        let code = pixels[center];
        assert!(pixels[center..center + 3].iter().all(|&c| c == code));
        // Half a code step either side bounds 8-bit quantization of the decoded value.
        let step = (decode(code.saturating_add(1)) - decode(code.saturating_sub(1))) / 2.0;
        (decode(code), step)
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

fn unit(v: [f64; 3]) -> [f64; 3] {
    let length = v.iter().map(|x| x * x).sum::<f64>().sqrt();
    v.map(|x| x / length)
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Shared GGX/Burley material (mesh.wgsl `brdf`) times the receiver cosine, in f64.
fn brdf_cos(f: Fixture, l: [f64; 3]) -> f64 {
    if l[2] <= 0.0 {
        return 0.0;
    }
    let v = [(1.0 - f.nv * f.nv).sqrt(), 0.0, f.nv];
    let h = unit([v[0] + l[0], v[1] + l[1], v[2] + l[2]]);
    let (rough, metal) = (f64::from(f.roughness), f64::from(f.metallic));
    let a2 = rough.powi(4);
    let d = a2 / (PI * (h[2] * h[2] * (a2 - 1.0) + 1.0).powi(2));
    let vis = 0.5
        / (l[2] * (f.nv * f.nv * (1.0 - a2) + a2).sqrt()
            + f.nv * (l[2] * l[2] * (1.0 - a2) + a2).sqrt());
    let vh = dot(v, h).max(0.0);
    let f0 = 0.04 * (1.0 - metal) + 0.7 * metal;
    let fresnel = f0 + (1.0 - f0) * (1.0 - vh).powi(5);
    let fd90 = 0.5 + 2.0 * rough * vh * vh;
    let diffuse = (1.0 + (fd90 - 1.0) * (1.0 - l[2]).powi(5))
        * (1.0 + (fd90 - 1.0) * (1.0 - f.nv).powi(5))
        / PI;
    l[2] * (0.7 * (1.0 - metal) * (1.0 - fresnel) * diffuse + fresnel * d * vis)
}

/// Independent high-sample reference: area sampling for planes, exact cone for spheres.
fn reference(f: Fixture, n: usize) -> f64 {
    let le = f64::from(f.radiance);
    let c = f.center;
    let mut sum = 0.0;
    if let AreaShape::Sphere { radius } = f.shape {
        let d = dot(c, c).sqrt();
        let w = c.map(|x| x / d);
        let u = unit(cross([0.0, 1.0, 0.0], w));
        let v = cross(w, u);
        let cos_max = (1.0 - (f64::from(radius) / d).powi(2)).sqrt();
        for i in 0..n {
            let cosine = 1.0 - (1.0 - cos_max) * (i as f64 + 0.5) / n as f64;
            let sine = (1.0 - cosine * cosine).sqrt();
            for j in 0..n {
                let phi = 2.0 * PI * (j as f64 + 0.5) / n as f64;
                let l: [f64; 3] = std::array::from_fn(|k| {
                    w[k] * cosine + sine * (u[k] * phi.cos() + v[k] * phi.sin())
                });
                sum += brdf_cos(f, l);
            }
        }
        return le * sum * 2.0 * PI * (1.0 - cos_max) / (n * n) as f64;
    }
    let normal = unit(f.normal);
    let u = [1.0, 0.0, 0.0];
    let w = cross(normal, u);
    for i in 0..n {
        for j in 0..n {
            let (a, b) = ((i as f64 + 0.5) / n as f64, (j as f64 + 0.5) / n as f64);
            let (x, y) = match f.shape {
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
            let p: [f64; 3] = std::array::from_fn(|k| c[k] + x * u[k] + y * w[k]);
            let r2 = dot(p, p);
            let l = p.map(|v| v / r2.sqrt());
            let cosine = -dot(normal, l);
            let emitter = if f.two_sided {
                cosine.abs()
            } else {
                cosine.max(0.0)
            };
            sum += brdf_cos(f, l) * emitter / r2;
        }
    }
    le * sum * f.shape.area() / (n * n) as f64
}

#[test]
#[ignore = "requires a hardware GPU"]
fn area_lights_reproduce_the_pinned_ltc_candidate() {
    let mut harness = Harness::new();
    let rectangle = AreaShape::Rectangle {
        half_width: 1.0,
        half_height: 0.6,
    };
    let disk = AreaShape::Disk { radius: 1.0 };
    let sphere = AreaShape::Sphere { radius: 1.0 };
    // LTC64 values recorded for the six counterexamples in area-light-evaluation.md
    // (metal, base 0.7, NdotV 0.3, centre (0, 0, 2), unit radiance).
    for (shape, roughness, ltc64) in [
        (rectangle, 0.15, 0.0003075307),
        (disk, 0.15, 0.0003096965),
        (sphere, 0.15, 0.0004052331),
        (rectangle, 0.5, 0.0296018885),
        (disk, 0.5, 0.0323913665),
        (sphere, 0.5, 0.0414708538),
    ] {
        let radiance = (0.5 / ltc64) as f32;
        let f = Fixture {
            radiance,
            ..fixture(shape, [0.0, 0.0, 2.0], roughness, 1.0, 0.3)
        };
        let (value, step) = harness.render(f);
        let expected = ltc64 * f64::from(radiance);
        assert!(
            (value - expected).abs() <= 0.015 * expected + step,
            "{shape:?} roughness {roughness}: {value} vs {expected}"
        );
    }
}

#[test]
#[ignore = "requires a hardware GPU"]
fn area_lights_meet_the_plausible_profile_against_the_shared_material() {
    let mut harness = Harness::new();
    let tilted = unit([0.4, 0.0, -1.0]);
    for f in [
        fixture(
            AreaShape::Rectangle {
                half_width: 1.0,
                half_height: 0.6,
            },
            [-1.0, 0.0, 2.5],
            0.5,
            0.0,
            0.5,
        ),
        Fixture {
            normal: tilted,
            ..fixture(
                AreaShape::Rectangle {
                    half_width: 0.8,
                    half_height: 0.5,
                },
                [1.0, 0.5, 3.0],
                0.3,
                0.0,
                0.8,
            )
        },
        fixture(
            AreaShape::Disk { radius: 1.0 },
            [-1.5, 0.5, 3.0],
            0.3,
            1.0,
            0.8,
        ),
        fixture(
            AreaShape::Disk { radius: 0.7 },
            [0.5, -1.0, 2.0],
            0.85,
            0.0,
            0.3,
        ),
        fixture(
            AreaShape::Sphere { radius: 1.0 },
            [1.0, 1.0, 2.5],
            0.85,
            0.0,
            0.3,
        ),
        // Smooth metal sphere centred on the mirror direction: a bright, narrow highlight.
        fixture(
            AreaShape::Sphere { radius: 0.5 },
            [-3.5 * 0.75_f64.sqrt(), 0.0, 1.75],
            0.15,
            1.0,
            0.5,
        ),
    ] {
        let expected = reference(f, 768);
        let (value, step) = harness.render(f);
        let limit = 0.02 * f64::from(f.radiance) + 0.30 * expected + step;
        println!(
            "{:?} roughness {} metal {} nv {}: gpu {value:.5} reference {expected:.5} limit {limit:.5}",
            f.shape, f.roughness, f.metallic, f.nv
        );
        assert!(
            (value - expected).abs() <= limit,
            "{:?}: {value} vs reference {expected} (limit {limit})",
            f.shape
        );
        assert!(expected > 0.01, "fixture must be visibly lit: {expected}");
    }
}

#[test]
#[ignore = "requires a hardware GPU"]
fn area_light_sidedness_interior_and_size() {
    let mut harness = Harness::new();
    let rectangle = AreaShape::Rectangle {
        half_width: 1.0,
        half_height: 0.6,
    };
    let front = fixture(rectangle, [0.0, 0.0, 2.0], 0.5, 0.0, 0.5);
    let (lit, _) = harness.render(front);
    assert!(lit > 0.05);
    let back = Fixture {
        normal: [0.0, 0.0, 1.0],
        ..front
    };
    assert_eq!(
        harness.render(back).0,
        0.0,
        "one-sided back face emits nothing"
    );
    let two_sided = Fixture {
        two_sided: true,
        ..back
    };
    assert_eq!(harness.render(two_sided).0, lit, "two-sided back face");
    let inside = fixture(
        AreaShape::Sphere { radius: 1.0 },
        [0.0, 0.0, 0.5],
        0.5,
        0.0,
        0.5,
    );
    assert_eq!(
        harness.render(inside).0,
        0.0,
        "outward sphere around the receiver"
    );
    let larger = Fixture {
        shape: AreaShape::Rectangle {
            half_width: 2.0,
            half_height: 1.2,
        },
        ..front
    };
    let (enlarged, _) = harness.render(larger);
    assert!(enlarged > 1.5 * lit, "fixed radiance: {enlarged} vs {lit}");
    // Keeping total power fixed quarters the radiance of a doubled rectangle.
    let flux = (PI * rectangle.area()) as f32;
    assert!((rectangle.radiance_from_flux([flux; 3], false)[0] - 1.0).abs() < 1e-6);
    let quarter = larger.shape.radiance_from_flux([flux; 3], false)[0];
    assert!((quarter - 0.25).abs() < 1e-6);
    let (fixed_power, step) = harness.render(Fixture {
        radiance: quarter,
        ..larger
    });
    assert!(
        (fixed_power - 0.25 * enlarged).abs() <= 2.0 * step,
        "{fixed_power} vs {enlarged}"
    );
}

/// Moving emitters and slowly varying views must not pop: sampled second differences stay
/// within 8-bit quantization plus a small curvature allowance.
#[test]
#[ignore = "requires a hardware GPU"]
fn area_lighting_is_continuous_along_trajectories() {
    let mut harness = Harness::new();
    let shapes = [
        AreaShape::Rectangle {
            half_width: 1.0,
            half_height: 0.6,
        },
        AreaShape::Disk { radius: 1.0 },
        AreaShape::Sphere { radius: 1.0 },
    ];
    for shape in shapes {
        for (metallic, roughness) in [(1.0, 0.3), (0.0, 0.6)] {
            // A light crossing the receiver's zenith, then a view sweep across LUT cells.
            let moving: Vec<_> = (0..=40)
                .map(|i| {
                    let x = -2.0 + 0.1 * f64::from(i);
                    harness.render(fixture(shape, [x, 0.3, 2.5], roughness, metallic, 0.5))
                })
                .collect();
            let viewing: Vec<_> = (0..=40)
                .map(|i| {
                    let nv = 0.3 + 0.0125 * f64::from(i);
                    harness.render(fixture(shape, [-1.0, 0.0, 2.5], roughness, metallic, nv))
                })
                .collect();
            for (name, series) in [("trajectory", moving), ("view", viewing)] {
                let range = series.iter().map(|s| s.0).fold(0.0, f64::max)
                    - series.iter().map(|s| s.0).fold(f64::MAX, f64::min);
                for w in series.windows(3) {
                    let second = (w[0].0 - 2.0 * w[1].0 + w[2].0).abs();
                    let allowance = 2.0 * (w[0].1 + w[1].1 + w[2].1) + 0.05 * range;
                    assert!(
                        second <= allowance,
                        "{shape:?} {name} metal {metallic}: {:?} (allowance {allowance})",
                        w.iter().map(|s| s.0).collect::<Vec<_>>()
                    );
                }
            }
        }
    }
}
