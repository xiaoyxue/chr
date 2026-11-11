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
    render_settings(
        gpu,
        renderer,
        scene,
        camera,
        ShadowSettings {
            mode,
            resolution: 256,
            filter_radius: 2.5,
            depth_bias: 0.0001,
            normal_bias: 0.003,
            ..Default::default()
        },
    )
}

fn render_settings(
    gpu: &Gpu,
    renderer: &mut MeshRenderer,
    scene: &Scene,
    camera: &Camera,
    shadows: ShadowSettings,
) -> Vec<u8> {
    let settings = PbrSettings {
        tone_map: ToneMap::Reinhard,
        shadows,
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

fn radiance(value: u8) -> f64 {
    let value = f64::from(value) / 255.0;
    let linear = if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    };
    linear / (1.0 - linear)
}

// Visibility-only disk integration, independent of shadow-map depth and PCSS.
fn sampled_visibility(x: f64, height: f64, radius: f64, spot: bool) -> f64 {
    let slope: f64 = if spot { 0.6 } else { 0.7 };
    let length = (1.0 + slope * slope).sqrt();
    let mut visible = 0;
    let mut samples = 0;
    for row in 0..48 {
        for col in 0..48 {
            let u = (f64::from(col) + 0.5) / 24.0 - 1.0;
            let v = (f64::from(row) + 0.5) / 24.0 - 1.0;
            if u * u + v * v > 1.0 {
                continue;
            }
            let (hit_x, hit_y) = if spot {
                let light_x = -3.0 + radius * u / length;
                let light_y = radius * v;
                let light_z = -5.0 - radius * u * slope / length;
                let t = height / -light_z;
                (x + (light_x - x) * t, light_y * t)
            } else {
                let dx = slope / length + radius.tan() * u / length;
                let dy = radius.tan() * v;
                let dz = 1.0 / length - radius.tan() * u * slope / length;
                (x - height * dx / dz, -height * dy / dz)
            };
            visible += u32::from(hit_x.abs() > 0.5 || hit_y.abs() > 0.5);
            samples += 1;
        }
    }
    f64::from(visible) / f64::from(samples)
}

#[test]
#[ignore = "requires a hardware GPU"]
fn pcss_contact_source_size_and_sampled_visibility() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let extent = ImageExtent::new(384, 384).unwrap();
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
    let mut ground = ModelMatrix::IDENTITY.rows();
    ground[0][0] = 8.0;
    ground[1][1] = 8.0;
    scene
        .add_instance(quad, vec![material], ModelMatrix::new(ground).unwrap())
        .unwrap();
    let caster = scene
        .add_instance(quad, vec![material], ModelMatrix::IDENTITY)
        .unwrap();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let settings = ShadowSettings {
        mode: ShadowMode::Pcss,
        resolution: 512,
        normal_bias: 0.0,
        depth_bias: 0.00005,
        ..Default::default()
    };
    for kind in 0..3 {
        let spot = kind != 0;
        let mut widths = Vec::new();
        for (height, size) in [(0.3, 1.0), (1.5, 1.0), (1.5, 2.0)] {
            let radius = size * if spot { 0.4 } else { 0.12 };
            let light = if kind == 2 {
                Light::Point {
                    position: [-3.0, 0.0, -5.0],
                    intensity: [100.0; 3],
                    source_radius: radius as f32,
                }
            } else if spot {
                Light::Spot {
                    position: [-3.0, 0.0, -5.0],
                    direction: [0.6, 0.0, 1.0],
                    intensity: [100.0; 3],
                    inner_angle: 0.7,
                    outer_angle: 1.0,
                    source_radius: radius as f32,
                }
            } else {
                Light::Directional {
                    direction: [0.7, 0.0, 1.0],
                    irradiance: [3.0; 3],
                    angular_radius: radius as f32,
                }
            };
            scene
                .set_lighting(Lighting::new(vec![light], None).unwrap())
                .unwrap();
            let mut model = ModelMatrix::IDENTITY.rows();
            model[2][3] = -height;
            scene
                .set_model(caster, ModelMatrix::new(model).unwrap())
                .unwrap();
            let off = render_settings(
                &gpu,
                &mut renderer,
                &scene,
                &camera,
                ShadowSettings {
                    mode: ShadowMode::Off,
                    ..settings
                },
            );
            let soft = render_settings(&gpu, &mut renderer, &scene, &camera, settings);
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
            render_settings(&gpu, &mut renderer, &scene, &moved_camera, settings);
            assert_eq!(
                renderer.shadow_passes_last_frame(),
                0,
                "PCSS camera motion reuses maps"
            );
            let repeated = render_settings(&gpu, &mut renderer, &scene, &camera, settings);
            assert_eq!(soft, repeated, "PCSS sampling must be deterministic");
            assert_eq!(renderer.shadow_passes_last_frame(), 0);
            let edge = if spot {
                (3.0 * height + 2.5) / (5.0 - height)
            } else {
                0.7 * height + 0.5
            };
            let mut partial = 0;
            let mut error = 0.0;
            let mut compared = 0;
            for i in 0..100 {
                let x = edge - 0.45 + f64::from(i) * 0.009;
                if x < 0.6 {
                    continue;
                } // Exclude the caster itself from the receiver scan.
                let point = [x, 0.0, 0.0];
                let visibility =
                    radiance(pixel(&camera, &soft, point)) / radiance(pixel(&camera, &off, point));
                partial += usize::from((0.1..0.9).contains(&visibility));
                error += (visibility - sampled_visibility(x, height, radius, spot)).abs();
                compared += 1;
            }
            eprintln!(
                "PCSS kind={kind} height={height} size={size}: partial={partial}, reference MAE={}",
                error / f64::from(compared)
            );
            assert!(
                error / f64::from(compared) < 0.08,
                "PCSS must approximate independently sampled emitter visibility"
            );
            widths.push(partial);
        }
        assert!(
            widths[1] > widths[0] + 4,
            "separation must widen the penumbra: {widths:?}"
        );
        assert!(
            widths[2] > widths[1] + 4,
            "source size must widen the penumbra: {widths:?}"
        );
    }
    scene
        .set_lighting(
            Lighting::new(
                vec![Light::Directional {
                    direction: [0.7, 0.0, 1.0],
                    irradiance: [3.0; 3],
                    angular_radius: 0.0,
                }],
                None,
            )
            .unwrap(),
        )
        .unwrap();
    let hard = render_settings(
        &gpu,
        &mut renderer,
        &scene,
        &camera,
        ShadowSettings {
            mode: ShadowMode::Hard,
            ..settings
        },
    );
    assert_eq!(
        hard,
        render_settings(&gpu, &mut renderer, &scene, &camera, settings),
        "zero source size is exactly Hard"
    );
    assert_eq!(
        renderer.shadow_passes_last_frame(),
        0,
        "Hard/PCSS share depth maps"
    );
    scene
        .set_lighting(
            Lighting::new(
                vec![Light::Directional {
                    direction: [0.7, 0.0, 1.0],
                    irradiance: [3.0; 3],
                    angular_radius: 0.12,
                }],
                None,
            )
            .unwrap(),
        )
        .unwrap();
    scene.set_visible(caster, false).unwrap();
    let clear = render_settings(&gpu, &mut renderer, &scene, &camera, settings);
    assert_eq!(
        clear,
        render_settings(
            &gpu,
            &mut renderer,
            &scene,
            &camera,
            ShadowSettings {
                mode: ShadowMode::Off,
                ..settings
            }
        ),
        "no blockers is fully lit"
    );
    let mut quality = settings.pcss;
    quality.max_samples_per_fragment = 1;
    assert!(
        renderer
            .prepare_pbr(
                &gpu,
                &scene.snapshot(),
                &camera,
                PbrSettings {
                    shadows: ShadowSettings {
                        pcss: quality,
                        ..settings
                    },
                    ..Default::default()
                }
            )
            .is_err(),
        "both PCSS stages count against the sample budget"
    );
    quality = settings.pcss;
    quality.blocker_samples = 0;
    assert!(
        renderer
            .prepare_pbr(
                &gpu,
                &scene.snapshot(),
                &camera,
                PbrSettings {
                    shadows: ShadowSettings {
                        pcss: quality,
                        ..settings
                    },
                    ..Default::default()
                }
            )
            .is_err(),
        "invalid PCSS quality must not fall back to PCF"
    );
    scene
        .set_lighting(
            Lighting::new(
                vec![Light::Spot {
                    position: [0.0, 0.0, -0.011],
                    direction: [0.0, 0.0, 1.0],
                    intensity: [0.001; 3],
                    inner_angle: 0.7,
                    outer_angle: 1.0,
                    source_radius: 0.05,
                }],
                None,
            )
            .unwrap(),
        )
        .unwrap();
    let clear = render_settings(&gpu, &mut renderer, &scene, &camera, settings);
    assert_eq!(
        clear,
        render_settings(
            &gpu,
            &mut renderer,
            &scene,
            &camera,
            ShadowSettings {
                mode: ShadowMode::Off,
                ..settings
            }
        ),
        "receivers just beyond the spot near plane remain finite and fully lit"
    );
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
                    angular_radius: 0.0,
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
                    source_radius: 0.0,
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
                    source_radius: 0.0,
                }],
                None,
            )
            .unwrap(),
        )
        .unwrap();
    render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Hard);
    assert_eq!(renderer.shadow_passes_last_frame(), 6);
}

