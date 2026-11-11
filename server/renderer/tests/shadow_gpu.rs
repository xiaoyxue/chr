use chr_renderer::{
    Gpu, MeshRenderer, OffscreenTarget, PbrSettings, ShadowMode, ShadowSettings, ToneMap,
    camera::Camera,
    coordinates::ImageExtent,
    scene::{Light, Lighting, MeshData, ModelMatrix, PbrMaterial, Scene},
};

fn render(
    gpu: &Gpu,
    renderer: &mut MeshRenderer,
    scene: &Scene,
    camera: &Camera,
    mode: ShadowMode,
) -> Vec<u8> {
    let settings = PbrSettings {
        tone_map: ToneMap::Reinhard,
        shadows: ShadowSettings {
            mode,
            resolution: 256,
            filter_radius: 2.5,
            depth_bias: 0.0001,
            normal_bias: 0.003,
            ..Default::default()
        },
        ..Default::default()
    };
    renderer
        .prepare_pbr(gpu, &scene.snapshot(), camera, settings)
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

fn pixel(camera: &Camera, image: &[u8], world: [f64; 3]) -> u8 {
    let (p, _) = camera.project(world).unwrap();
    image[(p[1] as usize * camera.extent().width() as usize + p[0] as usize) * 4]
}

#[test]
#[ignore = "requires a hardware GPU"]
fn directional_spot_pcf_and_caster_changes() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let extent = ImageExtent::new(192, 192).unwrap();
    let camera = Camera::look_at(
        extent,
        [0.0, 0.0, -5.0],
        [0.0; 3],
        [0.0, -1.0, 0.0],
        1.0,
        0.1,
        100.0,
    )
    .unwrap();
    let mut scene = Scene::new().unwrap();
    let quad = scene
        .add_mesh(
            MeshData::new(
                vec![
                    [-0.5, -0.5, 0.0],
                    [0.5, -0.5, 0.0],
                    [0.5, 0.5, 0.0],
                    [-0.5, 0.5, 0.0],
                ],
                vec![0, 2, 1, 0, 3, 2],
                std::iter::once(0..6).collect(),
            )
            .unwrap(),
        )
        .unwrap();
    let material = scene
        .add_material(PbrMaterial {
            roughness: 1.0,
            ..Default::default()
        })
        .unwrap();
    let caster_material = scene
        .add_material(PbrMaterial {
            roughness: 1.0,
            ..Default::default()
        })
        .unwrap();
    scene
        .add_instance(
            quad,
            vec![material],
            ModelMatrix::new([
                [4.0, 0.0, 0.0, 0.0],
                [0.0, 4.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ])
            .unwrap(),
        )
        .unwrap();
    let mut model = ModelMatrix::IDENTITY.rows();
    model[2][3] = -1.0;
    let caster = scene
        .add_instance(
            quad,
            vec![caster_material],
            ModelMatrix::new(model).unwrap(),
        )
        .unwrap();
    scene
        .set_lighting(
            Lighting::new(
                vec![Light::Directional {
                    direction: [0.7, 0.0, 1.0],
                    irradiance: [3.0; 3],
                }],
                None,
            )
            .unwrap(),
        )
        .unwrap();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let off = render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Off);
    let hard = render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Hard);
    assert_eq!(renderer.shadow_passes_last_frame(), 1);
    let soft = render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Pcf);
    assert_eq!(
        renderer.shadow_passes_last_frame(),
        0,
        "filter mode changes reuse depth maps"
    );
    let moved_camera = Camera::look_at(
        extent,
        [0.2, 0.0, -5.0],
        [0.0; 3],
        [0.0, -1.0, 0.0],
        1.0,
        0.1,
        100.0,
    )
    .unwrap();
    render(&gpu, &mut renderer, &scene, &moved_camera, ShadowMode::Hard);
    assert_eq!(
        renderer.shadow_passes_last_frame(),
        0,
        "camera motion must not redraw scene-fitted shadow maps"
    );
    let receiver = [0.9, 0.0, 0.0];
    assert!(pixel(&camera, &off, receiver) > 80);
    assert!(pixel(&camera, &hard, receiver) < 5);
    assert!(pixel(&camera, &soft, receiver) < 5);
    assert!(
        pixel(&camera, &hard, [1.6, 0.0, 0.0]).abs_diff(pixel(&camera, &off, [1.6, 0.0, 0.0])) < 3
    );
    assert!(
        (0..70).any(|i| {
            let point = [0.7 + f64::from(i) * 0.015, 0.0, 0.0];
            let s = pixel(&camera, &soft, point);
            let full = pixel(&camera, &off, point);
            s > 5 && s + 5 < full
        }),
        "PCF must contain partially covered edge pixels"
    );
    model[0][0] = -1.0;
    scene
        .set_model(caster, ModelMatrix::new(model).unwrap())
        .unwrap();
    assert!(
        pixel(
            &camera,
            &render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Hard),
            receiver
        ) < 5
    );
    scene
        .set_material(
            caster_material,
            PbrMaterial {
                base_color: [1.0, 1.0, 1.0, 0.0],
                alpha_cutoff: Some(0.5),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        pixel(
            &camera,
            &render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Hard),
            receiver
        ) > 80
    );
    scene
        .set_material(caster_material, PbrMaterial::default())
        .unwrap();
    scene
        .set_lighting(
            Lighting::new(
                vec![Light::Spot {
                    position: [-3.0, 0.0, -5.0],
                    direction: [0.7, 0.0, 1.0],
                    intensity: [100.0; 3],
                    inner_angle: 0.4,
                    outer_angle: 0.8,
                }],
                None,
            )
            .unwrap(),
        )
        .unwrap();
    assert!(
        pixel(
            &camera,
            &render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Hard),
            receiver
        ) < 5
    );
    model[0][3] = -1.5;
    scene
        .set_model(caster, ModelMatrix::new(model).unwrap())
        .unwrap();
    assert!(
        pixel(
            &camera,
            &render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Hard),
            receiver
        ) > 80
    );
    assert!(
        renderer
            .prepare_pbr(
                &gpu,
                &scene.snapshot(),
                &camera,
                PbrSettings {
                    shadows: ShadowSettings {
                        mode: ShadowMode::Hard,
                        resolution: 256,
                        filter_radius: 2.5,
                        max_bytes: 4,
                        ..Default::default()
                    },
                    ..Default::default()
                }
            )
            .is_err(),
        "a lower memory budget must not be bypassed by the shadow cache"
    );
    scene
        .set_lighting(
            Lighting::new(
                vec![Light::Point {
                    position: [0.0, 0.0, -5.0],
                    intensity: [1.0; 3],
                }],
                None,
            )
            .unwrap(),
        )
        .unwrap();
    assert!(
        renderer
            .prepare_pbr(
                &gpu,
                &scene.snapshot(),
                &camera,
                PbrSettings {
                    shadows: ShadowSettings {
                        mode: ShadowMode::Hard,
                        ..Default::default()
                    },
                    ..Default::default()
                }
            )
            .is_err()
    );
}
