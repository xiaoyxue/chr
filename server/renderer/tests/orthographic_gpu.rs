use std::sync::Arc;

use chr_renderer::{
    CascadeSettings, EnvironmentSettings, Gpu, MeshRenderer, OffscreenTarget, PbrSettings,
    ShadowMode, ShadowSettings, ToneMap,
    camera::{CameraView, OrthographicCamera},
    coordinates::ImageExtent,
    scene::{EnvironmentMap, Light, Lighting, MeshData, ModelMatrix, PbrMaterial, Scene, SkyLight},
};

fn camera(z: f64) -> OrthographicCamera {
    OrthographicCamera::from_pose(
        ImageExtent::new(128, 128).unwrap(),
        [8.0; 2],
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, z],
            [0.0, 0.0, 0.0, 1.0],
        ],
        2.0,
        34.0,
    )
    .unwrap()
}

fn gpu() -> Gpu {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    let info = gpu.adapter.get_info();
    eprintln!(
        "orthographic GPU: {} ({:?}, {:?})",
        info.name, info.device_type, info.backend
    );
    assert_ne!(info.device_type, wgpu::DeviceType::Cpu);
    gpu
}

fn quad(scene: &mut Scene, half_size: f32, z: f32, material: PbrMaterial) {
    let s = half_size;
    let mesh = scene
        .add_mesh(
            MeshData::new(
                vec![[-s, -s, z], [s, -s, z], [s, s, z], [-s, s, z]],
                vec![0, 2, 1, 0, 3, 2],
                std::iter::once(0..6).collect(),
            )
            .unwrap(),
        )
        .unwrap();
    let material = scene.add_material(material).unwrap();
    scene
        .add_instance(mesh, vec![material], ModelMatrix::IDENTITY)
        .unwrap();
}

