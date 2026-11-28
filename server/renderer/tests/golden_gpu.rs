//! Golden verification renders: fixed scenes compared with committed PNGs in
//! `tests/golden/`. Each scene covers one renderer feature; a regression in any of them
//! changes its pixels. `CHR_UPDATE_GOLDEN=1` rewrites the goldens after an intended
//! change; a failing comparison writes the actual image to `output/renderer/golden/`.
use chr_renderer::{
    EnvironmentSettings, Gpu, MeshRenderer, OffscreenTarget, PbrSettings, ShadowMode,
    ShadowSettings, ToneMap,
    camera::Camera,
    coordinates::ImageExtent,
    scene::{
        AreaShape, EnvironmentMap, Filter, Flag, FlagMotion, FlagShading, Light, Lighting, MapSlot,
        MaterialLayers, MaterialMaps, MeshAttributes, MeshData, ModelMatrix, PbrMaterial, Scene,
        SkyLight, TextureData, TextureSampler, UvTransform, Wave, Wrap,
    },
};
use std::{path::Path, sync::Arc};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 96;
/// Largest difference in 8-bit code values that still matches, and the share of
/// channel values allowed beyond it (rasterization of silhouettes and shadow edges).
const TOLERANCE: u8 = 2;
const OUTLIERS: f64 = 0.002;

fn gpu() -> Gpu {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    gpu
}

fn camera() -> Camera {
    Camera::look_at(
        ImageExtent::new(WIDTH, HEIGHT).unwrap(),
        [0.0, -9.0, -3.5],
        [0.0, 0.0, -0.9],
        [0.0, 0.0, -1.0],
        0.6,
        0.1,
        100.0,
    )
    .unwrap()
}

