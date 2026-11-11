use std::sync::Arc;

use chr_renderer::{
    DiagnosticOverlays, DiagnosticView, EnvironmentSettings, Gpu, MeshRenderer, OffscreenTarget,
    PbrSettings, RenderSettings, ShadowMode, ShadowSettings,
    camera::Camera,
    coordinates::ImageExtent,
    scene::{
        EnvironmentMap, Light, Lighting, MeshAttributes, MeshData, ModelMatrix, PbrMaterial, Scene,
        SceneView, SkyLight,
    },
};

fn gpu() -> Gpu {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    let info = gpu.adapter.get_info();
    eprintln!(
        "diagnostics GPU: {} ({:?}, {:?})",
        info.name, info.backend, info.device_type
    );
    assert_ne!(info.device_type, wgpu::DeviceType::Cpu);
    gpu
}

fn test_camera(width: u32, height: u32) -> Camera {
    Camera::look_at(
        ImageExtent::new(width, height).unwrap(),
        [0.0, 0.0, -5.0],
        [0.0; 3],
        [0.0, -1.0, 0.0],
        1.0,
        1.0,
        20.0,
    )
    .unwrap()
}

fn capture(
    gpu: &Gpu,
    renderer: &mut MeshRenderer,
    scene: &SceneView,
    camera: &Camera,
    pbr: Option<PbrSettings>,
) -> Vec<u8> {
    match pbr {
        Some(settings) => renderer.prepare_pbr(gpu, scene, camera, settings),
        None => renderer.prepare(gpu, scene, camera),
    }
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

fn encoded(value: f32) -> u8 {
    (255.0
        * if value <= 0.0031308 {
            value * 12.92
        } else {
            1.055 * value.powf(1.0 / 2.4) - 0.055
        })
    .round() as u8
}

fn triangle(normal: Option<[f32; 3]>, model: ModelMatrix) -> Scene {
    let mut scene = Scene::new().unwrap();
    let mut mesh = MeshData::new(
        vec![[-1.7, -1.4, 0.0], [1.8, -1.2, 0.0], [-0.3, 1.8, 0.0]],
        vec![0, 2, 1],
        std::iter::once(0..3).collect(),
    )
    .unwrap();
    if let Some(n) = normal {
        mesh = mesh
            .with_attributes(MeshAttributes {
                normals: vec![n; 3],
                ..Default::default()
            })
            .unwrap();
    }
    let mesh = scene.add_mesh(mesh).unwrap();
    let material = scene
        .add_material(PbrMaterial {
            double_sided: true,
            ..Default::default()
        })
        .unwrap();
    scene.add_instance(mesh, vec![material], model).unwrap();
    scene
}

#[test]
#[ignore = "requires a hardware GPU"]
fn configured_msaa_resolves_edges_recreates_attachments_and_rejects_unsupported_counts() {
    let gpu = gpu();
    let scene = triangle(None, ModelMatrix::IDENTITY).snapshot();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    for (width, height) in [(96, 64), (65, 97), (96, 64)] {
        let camera = test_camera(width, height);
        renderer
            .set_render_settings(&gpu, RenderSettings::default())
            .unwrap();
        let single = capture(&gpu, &mut renderer, &scene, &camera, None);
        renderer
            .set_render_settings(
                &gpu,
                RenderSettings {
                    sample_count: 4,
                    ..Default::default()
                },
            )
            .unwrap();
        let multi = capture(&gpu, &mut renderer, &scene, &camera, None);
        let background = single[0];
        let intermediates = |image: &[u8]| {
            image
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[0] > background + 3 && p[0] < 252)
                .count()
        };
        assert_eq!(intermediates(&single), 0);
        assert!(
            intermediates(&multi) > 20,
            "MSAA must create partially covered edge pixels"
        );
        for unsupported in [0, 2, 3, 8, 16, u32::MAX] {
            assert!(
                renderer
                    .set_render_settings(
                        &gpu,
                        RenderSettings {
                            sample_count: unsupported,
                            ..Default::default()
                        }
                    )
                    .is_err()
            );
            assert_eq!(renderer.render_settings().sample_count, 4);
        }
        renderer
            .set_render_settings(&gpu, RenderSettings::default())
            .unwrap();
        assert_eq!(capture(&gpu, &mut renderer, &scene, &camera, None), single);
    }
}