#[test]
#[ignore = "requires a hardware GPU"]
fn point_radial_shadows_cover_all_face_edges_and_corners() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let mut scene = Scene::new().unwrap();
    let mesh = scene
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
            double_sided: true,
            ..Default::default()
        })
        .unwrap();
    let caster_material = scene
        .add_material(PbrMaterial {
            roughness: 1.0,
            double_sided: true,
            ..Default::default()
        })
        .unwrap();
    let ground = scene
        .add_instance(mesh, vec![material], ModelMatrix::IDENTITY)
        .unwrap();
    let caster = scene
        .add_instance(mesh, vec![caster_material], ModelMatrix::IDENTITY)
        .unwrap();
    scene
        .set_lighting(
            Lighting::new(
                vec![Light::Point {
                    position: [0.0; 3],
                    intensity: [30.0; 3],
                    source_radius: 0.2,
                }],
                None,
            )
            .unwrap(),
        )
        .unwrap();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let extent = ImageExtent::new(160, 160).unwrap();
    let normalize = |v: [f64; 3]| {
        let len = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        v.map(|x| x / len)
    };
    let cross = |a: [f64; 3], b: [f64; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    for x in -1..=1 {
        for y in -1..=1 {
            for z in -1..=1 {
                if [x, y, z] == [0, 0, 0] {
                    continue;
                }
                let d = normalize([f64::from(x), f64::from(y), f64::from(z)]);
                let up = if z == 0 || x != 0 || y != 0 {
                    [0.0, 0.0, 1.0]
                } else {
                    [0.0, 1.0, 0.0]
                };
                let t = normalize(cross(d, up));
                let b = cross(d, t);
                let model = |distance: f64, scale: f64| {
                    ModelMatrix::new([
                        [t[0] * scale, b[0] * scale, d[0], d[0] * distance],
                        [t[1] * scale, b[1] * scale, d[1], d[1] * distance],
                        [t[2] * scale, b[2] * scale, d[2], d[2] * distance],
                        [0.0, 0.0, 0.0, 1.0],
                    ])
                    .unwrap()
                };
                scene.set_model(ground, model(4.0, 6.0)).unwrap();
                scene.set_model(caster, model(2.0, 1.0)).unwrap();
                let center = d.map(|v| v * 4.0);
                let outside = std::array::from_fn(|i| center[i] - t[i] * 1.6);
                let camera = Camera::look_at(
                    extent,
                    std::array::from_fn(|i| d[i] * 3.0 + t[i] * 2.0),
                    center,
                    b,
                    1.4,
                    0.01,
                    50.0,
                )
                .unwrap();
                for mode in [ShadowMode::Hard, ShadowMode::Pcf, ShadowMode::Pcss] {
                    let image = render(&gpu, &mut renderer, &scene, &camera, mode);
                    assert!(
                        pixel(&camera, &image, center) < 8,
                        "{x}/{y}/{z} {mode:?}: missing shadow at face boundary"
                    );
                    assert!(
                        pixel(&camera, &image, outside) > 70,
                        "{x}/{y}/{z} {mode:?}: false shadow outside caster"
                    );
                    assert_eq!(
                        renderer.shadow_passes_last_frame(),
                        if mode == ShadowMode::Hard { 6 } else { 0 }
                    );
                }
                scene.set_visible(caster, false).unwrap();
                let clear = render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Pcss);
                assert!(
                    pixel(&camera, &clear, center) > 70,
                    "receiver must not self-shadow on cube boundaries"
                );
                assert_eq!(
                    clear,
                    render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Off),
                    "empty radial samples stay lit even when the receiver plane extends beyond the fitted far sphere"
                );
                scene.set_visible(caster, true).unwrap();
                let mut shifted = model(2.0, 1.0).rows();
                for i in 0..3 {
                    shifted[i][3] += t[i] * 0.5;
                }
                scene
                    .set_model(caster, ModelMatrix::new(shifted).unwrap())
                    .unwrap();
                for mode in [ShadowMode::Pcf, ShadowMode::Pcss] {
                    let edge = render(&gpu, &mut renderer, &scene, &camera, mode);
                    let visibility = radiance(pixel(&camera, &edge, center))
                        / radiance(pixel(&camera, &clear, center));
                    assert!(
                        (0.15..0.85).contains(&visibility),
                        "{x}/{y}/{z} {mode:?}: edge on cube seam must be partially covered, got {visibility}"
                    );
                }
                scene.set_model(caster, model(2.0, 1.0)).unwrap();
                if [x, y, z] == [0, 0, 1] {
                    scene.set_model(caster, model(0.009, 0.001)).unwrap();
                    let near = render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Hard);
                    assert!(
                        pixel(&camera, &near, center) > 70,
                        "the spherical near exclusion must remove sub-centimetre casters"
                    );
                    scene.set_model(caster, model(2.0, 1.0)).unwrap();
                }
                if [x, y, z] == [1, 1, 1] {
                    scene
                        .set_material(
                            caster_material,
                            PbrMaterial {
                                base_color: [1.0, 1.0, 1.0, 0.0],
                                alpha_cutoff: Some(0.5),
                                double_sided: true,
                                ..Default::default()
                            },
                        )
                        .unwrap();
                    let masked = render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Pcss);
                    assert!(
                        pixel(&camera, &masked, center) > 70,
                        "radial depth must honor alpha masks"
                    );
                    scene
                        .set_material(
                            caster_material,
                            PbrMaterial {
                                double_sided: true,
                                ..Default::default()
                            },
                        )
                        .unwrap();
                    let mut mirrored = model(2.0, 1.0).rows();
                    for row in mirrored.iter_mut().take(3) {
                        row[0] *= -1.0;
                    }
                    scene
                        .set_model(caster, ModelMatrix::new(mirrored).unwrap())
                        .unwrap();
                    let point = Light::Point {
                        position: [0.0; 3],
                        intensity: [30.0; 3],
                        source_radius: 0.0,
                    };
                    scene
                        .set_lighting(Lighting::new(vec![point], None).unwrap())
                        .unwrap();
                    let hard = render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Hard);
                    assert!(pixel(&camera, &hard, center) < 8);
                    assert_eq!(
                        hard,
                        render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Pcss),
                        "zero point radius is exact Hard"
                    );
                    scene
                        .set_lighting(
                            Lighting::new(
                                vec![
                                    Light::Directional {
                                        direction: d,
                                        irradiance: [0.0; 3],
                                        angular_radius: 0.0,
                                    },
                                    Light::Spot {
                                        position: [0.0; 3],
                                        direction: d,
                                        intensity: [0.0; 3],
                                        inner_angle: 0.5,
                                        outer_angle: 1.0,
                                        source_radius: 0.0,
                                    },
                                    point,
                                ],
                                None,
                            )
                            .unwrap(),
                        )
                        .unwrap();
                    assert_eq!(
                        hard,
                        render(&gpu, &mut renderer, &scene, &camera, ShadowMode::Hard),
                        "mixed planar/cube lights must use disjoint atlas layers"
                    );
                    assert_eq!(renderer.shadow_passes_last_frame(), 8);
                }
            }
        }
    }
    let camera = Camera::look_at(
        extent,
        [0.0, 0.0, -5.0],
        [0.0; 3],
        [0.0, -1.0, 0.0],
        1.0,
        0.1,
        20.0,
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
                        resolution: 256,
                        max_bytes: 256 * 256 * 4 * 5,
                        ..Default::default()
                    },
                    ..Default::default()
                }
            )
            .is_err(),
        "the memory budget must include all six point faces"
    );
}