fn render(gpu: &Gpu, scene: &Scene, camera: &Camera, shadows: ShadowMode, seconds: f64) -> Vec<u8> {
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    renderer.set_animation_time(seconds);
    renderer
        .prepare_pbr(
            gpu,
            &scene.snapshot(),
            camera,
            PbrSettings {
                tone_map: ToneMap::Reinhard,
                shadows: ShadowSettings {
                    mode: shadows,
                    resolution: 1024,
                    ..Default::default()
                },
                environment: EnvironmentSettings {
                    resolution: 128,
                    samples: 64,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    let mut pixels = Vec::new();
    OffscreenTarget::new(gpu, camera.extent())
        .unwrap()
        .render_rgba(
            gpu,
            |encoder, view| renderer.encode(encoder, view),
            &mut pixels,
        )
        .unwrap();
    pixels
}

fn check(name: &str, pixels: &[u8]) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("tests/golden").join(format!("{name}.png"));
    let save = |path: &Path| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        image::save_buffer(path, pixels, WIDTH, HEIGHT, image::ColorType::Rgba8).unwrap();
    };
    if std::env::var_os("CHR_UPDATE_GOLDEN").is_some() {
        save(&path);
        return;
    }
    let golden = image::open(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .to_rgba8();
    assert_eq!(golden.dimensions(), (WIDTH, HEIGHT), "{name}");
    let differences: Vec<u8> = pixels
        .iter()
        .zip(golden.as_raw())
        .map(|(a, b)| a.abs_diff(*b))
        .collect();
    let outliers = differences.iter().filter(|&&d| d > TOLERANCE).count();
    if outliers as f64 > OUTLIERS * differences.len() as f64 {
        let actual = root
            .join("../../output/renderer/golden")
            .join(format!("{name}.png"));
        save(&actual);
        panic!(
            "{name}: {outliers} of {} channel values differ by more than {TOLERANCE} \
             (worst {}); actual image at {}",
            differences.len(),
            differences.iter().max().unwrap(),
            actual.display()
        );
    }
}

fn texture(width: u32, height: u32, texel: impl Fn(u32, u32) -> [u8; 4]) -> Arc<TextureData> {
    let pixels = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .flat_map(|(x, y)| texel(x, y))
        .collect();
    Arc::new(TextureData::new(ImageExtent::new(width, height).unwrap(), pixels).unwrap())
}

/// A ground quad at Z = 0, facing up (-Z), with texture coordinates over [0, 1].
fn quad(half: f32) -> MeshData {
    MeshData::new(
        vec![
            [-half, -half, 0.0],
            [half, -half, 0.0],
            [half, half, 0.0],
            [-half, half, 0.0],
        ],
        vec![0, 2, 1, 0, 3, 2],
        std::iter::once(0..6).collect(),
    )
    .unwrap()
    .with_attributes(MeshAttributes {
        normals: vec![[0.0, 0.0, -1.0]; 4],
        tex_coords: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        ..Default::default()
    })
    .unwrap()
}

/// A unit-radius UV sphere at the origin with outward normals; U runs around +Z,
/// V from the top (-Z) to the bottom.
fn sphere() -> MeshData {
    let (stacks, slices) = (24_u32, 48_u32);
    let mut positions = Vec::new();
    let mut attributes = MeshAttributes::default();
    for i in 0..=stacks {
        let theta = std::f32::consts::PI * i as f32 / stacks as f32;
        for j in 0..=slices {
            let phi = std::f32::consts::TAU * j as f32 / slices as f32;
            let n = [
                theta.sin() * phi.cos(),
                theta.sin() * phi.sin(),
                -theta.cos(),
            ];
            positions.push(n);
            attributes.normals.push(n);
            attributes
                .tex_coords
                .push([j as f32 / slices as f32, i as f32 / stacks as f32]);
        }
    }
    let mut indices = Vec::new();
    let row = slices + 1;
    for i in 0..stacks {
        for j in 0..slices {
            let [a, b, c, d] = [
                i * row + j,
                (i + 1) * row + j,
                (i + 1) * row + j + 1,
                i * row + j + 1,
            ];
            for [p, q, r] in [[a, b, c], [a, c, d]] {
                let [p0, p1, p2] = [p, q, r].map(|k| positions[k as usize]);
                let e1: [f32; 3] = std::array::from_fn(|k| p1[k] - p0[k]);
                let e2: [f32; 3] = std::array::from_fn(|k| p2[k] - p0[k]);
                let cross = [
                    e1[1] * e2[2] - e1[2] * e2[1],
                    e1[2] * e2[0] - e1[0] * e2[2],
                    e1[0] * e2[1] - e1[1] * e2[0],
                ];
                let centre: [f32; 3] = std::array::from_fn(|k| p0[k] + p1[k] + p2[k]);
                // Front faces wind so that the edge cross product points outward, as in glTF.
                if cross.iter().zip(centre).map(|(x, y)| x * y).sum::<f32>() >= 0.0 {
                    indices.extend([p, q, r]);
                } else {
                    indices.extend([p, r, q]);
                }
            }
        }
    }
    let count = indices.len() as u32;
    MeshData::new(positions, indices, std::iter::once(0..count).collect())
        .unwrap()
        .with_attributes(attributes)
        .unwrap()
}

/// Uniform scale `size` about the origin, then a move to `at`.
fn placed(at: [f64; 3], size: f64) -> ModelMatrix {
    ModelMatrix::new([
        [size, 0.0, 0.0, at[0]],
        [0.0, size, 0.0, at[1]],
        [0.0, 0.0, size, at[2]],
        [0.0, 0.0, 0.0, 1.0],
    ])
    .unwrap()
}

/// A grey ground and three spheres resting on it, left to right, each with its own
/// material; returns the scene for lights to be added.
fn spheres(materials: [PbrMaterial; 3], maps: [MaterialMaps; 3]) -> Scene {
    let mut scene = Scene::new().unwrap();
    let ground = scene.add_mesh(quad(12.0)).unwrap();
    let grey = scene
        .add_material(PbrMaterial {
            base_color: [0.45, 0.45, 0.42, 1.0],
            roughness: 0.85,
            double_sided: true,
            ..Default::default()
        })
        .unwrap();
    scene
        .add_instance(ground, vec![grey], ModelMatrix::IDENTITY)
        .unwrap();
    let ball = scene.add_mesh(sphere()).unwrap();
    for (i, (material, maps)) in materials.into_iter().zip(maps).enumerate() {
        let id = scene.add_material_with_maps(material, maps).unwrap();
        scene
            .add_instance(
                ball,
                vec![id],
                placed([2.4 * (i as f64 - 1.0), 0.0, -1.0], 1.0),
            )
            .unwrap();
    }
    scene
}

fn sun(shadow_mode: Option<ShadowMode>) -> Light {
    Light::Directional {
        direction: [0.6, 0.8, 1.0],
        irradiance: [3.0, 2.9, 2.7],
        angular_radius: 0.01,
        shadow_mode,
    }
}

fn lit(mut scene: Scene, lights: Vec<Light>, sky: Option<SkyLight>, constant: [f32; 3]) -> Scene {
    scene
        .set_lighting(
            Lighting::new(lights, sky)
                .unwrap()
                .with_constant(constant)
                .unwrap(),
        )
        .unwrap();
    scene
}

/// A 2:1 panorama: a blue-to-white sky over a dark ground and a small bright sun.
fn panorama() -> Arc<EnvironmentMap> {
    let (width, height) = (128_u32, 64_u32);
    let pixels = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let v = (y as f32 + 0.5) / height as f32;
            let u = (x as f32 + 0.5) / width as f32;
            if (u - 0.3).hypot(v - 0.25) < 0.03 {
                [40.0, 36.0, 30.0]
            } else if v < 0.5 {
                let t = v * 2.0;
                [0.3 + 0.6 * t, 0.5 + 0.4 * t, 1.0]
            } else {
                [0.15, 0.12, 0.1]
            }
        })
        .collect();
    Arc::new(EnvironmentMap::new(ImageExtent::new(width, height).unwrap(), pixels).unwrap())
}