#[test]
#[ignore = "requires a hardware GPU"]
fn normals_depth_and_indexed_wireframe_are_nonphysical_shared_views() {
    let gpu = gpu();
    let camera = test_camera(96, 96);
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let center = (48 * 96 + 48) * 4;
    for x_scale in [2.0, -2.0] {
        let mut rows = ModelMatrix::IDENTITY.rows();
        rows[0][0] = x_scale;
        rows[1][1] = 0.75;
        rows[2][2] = 0.5;
        let model = ModelMatrix::new(rows).unwrap();
        let scene = triangle(Some([1.0, 1.0, -1.0]), model).snapshot();
        let normal = model.transform_normal([1.0, 1.0, -1.0]).unwrap();
        renderer
            .set_render_settings(
                &gpu,
                RenderSettings {
                    view: DiagnosticView::Normals,
                    sample_count: 4,
                    ..Default::default()
                },
            )
            .unwrap();
        let normals = capture(
            &gpu,
            &mut renderer,
            &scene,
            &camera,
            Some(PbrSettings {
                exposure: 0.0,
                ..Default::default()
            }),
        );
        for channel in 0..3 {
            let expected = encoded((normal[channel] as f32 + 1.0) * 0.5);
            assert!(
                normals[center + channel].abs_diff(expected) <= 1,
                "{:?} vs {normal:?}",
                &normals[center..center + 3]
            );
        }
        assert_eq!(normals, capture(&gpu, &mut renderer, &scene, &camera, None));
        renderer
            .set_render_settings(
                &gpu,
                RenderSettings {
                    view: DiagnosticView::Depth,
                    sample_count: 4,
                    ..Default::default()
                },
            )
            .unwrap();
        let depth = capture(&gpu, &mut renderer, &scene, &camera, None);
        let clip = camera.clip_from_world().unwrap();
        let expected = encoded(clip[3][2] / clip[3][3]);
        assert!(depth[center].abs_diff(expected) <= 1);
        assert_eq!(depth[center], depth[center + 1]);
        renderer
            .set_render_settings(
                &gpu,
                RenderSettings {
                    view: DiagnosticView::Wireframe,
                    sample_count: 4,
                    ..Default::default()
                },
            )
            .unwrap();
        let wire = capture(&gpu, &mut renderer, &scene, &camera, None);
        assert!(
            wire.as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[0] > 200)
                .count()
                > 40
        );
        assert!(
            wire[center] < 100,
            "wireframe must not fill triangle interiors"
        );
    }
    // Missing vertex normals use winding-aware geometric derivatives.
    for mirrored in [1.0, -1.0] {
        let mut rows = ModelMatrix::IDENTITY.rows();
        rows[0][0] = mirrored;
        let model = ModelMatrix::new(rows).unwrap();
        renderer
            .set_render_settings(
                &gpu,
                RenderSettings {
                    view: DiagnosticView::Normals,
                    ..Default::default()
                },
            )
            .unwrap();
        let flat = capture(
            &gpu,
            &mut renderer,
            &triangle(None, model).snapshot(),
            &camera,
            None,
        );
        let authored = capture(
            &gpu,
            &mut renderer,
            &triangle(Some([0.0, 0.0, -1.0]), model).snapshot(),
            &camera,
            None,
        );
        assert_eq!(flat, authored);
    }
}

