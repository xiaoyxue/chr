use chr_renderer::{
    EnvironmentSettings, Gpu, MeshRenderer, OffscreenTarget, PbrSettings, ToneMap,
    camera::Camera,
    coordinates::ImageExtent,
    scene::{
        EnvironmentMap, Light, Lighting, MaterialId, MaterialMaps, MeshData, ModelMatrix,
        PbrMaterial, Scene, SkyLight, TextureData,
    },
};
use std::sync::Arc;

fn render(
    gpu: &Gpu,
    renderer: &mut MeshRenderer,
    scene: &Scene,
    camera: &Camera,
    exposure: f32,
) -> Vec<u8> {
    renderer
        .prepare_pbr(
            gpu,
            &scene.snapshot(),
            camera,
            PbrSettings {
                exposure,
                tone_map: ToneMap::Reinhard,
                environment: EnvironmentSettings {
                    resolution: 64,
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

fn plane(material: PbrMaterial) -> (Scene, MaterialId) {
    let mut scene = Scene::new().unwrap();
    let mesh = scene
        .add_mesh(
            MeshData::new(
                vec![
                    [-1.0, -1.0, 0.0],
                    [1.0, -1.0, 0.0],
                    [1.0, 1.0, 0.0],
                    [-1.0, 1.0, 0.0],
                ],
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
    (scene, material)
}

fn sky(
    scene: &mut Scene,
    environment: &Arc<EnvironmentMap>,
    rotation: f64,
    gain: f32,
    visible: bool,
) {
    scene
        .set_lighting(
            Lighting::new(
                Vec::new(),
                Some(SkyLight {
                    environment: Arc::clone(environment),
                    rotation,
                    gain,
                    visible,
                }),
            )
            .unwrap(),
        )
        .unwrap();
}

fn rgb(image: &[u8], extent: ImageExtent, x: u32, y: u32) -> [u8; 3] {
    let index = (y as usize * extent.width() as usize + x as usize) * 4;
    image[index..index + 3].try_into().unwrap()
}

fn radiance(value: u8) -> f64 {
    let value = f64::from(value) / 255.0;
    let linear = if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    };
    linear / (1.0 - linear)
}

#[test]
#[ignore = "requires a hardware GPU"]
fn environment_hdr_energy_background_cache_and_ao() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let extent = ImageExtent::new(128, 128).unwrap();
    let camera = Camera::look_at(
        extent,
        [0.0, 0.0, -3.0],
        [0.0; 3],
        [0.0, -1.0, 0.0],
        1.0,
        0.1,
        20.0,
    )
    .unwrap();
    let (mut scene, material) = plane(PbrMaterial {
        roughness: 1.0,
        ..Default::default()
    });
    let environment = Arc::new(
        EnvironmentMap::new(ImageExtent::new(1, 1).unwrap(), vec![[4.0, 2.0, 1.0]]).unwrap(),
    );
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let empty = Scene::new().unwrap();
    let baseline = render(&gpu, &mut renderer, &empty, &camera, 1.0);
    sky(&mut scene, &environment, 0.0, 0.5, false);
    let hidden = render(&gpu, &mut renderer, &scene, &camera, 1.0);
    assert!(renderer.environment_preprocessed_last_prepare());
    let center = rgb(&hidden, extent, 64, 64);
    // Normal-view rough GGX contributes about F0*(1-ln(2)); diffuse is 0.96*L.
    for (pixel, light) in center.into_iter().zip([2.0, 1.0, 0.5]) {
        assert!(
            (radiance(pixel) / light - 0.9723).abs() < 0.035,
            "constant environment energy: {center:?}"
        );
    }
    assert_eq!(rgb(&hidden, extent, 0, 0), rgb(&baseline, extent, 0, 0));
    sky(&mut scene, &environment, 0.0, 0.5, true);
    let visible = render(&gpu, &mut renderer, &scene, &camera, 1.0);
    assert!(!renderer.environment_preprocessed_last_prepare());
    assert_eq!(
        rgb(&visible, extent, 64, 64),
        center,
        "background visibility cannot change illumination"
    );
    for (pixel, light) in rgb(&visible, extent, 0, 0).into_iter().zip([2.0, 1.0, 0.5]) {
        assert!((radiance(pixel) / light - 1.0).abs() < 0.025);
    }
    let exposed = render(&gpu, &mut renderer, &scene, &camera, 0.25);
    assert!(!renderer.environment_preprocessed_last_prepare());
    assert!(
        (radiance(rgb(&exposed, extent, 0, 0)[0]) - 0.5).abs() < 0.015,
        "HDR source values above one must survive"
    );
    scene
        .set_material(
            material,
            PbrMaterial {
                metallic: 1.0,
                roughness: 1.0,
                ..Default::default()
            },
        )
        .unwrap();
    let metal = render(&gpu, &mut renderer, &scene, &camera, 1.0);
    assert!((radiance(rgb(&metal, extent, 64, 64)[0]) / 2.0 - (1.0 - 2.0_f64.ln())).abs() < 0.035);
    scene
        .set_material(
            material,
            PbrMaterial {
                roughness: 1.0,
                ..Default::default()
            },
        )
        .unwrap();
    scene
        .set_material_maps(
            material,
            MaterialMaps {
                occlusion: Some(Arc::new(
                    TextureData::new(ImageExtent::new(1, 1).unwrap(), vec![0, 0, 0, 255]).unwrap(),
                )),
                ..Default::default()
            },
        )
        .unwrap();
    let occluded = render(&gpu, &mut renderer, &scene, &camera, 1.0);
    assert_eq!(
        rgb(&occluded, extent, 64, 64),
        [0; 3],
        "material AO applies only to indirect lighting"
    );
    scene
        .set_lighting(
            Lighting::new(
                vec![Light::Directional {
                    direction: [0.0, 0.0, 1.0],
                    irradiance: [3.0; 3],
                    angular_radius: 0.0,
                    shadow_mode: None,
                }],
                scene.snapshot().lighting().sky().cloned(),
            )
            .unwrap(),
        )
        .unwrap();
    assert!(
        rgb(
            &render(&gpu, &mut renderer, &scene, &camera, 1.0),
            extent,
            64,
            64
        )[0] > 80
    );
    let replacement = Arc::new(
        EnvironmentMap::new(ImageExtent::new(1, 1).unwrap(), vec![[1.0, 4.0, 2.0]]).unwrap(),
    );
    sky(&mut scene, &replacement, 0.0, 0.5, true);
    let changed = render(&gpu, &mut renderer, &scene, &camera, 1.0);
    assert!(renderer.environment_preprocessed_last_prepare());
    assert!(rgb(&changed, extent, 0, 0)[1] > rgb(&changed, extent, 0, 0)[0]);
    assert!(
        renderer
            .prepare_pbr(
                &gpu,
                &scene.snapshot(),
                &camera,
                PbrSettings {
                    environment: EnvironmentSettings {
                        resolution: 64,
                        samples: 64,
                        max_bytes: 1
                    },
                    ..Default::default()
                }
            )
            .is_err(),
        "cache reuse cannot bypass a lowered environment budget"
    );
    sky(&mut scene, &replacement, 0.0, 0.0, true);
    let zero = render(&gpu, &mut renderer, &scene, &camera, 1.0);
    assert!(!renderer.environment_preprocessed_last_prepare());
    assert_eq!(rgb(&zero, extent, 0, 0), [0; 3]);
    assert_eq!(
        baseline,
        render(&gpu, &mut renderer, &empty, &camera, 1.0),
        "scene switches must not leak an old sky"
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn environment_rotation_matches_reflections_and_background() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let extent = ImageExtent::new(128, 128).unwrap();
    let camera = Camera::look_at(
        extent,
        [3.0, 0.0, -3.0],
        [0.0; 3],
        [0.0, 0.0, -1.0],
        0.8,
        0.1,
        20.0,
    )
    .unwrap();
    let reflected = [
        -std::f64::consts::FRAC_1_SQRT_2,
        0.0,
        -std::f64::consts::FRAC_1_SQRT_2,
    ];
    let background_camera = Camera::look_at(
        extent,
        [0.0; 3],
        reflected,
        [0.0, 0.0, -1.0],
        0.8,
        0.1,
        20.0,
    )
    .unwrap();
    let mut pixels = Vec::new();
    for y in 0..32 {
        let theta = (f64::from(y) + 0.5) * std::f64::consts::PI / 32.0;
        for x in 0..64 {
            let phi = ((f64::from(x) + 0.5) / 64.0 - 0.5) * std::f64::consts::TAU;
            let direction = [
                theta.sin() * phi.cos(),
                theta.sin() * phi.sin(),
                -theta.cos(),
            ];
            let cosine: f64 = direction
                .into_iter()
                .zip(reflected)
                .map(|(a, b)| a * b)
                .sum();
            let peak = ((cosine - 0.8) / 0.2).clamp(0.0, 1.0).powi(2) as f32;
            pixels.push([0.02 + 8.0 * peak, 0.02 + 2.0 * peak, 0.02 + 0.5 * peak]);
        }
    }
    let environment =
        Arc::new(EnvironmentMap::new(ImageExtent::new(64, 32).unwrap(), pixels).unwrap());
    let (mut scene, material) = plane(PbrMaterial {
        metallic: 1.0,
        roughness: 0.045,
        ..Default::default()
    });
    let mut background = Scene::new().unwrap();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    sky(&mut scene, &environment, 0.0, 1.0, false);
    let sharp = rgb(
        &render(&gpu, &mut renderer, &scene, &camera, 1.0),
        extent,
        64,
        64,
    );
    sky(&mut background, &environment, 0.0, 1.0, true);
    let sky_sharp = rgb(
        &render(&gpu, &mut renderer, &background, &background_camera, 1.0),
        extent,
        64,
        64,
    );
    assert!(
        !renderer.environment_preprocessed_last_prepare(),
        "one scene-level source is shared across camera/scene changes"
    );
    assert!(sharp[0] > 200 && sky_sharp[0] > 200);
    sky(
        &mut scene,
        &environment,
        std::f64::consts::FRAC_PI_2,
        1.0,
        false,
    );
    let rotated = rgb(
        &render(&gpu, &mut renderer, &scene, &camera, 1.0),
        extent,
        64,
        64,
    );
    sky(
        &mut background,
        &environment,
        std::f64::consts::FRAC_PI_2,
        1.0,
        true,
    );
    let sky_rotated = rgb(
        &render(&gpu, &mut renderer, &background, &background_camera, 1.0),
        extent,
        64,
        64,
    );
    assert!(!renderer.environment_preprocessed_last_prepare());
    assert!(
        rotated[0] + 80 < sharp[0] && sky_rotated[0] + 80 < sky_sharp[0],
        "rotation must agree in reflections and backdrop"
    );
    sky(&mut scene, &environment, 0.0, 1.0, false);
    scene
        .set_material(
            material,
            PbrMaterial {
                metallic: 1.0,
                roughness: 1.0,
                ..Default::default()
            },
        )
        .unwrap();
    let rough = rgb(
        &render(&gpu, &mut renderer, &scene, &camera, 1.0),
        extent,
        64,
        64,
    );
    assert!(
        rough[0] + 30 < sharp[0],
        "roughness must broaden the environment reflection"
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn punctual_shadows_do_not_multiply_unoccluded_ibl() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let extent = ImageExtent::new(128, 128).unwrap();
    let camera = Camera::look_at(
        extent,
        [0.0, 0.0, -3.0],
        [0.0; 3],
        [0.0, -1.0, 0.0],
        1.0,
        0.1,
        20.0,
    )
    .unwrap();
    let (mut scene, material) = plane(PbrMaterial {
        roughness: 1.0,
        ..Default::default()
    });
    let mesh = scene.snapshot().instances().next().unwrap().1.mesh();
    let mut model = ModelMatrix::IDENTITY.rows();
    model[0][0] = 0.25;
    model[1][1] = 0.25;
    model[2][3] = -0.5;
    scene
        .add_instance(mesh, vec![material], ModelMatrix::new(model).unwrap())
        .unwrap();
    scene
        .set_lighting(
            Lighting::new(
                vec![Light::Directional {
                    direction: [1.0, 0.0, 1.0],
                    irradiance: [3.0, 0.0, 0.0],
                    angular_radius: 0.0,
                    shadow_mode: None,
                }],
                Some(SkyLight {
                    environment: Arc::new(
                        EnvironmentMap::new(ImageExtent::new(1, 1).unwrap(), vec![[0.0, 0.0, 2.0]])
                            .unwrap(),
                    ),
                    gain: 1.0,
                    rotation: 0.0,
                    visible: false,
                }),
            )
            .unwrap(),
        )
        .unwrap();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let (p, _) = camera.project([0.5, 0.0, 0.0]).unwrap();
    let mut results = Vec::new();
    for mode in [chr_renderer::ShadowMode::Pcf, chr_renderer::ShadowMode::Off] {
        renderer
            .prepare_pbr(
                &gpu,
                &scene.snapshot(),
                &camera,
                PbrSettings {
                    tone_map: ToneMap::Reinhard,
                    shadows: chr_renderer::ShadowSettings {
                        mode,
                        resolution: 256,
                        ..Default::default()
                    },
                    environment: EnvironmentSettings {
                        resolution: 64,
                        samples: 32,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        let mut pixels = Vec::new();
        OffscreenTarget::new(&gpu, extent)
            .unwrap()
            .render_rgba(
                &gpu,
                |encoder, view| renderer.encode(encoder, view),
                &mut pixels,
            )
            .unwrap();
        results.push(rgb(&pixels, extent, p[0] as u32, p[1] as u32));
    }
    assert!(
        results[0][0] < 5 && results[1][0] > 80,
        "punctual visibility affects the red direct light"
    );
    assert!(results[0][2] > 120);
    assert_eq!(
        results[0][2], results[1][2],
        "unoccluded blue IBL is independent of the punctual shadow map"
    );
}