fn three<T: Default>() -> [T; 3] {
    std::array::from_fn(|_| T::default())
}

#[test]
#[ignore = "requires a hardware GPU"]
fn golden_base_materials_under_punctual_lights_and_shadows() {
    let gpu = gpu();
    let scene = spheres(
        [
            PbrMaterial {
                base_color: [0.8, 0.1, 0.08, 1.0],
                roughness: 0.4,
                ..Default::default()
            },
            PbrMaterial {
                base_color: [0.95, 0.75, 0.35, 1.0],
                metallic: 1.0,
                roughness: 0.25,
                ..Default::default()
            },
            PbrMaterial {
                base_color: [0.9, 0.9, 0.9, 1.0],
                metallic: 0.5,
                roughness: 0.6,
                emissive: [0.0, 0.05, 0.1],
                ..Default::default()
            },
        ],
        three(),
    );
    let scene = lit(
        scene,
        vec![
            sun(None),
            Light::Point {
                position: [-3.0, -3.0, -4.0],
                intensity: [20.0, 16.0, 12.0],
                source_radius: 0.2,
                shadow_mode: Some(ShadowMode::Pcf),
            },
            Light::Spot {
                position: [3.0, -4.0, -6.0],
                direction: [-0.4, 0.6, 1.0],
                intensity: [10.0, 30.0, 40.0],
                inner_angle: 0.2,
                outer_angle: 0.5,
                source_radius: 0.1,
                shadow_mode: Some(ShadowMode::Hard),
            },
        ],
        None,
        [0.0; 3],
    );
    check(
        "base_punctual_shadows",
        &render(&gpu, &scene, &camera(), ShadowMode::Pcss, 0.0),
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn golden_base_texture_maps_and_alpha_mask() {
    let gpu = gpu();
    let checker = texture(8, 8, |x, y| {
        if (x + y) % 2 == 0 {
            [200, 60, 40, 255]
        } else {
            [40, 90, 200, 255]
        }
    });
    // Roughness rises along U, metallic along V.
    let metal_rough = texture(16, 16, |x, y| [0, (x * 16) as u8, (y * 16) as u8, 255]);
    let bumps = texture(32, 32, |x, y| {
        let (u, v) = (x as f32 / 32.0, y as f32 / 32.0);
        let (dx, dy) = (
            0.5 * (u * std::f32::consts::TAU * 4.0).cos(),
            0.5 * (v * std::f32::consts::TAU * 4.0).cos(),
        );
        let length = (dx * dx + dy * dy + 1.0).sqrt();
        [dx / length, dy / length, 1.0 / length, 1.0]
            .map(|c| ((c * 0.5 + 0.5) * 255.0).round() as u8)
    });
    let glow = texture(4, 4, |x, _| {
        if x == 1 {
            [255, 160, 40, 255]
        } else {
            [0, 0, 0, 255]
        }
    });
    let occlusion = texture(8, 8, |_, y| [(64 + y * 24) as u8, 0, 0, 255]);
    let holes = texture(8, 8, |x, y| {
        [
            255,
            255,
            255,
            if (x / 2 + y / 2) % 2 == 0 { 255 } else { 0 },
        ]
    });
    let scene = spheres(
        [
            PbrMaterial {
                roughness: 0.5,
                normal_scale: 0.8,
                ..Default::default()
            },
            PbrMaterial {
                base_color: [0.9, 0.9, 0.9, 1.0],
                emissive: [2.0, 2.0, 2.0],
                occlusion_strength: 1.0,
                ..Default::default()
            },
            PbrMaterial {
                base_color: [0.3, 0.8, 0.3, 1.0],
                alpha_cutoff: Some(0.5),
                double_sided: true,
                ..Default::default()
            },
        ],
        [
            MaterialMaps {
                base_color: Some(checker),
                normal: Some(bumps),
                ..Default::default()
            },
            MaterialMaps {
                metallic_roughness: Some(metal_rough),
                emissive: Some(glow),
                occlusion: Some(occlusion),
                ..Default::default()
            },
            MaterialMaps {
                base_color: Some(holes),
                ..Default::default()
            },
        ],
    );
    let scene = lit(
        scene,
        vec![
            sun(None),
            Light::Point {
                position: [0.0, -4.0, -3.0],
                intensity: [12.0; 3],
                source_radius: 0.0,
                shadow_mode: Some(ShadowMode::Off),
            },
        ],
        None,
        [0.05; 3],
    );
    check(
        "base_texture_maps",
        &render(&gpu, &scene, &camera(), ShadowMode::Pcf, 0.0),
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn golden_base_image_based_lighting() {
    let gpu = gpu();
    let scene = spheres(
        [
            PbrMaterial {
                base_color: [0.95, 0.95, 0.95, 1.0],
                metallic: 1.0,
                roughness: 0.05,
                ..Default::default()
            },
            PbrMaterial {
                base_color: [0.95, 0.64, 0.54, 1.0],
                metallic: 1.0,
                roughness: 0.5,
                ..Default::default()
            },
            PbrMaterial {
                base_color: [0.2, 0.3, 0.8, 1.0],
                roughness: 0.3,
                ..Default::default()
            },
        ],
        three(),
    );
    let scene = lit(
        scene,
        vec![sun(None)],
        Some(SkyLight {
            environment: panorama(),
            gain: 0.6,
            rotation: 0.4,
            visible: false,
            max_radiance: Some(20.0),
        }),
        [0.02; 3],
    );
    check(
        "base_environment",
        &render(&gpu, &scene, &camera(), ShadowMode::Pcss, 0.0),
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn golden_base_area_lights() {
    let gpu = gpu();
    let scene = spheres(
        [
            PbrMaterial {
                base_color: [0.7, 0.7, 0.7, 1.0],
                roughness: 0.3,
                ..Default::default()
            },
            PbrMaterial {
                base_color: [0.9, 0.9, 0.9, 1.0],
                metallic: 1.0,
                roughness: 0.35,
                ..Default::default()
            },
            PbrMaterial {
                base_color: [0.6, 0.2, 0.6, 1.0],
                roughness: 0.8,
                ..Default::default()
            },
        ],
        three(),
    );
    let scene = lit(
        scene,
        vec![
            Light::Area {
                position: [-2.0, -2.0, -4.0],
                normal: [0.3, 0.3, 1.0],
                tangent: [1.0, 0.0, 0.0],
                shape: AreaShape::Rectangle {
                    half_width: 1.0,
                    half_height: 0.5,
                },
                radiance: [3.0, 2.8, 2.5],
                two_sided: false,
                shadow_mode: None,
            },
            Light::Area {
                position: [3.0, -2.0, -3.0],
                normal: [-0.5, 0.3, 1.0],
                tangent: [0.0, 1.0, 0.0],
                shape: AreaShape::Disk { radius: 0.6 },
                radiance: [1.0, 2.0, 4.0],
                two_sided: true,
                shadow_mode: Some(ShadowMode::Off),
            },
        ],
        None,
        [0.0; 3],
    );
    check(
        "base_area_lights",
        &render(&gpu, &scene, &camera(), ShadowMode::Pcss, 0.0),
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn golden_base_decal_shadow_catcher_and_flag() {
    let gpu = gpu();
    let mut scene = Scene::new().unwrap();
    let ground = scene.add_mesh(quad(12.0)).unwrap();
    let catcher = scene
        .add_material(PbrMaterial {
            shadow_catcher: true,
            double_sided: true,
            ..Default::default()
        })
        .unwrap();
    scene
        .add_instance(ground, vec![catcher], ModelMatrix::IDENTITY)
        .unwrap();
    let decal_image = texture(16, 8, |x, y| {
        let inside = (2..14).contains(&x) && (2..6).contains(&y);
        if inside {
            [250, 220, 30, 255]
        } else {
            [30, 30, 200, 128]
        }
    });
    let decal = scene
        .add_material_with_maps(
            PbrMaterial {
                base_color: [1.0, 1.0, 1.0, 0.8],
                decal: true,
                double_sided: true,
                ..Default::default()
            },
            MaterialMaps {
                base_color: Some(decal_image),
                ..Default::default()
            },
        )
        .unwrap();
    let plate = scene.add_mesh(quad(0.5)).unwrap();
    let decal_model = ModelMatrix::new([
        [6.0, 0.0, 0.0, -1.5],
        [0.0, 3.0, 0.0, 1.0],
        [0.0, 0.0, 1.0, -0.01],
        [0.0, 0.0, 0.0, 1.0],
    ])
    .unwrap();
    scene.add_instance(plate, vec![decal], decal_model).unwrap();
    let ball = scene.add_mesh(sphere()).unwrap();
    let white = scene
        .add_material(PbrMaterial {
            base_color: [0.85, 0.85, 0.85, 1.0],
            roughness: 0.5,
            ..Default::default()
        })
        .unwrap();
    scene
        .add_instance(ball, vec![white], placed([-1.5, 1.0, -1.5], 0.8))
        .unwrap();
    let grid = scene.add_mesh(MeshData::flag_grid()).unwrap();
    let red = scene
        .add_material(PbrMaterial {
            base_color: [0.8, 0.05, 0.05, 1.0],
            roughness: 0.8,
            ..Default::default()
        })
        .unwrap();
    // The flag hangs in the XZ plane facing -Y, its top-left corner 3.5 m up.
    let model = ModelMatrix::new([
        [1.0, 0.0, 0.0, 1.5],
        [0.0, 0.0, -1.0, 1.0],
        [0.0, 1.0, 0.0, -3.5],
        [0.0, 0.0, 0.0, 1.0],
    ])
    .unwrap();
    let flag = scene.add_instance(grid, vec![red], model).unwrap();
    scene
        .set_flag(
            flag,
            Some(Flag {
                width: 2.4,
                height: 1.6,
                motion: FlagMotion::Wave(Wave {
                    amplitude: 0.2,
                    frequency: 1.35,
                    speed: 1.0,
                }),
                shading: FlagShading::Pbr,
                artwork_turns: 0,
                revealed: 1.0,
                reveal_direction: Default::default(),
            }),
        )
        .unwrap();
    let scene = lit(scene, vec![sun(None)], None, [0.05; 3]);
    check(
        "base_decal_catcher_flag",
        &render(&gpu, &scene, &camera(), ShadowMode::Pcf, 0.5),
    );
}

// glTF material extensions (#208): goldens over direct light and IBL, then behaviour
// checks that pin each layer's effect independently of the goldens.

fn layered(base: PbrMaterial, layers: MaterialLayers) -> PbrMaterial {
    PbrMaterial { layers, ..base }
}

fn sky() -> Option<SkyLight> {
    Some(SkyLight {
        environment: panorama(),
        gain: 0.5,
        rotation: 0.4,
        visible: false,
        max_radiance: Some(20.0),
    })
}

fn extension_golden(gpu: &Gpu, name: &str, materials: [PbrMaterial; 3], maps: [MaterialMaps; 3]) {
    let scene = lit(spheres(materials, maps), vec![sun(None)], sky(), [0.0; 3]);
    check(name, &render(gpu, &scene, &camera(), ShadowMode::Pcss, 0.0));
}

fn bumps(frequency: f32) -> Arc<TextureData> {
    texture(64, 64, move |x, y| {
        let (u, v) = (x as f32 / 64.0, y as f32 / 64.0);
        let (dx, dy) = (
            0.6 * (u * std::f32::consts::TAU * frequency).cos(),
            0.6 * (v * std::f32::consts::TAU * frequency).cos(),
        );
        let length = (dx * dx + dy * dy + 1.0).sqrt();
        [dx / length, dy / length, 1.0 / length, 1.0]
            .map(|c| ((c * 0.5 + 0.5) * 255.0).round() as u8)
    })
}

#[test]
#[ignore = "requires a hardware GPU"]
fn golden_extension_specular_and_ior() {
    let base = PbrMaterial {
        base_color: [0.35, 0.4, 0.45, 1.0],
        roughness: 0.25,
        ..Default::default()
    };
    let stripes = texture(16, 16, |x, _| {
        [255, 255, 255, if x % 4 < 2 { 255 } else { 40 }]
    });
    extension_golden(
        &gpu(),
        "extension_specular_ior",
        [
            layered(
                base,
                MaterialLayers {
                    specular: 0.0,
                    ..Default::default()
                },
            ),
            layered(
                base,
                MaterialLayers {
                    ior: 2.4,
                    specular_color: [1.0, 0.6, 0.2],
                    ..Default::default()
                },
            ),
            // Specular-glossiness compatibility: a constant, coloured reflectance.
            layered(
                PbrMaterial {
                    base_color: [0.1, 0.1, 0.1, 1.0],
                    ..base
                },
                MaterialLayers {
                    ior: 0.0,
                    specular_color: [0.2, 0.6, 0.9],
                    ..Default::default()
                },
            ),
        ],
        [
            MaterialMaps::default(),
            MaterialMaps {
                specular: Some(stripes),
                ..Default::default()
            },
            MaterialMaps::default(),
        ],
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn golden_extension_clearcoat() {
    let base = PbrMaterial {
        base_color: [0.6, 0.05, 0.05, 1.0],
        roughness: 0.8,
        ..Default::default()
    };
    let coat = MaterialLayers {
        clearcoat: 1.0,
        clearcoat_roughness: 0.05,
        ..Default::default()
    };
    extension_golden(
        &gpu(),
        "extension_clearcoat",
        [
            base,
            layered(base, coat),
            layered(
                base,
                MaterialLayers {
                    clearcoat_normal_scale: 0.6,
                    ..coat
                },
            ),
        ],
        [
            MaterialMaps::default(),
            MaterialMaps::default(),
            MaterialMaps {
                clearcoat_normal: Some(bumps(6.0)),
                ..Default::default()
            },
        ],
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn golden_extension_sheen() {
    let base = PbrMaterial {
        base_color: [0.05, 0.05, 0.2, 1.0],
        roughness: 0.9,
        ..Default::default()
    };
    extension_golden(
        &gpu(),
        "extension_sheen",
        [
            base,
            layered(
                base,
                MaterialLayers {
                    sheen_color: [1.0, 1.0, 1.0],
                    sheen_roughness: 0.3,
                    ..Default::default()
                },
            ),
            layered(
                base,
                MaterialLayers {
                    sheen_color: [1.0, 0.4, 0.7],
                    sheen_roughness: 0.8,
                    ..Default::default()
                },
            ),
        ],
        three(),
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn golden_extension_anisotropy() {
    let base = PbrMaterial {
        base_color: [0.9, 0.9, 0.9, 1.0],
        metallic: 1.0,
        roughness: 0.2,
        ..Default::default()
    };
    let anisotropic = MaterialLayers {
        anisotropy_strength: 0.9,
        ..Default::default()
    };
    // Directions along V (bitangent) in the texture: (0.5, 1.0) encodes +Y.
    let along_v = texture(4, 4, |_, _| [128, 255, 255, 255]);
    extension_golden(
        &gpu(),
        "extension_anisotropy",
        [
            base,
            layered(base, anisotropic),
            layered(
                base,
                MaterialLayers {
                    anisotropy_rotation: 0.3,
                    ..anisotropic
                },
            ),
        ],
        [
            MaterialMaps::default(),
            MaterialMaps::default(),
            MaterialMaps {
                anisotropy: Some(along_v),
                ..Default::default()
            },
        ],
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn golden_extension_iridescence() {
    let base = PbrMaterial {
        base_color: [0.05, 0.05, 0.05, 1.0],
        roughness: 0.3,
        ..Default::default()
    };
    let film = MaterialLayers {
        iridescence: 1.0,
        iridescence_thickness: [100.0, 800.0],
        ..Default::default()
    };
    let gradient = texture(16, 16, |_, y| [255, (y * 16) as u8, 0, 255]);
    extension_golden(
        &gpu(),
        "extension_iridescence",
        [
            layered(base, film),
            layered(
                PbrMaterial {
                    base_color: [0.9, 0.9, 0.9, 1.0],
                    metallic: 1.0,
                    ..base
                },
                MaterialLayers {
                    iridescence_thickness: [400.0, 400.0],
                    ..film
                },
            ),
            layered(base, film),
        ],
        [
            MaterialMaps::default(),
            MaterialMaps::default(),
            MaterialMaps {
                iridescence_thickness: Some(gradient),
                ..Default::default()
            },
        ],
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn golden_extension_texture_transform() {
    let checker = texture(8, 8, |x, y| {
        if (x + y) % 2 == 0 {
            [230, 230, 230, 255]
        } else {
            [30, 120, 60, 255]
        }
    });
    let base = PbrMaterial {
        roughness: 0.5,
        normal_scale: 1.0,
        ..Default::default()
    };
    let transformed = |rotation: f32| {
        let mut maps = MaterialMaps {
            base_color: Some(Arc::clone(&checker)),
            normal: Some(bumps(3.0)),
            ..Default::default()
        };
        for slot in [MapSlot::BaseColor, MapSlot::Normal] {
            maps.transforms[slot as usize] = UvTransform {
                offset: [0.1, 0.2],
                rotation,
                scale: [3.0, 1.5],
            };
        }
        maps
    };
    extension_golden(
        &gpu(),
        "extension_texture_transform",
        [base, base, base],
        [
            MaterialMaps {
                base_color: Some(Arc::clone(&checker)),
                normal: Some(bumps(3.0)),
                ..Default::default()
            },
            transformed(0.0),
            transformed(0.8),
        ],
    );
}

/// One unit sphere at the origin, seen head-on from -Y, lit along the view.
fn front(material: PbrMaterial, maps: MaterialMaps) -> Scene {
    let mut scene = Scene::new().unwrap();
    let ball = scene.add_mesh(sphere()).unwrap();
    let id = scene.add_material_with_maps(material, maps).unwrap();
    scene
        .add_instance(ball, vec![id], ModelMatrix::IDENTITY)
        .unwrap();
    let light = Light::Directional {
        direction: [0.0, 1.0, 0.0],
        irradiance: [2.0; 3],
        angular_radius: 0.0,
        shadow_mode: Some(ShadowMode::Off),
    };
    lit(scene, vec![light], None, [0.0; 3])
}

fn front_camera() -> Camera {
    Camera::look_at(
        ImageExtent::new(WIDTH, HEIGHT).unwrap(),
        [0.0, -4.0, 0.0],
        [0.0; 3],
        [0.0, 0.0, -1.0],
        0.6,
        0.1,
        100.0,
    )
    .unwrap()
}

fn shot(gpu: &Gpu, material: PbrMaterial, maps: MaterialMaps) -> Vec<u8> {
    render(
        gpu,
        &front(material, maps),
        &front_camera(),
        ShadowMode::Off,
        0.0,
    )
}

fn pixel(image: &[u8], x: u32, y: u32) -> [i32; 3] {
    let i = ((y * WIDTH + x) * 4) as usize;
    [0, 1, 2].map(|c| i32::from(image[i + c]))
}

fn luma(image: &[u8], x: u32, y: u32) -> i32 {
    pixel(image, x, y).iter().sum::<i32>() / 3
}

/// Pixels along a row and a column through `(x, y)` at least half as bright as it.
fn highlight_extent(image: &[u8], x: u32, y: u32) -> (usize, usize) {
    let peak = luma(image, x, y);
    let floor = luma(image, x + 30, y).max(luma(image, x, y + 30));
    let half = floor + (peak - floor) / 2;
    let row = (0..WIDTH).filter(|&i| luma(image, i, y) >= half).count();
    let column = (0..HEIGHT).filter(|&j| luma(image, x, j) >= half).count();
    (row, column)
}

const CX: u32 = WIDTH / 2;
const CY: u32 = HEIGHT / 2;

#[test]
#[ignore = "requires a hardware GPU"]
fn specular_and_ior_scale_the_dielectric_reflection() {
    let gpu = gpu();
    let base = PbrMaterial {
        base_color: [0.5, 0.5, 0.5, 1.0],
        roughness: 0.3,
        ..Default::default()
    };
    let plain = shot(&gpu, base, MaterialMaps::default());
    // Explicit default layers are the plain material.
    assert_eq!(
        shot(
            &gpu,
            layered(base, MaterialLayers::default()),
            MaterialMaps::default()
        ),
        plain
    );
    let off = shot(
        &gpu,
        layered(
            base,
            MaterialLayers {
                specular: 0.0,
                ..Default::default()
            },
        ),
        MaterialMaps::default(),
    );
    let dense = shot(
        &gpu,
        layered(
            base,
            MaterialLayers {
                ior: 2.5,
                ..Default::default()
            },
        ),
        MaterialMaps::default(),
    );
    let (p, o, d) = (
        luma(&plain, CX, CY),
        luma(&off, CX, CY),
        luma(&dense, CX, CY),
    );
    assert!(
        o + 30 < p && p + 30 < d,
        "specular 0 {o}, default {p}, IOR 2.5 {d}"
    );
    // IOR 0 with a red specular colour: the reflection itself is red.
    let red = shot(
        &gpu,
        layered(
            PbrMaterial {
                base_color: [0.02, 0.02, 0.02, 1.0],
                ..base
            },
            MaterialLayers {
                ior: 0.0,
                specular_color: [0.9, 0.05, 0.05],
                ..Default::default()
            },
        ),
        MaterialMaps::default(),
    );
    let [r, g, b] = pixel(&red, CX, CY);
    assert!(r > g + 60 && r > b + 60, "{:?}", [r, g, b]);
}

#[test]
#[ignore = "requires a hardware GPU"]
fn clearcoat_adds_a_sharp_highlight_and_dims_the_base() {
    let gpu = gpu();
    let base = PbrMaterial {
        base_color: [0.4, 0.4, 0.4, 1.0],
        roughness: 1.0,
        ..Default::default()
    };
    let plain = shot(&gpu, base, MaterialMaps::default());
    let coated = shot(
        &gpu,
        layered(
            base,
            MaterialLayers {
                clearcoat: 1.0,
                clearcoat_roughness: 0.1,
                ..Default::default()
            },
        ),
        MaterialMaps::default(),
    );
    assert!(luma(&coated, CX, CY) > luma(&plain, CX, CY) + 40);
    // Away from the highlight the coat only reflects light away from the base.
    let (x, y) = (CX + 25, CY);
    assert!(
        luma(&coated, x, y) <= luma(&plain, x, y),
        "{} > {}",
        luma(&coated, x, y),
        luma(&plain, x, y)
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn sheen_brightens_grazing_views() {
    let gpu = gpu();
    let base = PbrMaterial {
        base_color: [0.02, 0.02, 0.02, 1.0],
        roughness: 1.0,
        ..Default::default()
    };
    let plain = shot(&gpu, base, MaterialMaps::default());
    let velvet = shot(
        &gpu,
        layered(
            base,
            MaterialLayers {
                sheen_color: [1.0; 3],
                sheen_roughness: 0.4,
                ..Default::default()
            },
        ),
        MaterialMaps::default(),
    );
    // Lit along the view, Charlie sheen vanishes head-on (the half vector is the normal)
    // and peaks towards the rim, inside the silhouette where masking takes over.
    let background = pixel(&plain, 0, 0);
    let radius = (CX..WIDTH)
        .take_while(|&x| pixel(&plain, x, CY) != background)
        .count() as u32;
    let gains: Vec<i32> = (0..radius)
        .map(|r| luma(&velvet, CX + r, CY) - luma(&plain, CX + r, CY))
        .collect();
    let (peak, gain) = gains.iter().enumerate().max_by_key(|(_, g)| **g).unwrap();
    assert!(
        *gain > gains[0] + 20 && peak as u32 > radius / 2,
        "gain +{gain} at {peak} of {radius}, centre +{}",
        gains[0]
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn anisotropy_stretches_the_highlight_along_its_direction() {
    let gpu = gpu();
    let base = PbrMaterial {
        base_color: [0.9, 0.9, 0.9, 1.0],
        metallic: 1.0,
        roughness: 0.15,
        ..Default::default()
    };
    let round = highlight_extent(&shot(&gpu, base, MaterialMaps::default()), CX, CY);
    // The tangent follows U, around the sphere: horizontal at the image centre.
    let along_u = MaterialLayers {
        anisotropy_strength: 0.9,
        ..Default::default()
    };
    let wide = highlight_extent(
        &shot(&gpu, layered(base, along_u), MaterialMaps::default()),
        CX,
        CY,
    );
    let tall = highlight_extent(
        &shot(
            &gpu,
            layered(
                base,
                MaterialLayers {
                    anisotropy_rotation: std::f32::consts::FRAC_PI_2,
                    ..along_u
                },
            ),
            MaterialMaps::default(),
        ),
        CX,
        CY,
    );
    assert!(round.0.abs_diff(round.1) <= 2, "isotropic {round:?}");
    assert!(wide.0 > 2 * wide.1, "rotation 0 {wide:?}");
    assert!(tall.1 > 2 * tall.0, "rotation pi/2 {tall:?}");
}

#[test]
#[ignore = "requires a hardware GPU"]
fn iridescence_colours_a_neutral_reflection() {
    let gpu = gpu();
    let base = PbrMaterial {
        base_color: [0.02, 0.02, 0.02, 1.0],
        roughness: 0.3,
        ..Default::default()
    };
    let spread = |image: &[u8]| {
        let p = pixel(image, CX, CY);
        p.iter().max().unwrap() - p.iter().min().unwrap()
    };
    let plain = shot(&gpu, base, MaterialMaps::default());
    let film = shot(
        &gpu,
        layered(
            base,
            MaterialLayers {
                iridescence: 1.0,
                iridescence_thickness: [450.0, 450.0],
                ..Default::default()
            },
        ),
        MaterialMaps::default(),
    );
    assert!(spread(&plain) <= 3, "{:?}", pixel(&plain, CX, CY));
    assert!(spread(&film) >= 25, "{:?}", pixel(&film, CX, CY));
    // Zero thickness is no film at all.
    let thin = shot(
        &gpu,
        layered(
            base,
            MaterialLayers {
                iridescence: 1.0,
                iridescence_thickness: [0.0, 0.0],
                ..Default::default()
            },
        ),
        MaterialMaps::default(),
    );
    assert!(
        pixel(&thin, CX, CY)
            .iter()
            .zip(pixel(&plain, CX, CY))
            .all(|(a, b)| a.abs_diff(b) <= 2)
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn texture_transform_selects_the_transformed_texels() {
    let gpu = gpu();
    // Left texel red, right texel green, sampled nearest and clamped.
    let halves = Arc::new(
        TextureData::new(
            ImageExtent::new(2, 1).unwrap(),
            vec![255, 0, 0, 255, 0, 255, 0, 255],
        )
        .unwrap()
        .with_sampler(TextureSampler {
            wrap_u: Wrap::Clamp,
            wrap_v: Wrap::Clamp,
            mag: Filter::Nearest,
            min: Filter::Nearest,
            mip: None,
        }),
    );
    let unlit = PbrMaterial {
        unlit: true,
        ..Default::default()
    };
    let colours = |maps: MaterialMaps| {
        let image = shot(&gpu, unlit, maps);
        // Tone mapping keeps unlit primaries below 255; their other channels stay 0.
        let reds = image.chunks(4).filter(|p| p[0] > 100 && p[1] == 0).count();
        let greens = image.chunks(4).filter(|p| p[1] > 100 && p[0] == 0).count();
        (reds, greens)
    };
    // The visible half of the sphere has U in [0.5, 1]: the green texel.
    let identity = colours(MaterialMaps {
        base_color: Some(Arc::clone(&halves)),
        ..Default::default()
    });
    assert!(identity.0 == 0 && identity.1 > 1000, "{identity:?}");
    // u' = u - 0.5 moves it onto the red texel.
    let mut maps = MaterialMaps {
        base_color: Some(halves),
        ..Default::default()
    };
    maps.transforms[MapSlot::BaseColor as usize] = UvTransform {
        offset: [-0.5, 0.0],
        rotation: 0.0,
        scale: [1.0, 1.0],
    };
    let shifted = colours(maps);
    assert_eq!(shifted, (identity.1, 0));
}