#[test]
#[ignore = "requires a hardware GPU"]
fn overlays_and_view_switches_preserve_lighting_resources_environment_and_shadow_cache() {
    let gpu = gpu();
    let mut rows = ModelMatrix::IDENTITY.rows();
    rows[0][0] = -1.25;
    rows[1][1] = 0.75;
    rows[0][3] = 0.2;
    let mut scene = triangle(Some([0.0, 0.0, -1.0]), ModelMatrix::new(rows).unwrap());
    let environment = Arc::new(
        EnvironmentMap::new(ImageExtent::new(1, 1).unwrap(), vec![[0.3, 0.2, 0.1]]).unwrap(),
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
                Some(SkyLight {
                    environment: Arc::clone(&environment),
                    rotation: 0.0,
                    gain: 1.0,
                    visible: true,
                }),
            )
            .unwrap(),
        )
        .unwrap();
    let snapshot = scene.snapshot();
    let settings = PbrSettings {
        shadows: ShadowSettings {
            mode: ShadowMode::Hard,
            resolution: 64,
            ..Default::default()
        },
        environment: EnvironmentSettings {
            resolution: 64,
            samples: 64,
            ..Default::default()
        },
        ..Default::default()
    };
    let camera = Camera::look_at(
        ImageExtent::new(96, 96).unwrap(),
        [2.0, 2.0, -5.0],
        [0.0; 3],
        [0.0, 0.0, -1.0],
        1.0,
        0.1,
        20.0,
    )
    .unwrap();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let baseline = capture(&gpu, &mut renderer, &snapshot, &camera, Some(settings));
    assert!(renderer.shadow_passes_last_frame() > 0);
    assert!(renderer.environment_preprocessed_last_prepare());
    for count in [4, 1, 4, 1] {
        for view in [
            DiagnosticView::Normals,
            DiagnosticView::Depth,
            DiagnosticView::Wireframe,
            DiagnosticView::Shaded,
        ] {
            renderer
                .set_render_settings(
                    &gpu,
                    RenderSettings {
                        sample_count: count,
                        view,
                        overlays: DiagnosticOverlays {
                            axes: true,
                            bounds: true,
                        },
                    },
                )
                .unwrap();
            let image = capture(&gpu, &mut renderer, &snapshot, &camera, Some(settings));
            for color in [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]] {
                assert!(
                    image.as_chunks::<4>().0.iter().any(|p| p[..3] == color),
                    "missing overlay color {color:?}"
                );
            }
            let instance = snapshot.instances().next().unwrap().1;
            let bounds = snapshot
                .mesh(instance.mesh())
                .unwrap()
                .bounds()
                .transformed(instance.model())
                .unwrap();
            let min = bounds.min();
            let max = bounds.max();
            let (pixel, _) = camera
                .project([(min[0] + max[0]) * 0.5, min[1], min[2]])
                .unwrap();
            let x = pixel[0].floor() as usize;
            let y = pixel[1].floor() as usize;
            assert!(
                (y - 1..=y + 1).any(|y| (x - 1..=x + 1).any(|x| {
                    let offset = (y * 96 + x) * 4;
                    image[offset..offset + 3] == [255, 255, 0]
                })),
                "bounds must follow the transformed mesh"
            );
            assert_eq!(renderer.shadow_passes_last_frame(), 0);
            assert!(!renderer.environment_preprocessed_last_prepare());
        }
        capture(
            &gpu,
            &mut renderer,
            &snapshot,
            &test_camera(65, 97),
            Some(settings),
        );
        assert_eq!(renderer.shadow_passes_last_frame(), 0);
        assert!(!renderer.environment_preprocessed_last_prepare());
    }
    renderer
        .set_render_settings(&gpu, RenderSettings::default())
        .unwrap();
    let resized = test_camera(65, 97);
    capture(&gpu, &mut renderer, &snapshot, &resized, Some(settings));
    assert_eq!(renderer.shadow_passes_last_frame(), 0);
    assert!(!renderer.environment_preprocessed_last_prepare());
    assert_eq!(
        baseline,
        capture(&gpu, &mut renderer, &snapshot, &camera, Some(settings))
    );
    assert_eq!(renderer.shadow_passes_last_frame(), 0);
    assert_eq!(
        scene.snapshot().instances().count(),
        snapshot.instances().count()
    );
    assert_eq!(
        snapshot.lighting().lights(),
        scene.snapshot().lighting().lights()
    );
    assert!(Arc::ptr_eq(
        &snapshot.lighting().sky().unwrap().environment,
        &environment
    ));
}