fn render(
    gpu: &Gpu,
    renderer: &mut MeshRenderer,
    scene: &Scene,
    camera: &dyn CameraView,
    settings: Option<PbrSettings>,
) -> Vec<u8> {
    match settings {
        Some(settings) => renderer
            .prepare_pbr(gpu, &scene.snapshot(), camera, settings)
            .unwrap(),
        None => renderer.prepare(gpu, &scene.snapshot(), camera).unwrap(),
    }
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

fn rgb(pixels: &[u8], x: usize, y: usize) -> [u8; 3] {
    pixels[(y * 128 + x) * 4..(y * 128 + x) * 4 + 3]
        .try_into()
        .unwrap()
}

fn settings() -> PbrSettings {
    PbrSettings {
        tone_map: ToneMap::Reinhard,
        environment: EnvironmentSettings {
            resolution: 64,
            samples: 64,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
#[ignore = "requires a hardware GPU"]
fn orthographic_raster_size_clipping_and_depth_occlusion() {
    let gpu = gpu();
    let camera = camera(-10.0);
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let red = PbrMaterial {
        base_color: [1.0, 0.0, 0.0, 1.0],
        ..Default::default()
    };
    let green = PbrMaterial {
        base_color: [0.0, 1.0, 0.0, 1.0],
        ..Default::default()
    };
    let mut previous = None;
    for z in [-7.0, 0.0, 20.0] {
        let mut scene = Scene::new().unwrap();
        quad(&mut scene, 1.0, z, red);
        let pixels = render(&gpu, &mut renderer, &scene, &camera, None);
        assert_eq!(rgb(&pixels, 64, 64), [255, 0, 0]);
        assert_eq!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[..3] == [255, 0, 0])
                .count(),
            32 * 32
        );
        if let Some(previous) = &previous {
            assert_eq!(
                &pixels, previous,
                "projected size and coverage must be depth-independent"
            );
        }
        previous = Some(pixels);
    }
    // Nearer first, farther second: the winner must be depth, not draw order.
    let mut scene = Scene::new().unwrap();
    quad(&mut scene, 0.5, -5.0, green);
    quad(&mut scene, 1.0, 0.0, red);
    let image = render(&gpu, &mut renderer, &scene, &camera, None);
    assert_eq!(rgb(&image, 64, 64), [0, 255, 0]);
    assert_eq!(rgb(&image, 52, 64), [255, 0, 0]);
    for z in [-11.0, -8.5, 25.0] {
        let mut clipped = Scene::new().unwrap();
        quad(&mut clipped, 1.0, z, green);
        let image = render(&gpu, &mut renderer, &clipped, &camera, None);
        assert!(
            !image
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[..3] == [0, 255, 0]),
            "depth {z} must clip"
        );
    }
}

#[test]
#[ignore = "requires a hardware GPU"]
fn orthographic_specular_and_sky_use_parallel_world_directions() {
    let gpu = gpu();
    let camera = camera(-3.0);
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let mut scene = Scene::new().unwrap();
    quad(
        &mut scene,
        3.8,
        0.0,
        PbrMaterial {
            metallic: 1.0,
            roughness: 0.25,
            ..Default::default()
        },
    );
    scene
        .set_lighting(
            Lighting::new(
                vec![Light::Directional {
                    direction: [0.0, 0.0, 1.0],
                    irradiance: [1.0; 3],
                    angular_radius: 0.0,
                    shadow_mode: None,
                }],
                None,
            )
            .unwrap(),
        )
        .unwrap();
    let direct = render(&gpu, &mut renderer, &scene, &camera, Some(settings()));
    let center = rgb(&direct, 64, 64);
    assert!(center[0] > 150);
    for (x, y) in [(20, 30), (105, 95), (80, 12)] {
        assert_eq!(
            rgb(&direct, x, y),
            center,
            "finite-eye vectors produce a spurious moving highlight"
        );
    }

    // A directional HDR panorama, with independent world-axis channels.
    let mut pixels = Vec::new();
    for y in 0..32 {
        let theta = (f64::from(y) + 0.5) * std::f64::consts::PI / 32.0;
        for x in 0..64 {
            let phi = ((f64::from(x) + 0.5) / 64.0 - 0.5) * std::f64::consts::TAU;
            pixels.push([
                (2.0 + 1.8 * theta.sin() * phi.cos()) as f32,
                (2.0 + 1.8 * theta.sin() * phi.sin()) as f32,
                (2.0 - 1.8 * theta.cos()) as f32,
            ]);
        }
    }
    let environment =
        Arc::new(EnvironmentMap::new(ImageExtent::new(64, 32).unwrap(), pixels).unwrap());
    let sky = SkyLight {
        environment,
        gain: 1.0,
        rotation: 0.0,
        visible: true,
    };
    scene
        .set_lighting(Lighting::new(Vec::new(), Some(sky.clone())).unwrap())
        .unwrap();
    let ibl = render(&gpu, &mut renderer, &scene, &camera, Some(settings()));
    let center = rgb(&ibl, 64, 64);
    for (x, y) in [(20, 30), (105, 95), (80, 12)] {
        assert_eq!(
            rgb(&ibl, x, y),
            center,
            "IBL reflections must also use constant V"
        );
    }
    let mut background = Scene::new().unwrap();
    background
        .set_lighting(Lighting::new(Vec::new(), Some(sky.clone())).unwrap())
        .unwrap();
    let sky_z = render(&gpu, &mut renderer, &background, &camera, Some(settings()));
    let pixel = rgb(&sky_z, 64, 64);
    assert!(
        pixel[2] > pixel[0] + 8,
        "+Z sky looks down, not along the reflected -Z direction"
    );
    assert!(sky_z.as_chunks::<4>().0.iter().all(|p| p[..3] == pixel));
    // Optical +Z -> world +X. Translation and span cannot change sky rays.
    for (x, span) in [(0.0, 8.0), (37.0, 23.0)] {
        let side = OrthographicCamera::from_pose(
            camera.extent(),
            [span, 5.0],
            [
                [0.0, 0.0, 1.0, x],
                [1.0, 0.0, 0.0, 2.0],
                [0.0, 1.0, 0.0, -3.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            2.0,
            34.0,
        )
        .unwrap();
        let image = render(&gpu, &mut renderer, &background, &side, Some(settings()));
        let pixel = rgb(&image, 64, 64);
        assert!(pixel[0] > pixel[1] + 8 && pixel[0] > pixel[2] + 8);
        assert!(image.as_chunks::<4>().0.iter().all(|p| p[..3] == pixel));
        let mut rotated = sky.clone();
        rotated.rotation = std::f64::consts::FRAC_PI_2;
        background
            .set_lighting(Lighting::new(Vec::new(), Some(rotated)).unwrap())
            .unwrap();
        let image = render(&gpu, &mut renderer, &background, &side, Some(settings()));
        let pixel = rgb(&image, 64, 64);
        assert!(pixel[1] > pixel[0] + 8 && pixel[1] > pixel[2] + 8);
        assert!(image.as_chunks::<4>().0.iter().all(|p| p[..3] == pixel));
        background
            .set_lighting(Lighting::new(Vec::new(), Some(sky.clone())).unwrap())
            .unwrap();
    }
}

#[test]
#[ignore = "requires a hardware GPU"]
fn orthographic_cascades_follow_optical_depth_not_constant_clip_w() {
    let gpu = gpu();
    let camera = camera(0.0);
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    for depth in [8.0, 16.0, 24.0, 32.0] {
        let mut scene = Scene::new().unwrap();
        quad(
            &mut scene,
            3.5,
            depth,
            PbrMaterial {
                roughness: 1.0,
                ..Default::default()
            },
        );
        quad(
            &mut scene,
            0.5,
            depth - 2.0,
            PbrMaterial {
                roughness: 1.0,
                ..Default::default()
            },
        );
        scene
            .set_lighting(
                Lighting::new(
                    vec![Light::Directional {
                        direction: [0.7, 0.0, 1.0],
                        irradiance: [2.0; 3],
                        angular_radius: 0.0,
                        shadow_mode: None,
                    }],
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        let unshadowed = render(&gpu, &mut renderer, &scene, &camera, Some(settings()));
        let mut settings = settings();
        settings.shadows = ShadowSettings {
            mode: ShadowMode::Hard,
            resolution: 512,
            normal_bias: 0.0,
            depth_bias: 0.00001,
            cascades: CascadeSettings {
                count: 4,
                max_distance: 34.0,
                split_lambda: 0.0,
                blend_fraction: 0.0,
            },
            ..Default::default()
        };
        let shadowed = render(&gpu, &mut renderer, &scene, &camera, Some(settings));
        let (pixel, _) = camera.project([1.4, 0.0, f64::from(depth)]).unwrap();
        let x = pixel[0] as usize;
        assert!(rgb(&unshadowed, x, 64)[0] > 100);
        assert!(
            rgb(&shadowed, x, 64)[0] + 80 < rgb(&unshadowed, x, 64)[0],
            "cascade receiver at depth {depth} must be shadowed"
        );
        assert_eq!(
            rgb(&shadowed, 30, 64),
            rgb(&unshadowed, 30, 64),
            "outside the caster footprint remains lit"
        );
    }
}
