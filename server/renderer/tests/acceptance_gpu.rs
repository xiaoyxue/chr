//! #34 cross-feature acceptance: one synthetic scene combining transformed instances,
//! material variants, all five light families with mixed shadow modes, and the
//! pinhole, orthographic and distorted camera models. Synthetic data only.
//! World metres, ground Z=0, upwards -Z.

use chr_renderer::{
    Gpu, MeshRenderer, OffscreenTarget, PbrSettings, PcssSettings, ShadowMode, ShadowSettings,
    camera::{Camera, CameraView, OrthographicCamera},
    composition::{
        AlphaMode, CompositionOutput, FrameCompositor, ImageLayer, ImageStage, ImageView, Transfer,
    },
    coordinates::ImageExtent,
    distortion::{CameraModel, FrameWarp, LensDistortion, RadialDistortion},
    scene::{
        AreaShape, EnvironmentMap, Light, Lighting, MaterialMaps, MeshData, ModelMatrix,
        PbrMaterial, Scene, SceneError, SkyLight, TextureData,
    },
};
use std::{sync::Arc, time::Duration};

fn gpu() -> Gpu {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    gpu
}

fn cube() -> MeshData {
    let p: Vec<[f32; 3]> = (0..8)
        .map(|i| {
            [
                if i & 1 == 0 { -0.5 } else { 0.5 },
                if i & 2 == 0 { -0.5 } else { 0.5 },
                if i & 4 == 0 { 0.0 } else { -1.0 },
            ]
        })
        .collect();
    let faces = [
        [0, 2, 3, 1],
        [4, 5, 7, 6],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
        [0, 4, 6, 2],
        [1, 3, 7, 5],
    ];
    let indices: Vec<u32> = faces
        .iter()
        .flat_map(|f| [f[0], f[1], f[2], f[0], f[2], f[3]])
        .collect();
    MeshData::new(p, indices, std::iter::once(0..36).collect()).unwrap()
}

fn quad(half: [f32; 2], height: f32) -> MeshData {
    MeshData::new(
        vec![
            [-half[0], -half[1], -height],
            [half[0], -half[1], -height],
            [half[0], half[1], -height],
            [-half[0], half[1], -height],
        ],
        vec![0, 2, 1, 0, 3, 2],
        std::iter::once(0..6).collect(),
    )
    .unwrap()
    .with_attributes(chr_renderer::scene::MeshAttributes {
        tex_coords: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        ..Default::default()
    })
    .unwrap()
}

fn model(rows: [[f64; 4]; 4]) -> ModelMatrix {
    ModelMatrix::new(rows).unwrap()
}

fn lights(variant: u32) -> Lighting {
    let sky = Arc::new(
        EnvironmentMap::new(
            ImageExtent::new(8, 4).unwrap(),
            (0..32)
                .map(|i| {
                    let up = 1.0 - (i / 8) as f32 / 3.0;
                    [0.3 * up + 0.05, 0.45 * up + 0.05, 0.8 * up + 0.05]
                })
                .collect(),
        )
        .unwrap(),
    );
    let gain = if variant == 0 { 0.6 } else { 1.4 };
    Lighting::new(
        vec![
            Light::Directional {
                direction: [0.4, -0.3, 1.0],
                irradiance: [2.2, 2.1, 1.9],
                angular_radius: 0.01,
                shadow_mode: Some(ShadowMode::Pcss),
            },
            Light::Point {
                position: [-6.0, 4.0, -5.0],
                intensity: [60.0, 50.0, 40.0],
                source_radius: 0.3,
                shadow_mode: Some(ShadowMode::Pcf),
            },
            Light::Spot {
                position: [6.0, -5.0, -7.0],
                direction: [-6.0, 5.0, 7.0],
                intensity: [150.0, 160.0, 180.0],
                inner_angle: 0.3,
                outer_angle: 0.6,
                source_radius: 0.0,
                shadow_mode: Some(ShadowMode::Hard),
            },
            Light::Area {
                position: [0.0, 6.0, -4.0],
                normal: [0.0, -0.6, 1.0],
                tangent: [1.0, 0.0, 0.0],
                shape: AreaShape::Rectangle {
                    half_width: 1.5,
                    half_height: 0.8,
                },
                radiance: [3.0; 3],
                two_sided: false,
                shadow_mode: Some(ShadowMode::Pcss),
            },
            Light::Area {
                position: [-4.0, -4.0, -3.0],
                normal: [0.0, 0.0, 1.0],
                tangent: [1.0, 0.0, 0.0],
                shape: AreaShape::Sphere { radius: 0.5 },
                radiance: [4.0, 3.0, 2.0],
                two_sided: false,
                shadow_mode: None,
            },
        ],
        Some(SkyLight {
            environment: sky,
            gain,
            rotation: 0.3,
            visible: true,
            max_radiance: None,
        }),
    )
    .unwrap()
}

/// Transformed instances, material variants (metal, dielectric, emissive, unlit,
/// alpha-masked double-sided) and all five light families with mixed shadow modes.
fn matrix_scene(variant: u32) -> Scene {
    let mut scene = Scene::new().unwrap();
    let ground = scene.add_mesh(quad([15.0, 10.0], 0.0)).unwrap();
    let box_mesh = scene.add_mesh(cube()).unwrap();
    let plate = scene.add_mesh(quad([1.0, 1.0], 0.0)).unwrap();
    let material = |scene: &mut Scene, m: PbrMaterial| scene.add_material(m).unwrap();
    let grass = material(
        &mut scene,
        PbrMaterial {
            base_color: [0.2, 0.45, 0.15, 1.0],
            roughness: 0.85,
            ..Default::default()
        },
    );
    let gold = material(
        &mut scene,
        PbrMaterial {
            base_color: [0.9, 0.7, 0.3, 1.0],
            metallic: 1.0,
            roughness: 0.3,
            ..Default::default()
        },
    );
    let paint = material(
        &mut scene,
        PbrMaterial {
            base_color: [0.7, 0.1, 0.1, 1.0],
            roughness: 0.5,
            emissive: [0.4, 0.05, 0.0],
            ..Default::default()
        },
    );
    let unlit = material(
        &mut scene,
        PbrMaterial {
            base_color: [0.1, 0.6, 0.9, 1.0],
            unlit: true,
            ..Default::default()
        },
    );
    // A 2x2 alpha checker: masked texels must disappear from colour and shadows.
    let checker = Arc::new(
        TextureData::new(
            ImageExtent::new(2, 2).unwrap(),
            vec![
                255, 255, 255, 255, 255, 255, 255, 0, 255, 255, 255, 0, 255, 255, 255, 255,
            ],
        )
        .unwrap(),
    );
    let masked = scene
        .add_material_with_maps(
            PbrMaterial {
                alpha_cutoff: Some(0.5),
                double_sided: true,
                ..Default::default()
            },
            MaterialMaps {
                base_color: Some(checker),
                ..Default::default()
            },
        )
        .unwrap();
    scene
        .add_instance(ground, vec![grass], ModelMatrix::IDENTITY)
        .unwrap();
    // Nonuniform scale with rotation, a mirrored instance and a plain one.
    let (c, s) = (0.5_f64.cos(), 0.5_f64.sin());
    for (rows, m) in [
        (
            [
                [2.0 * c, -s, 0.0, -3.0],
                [2.0 * s, c, 0.0, 0.0],
                [0.0, 0.0, 3.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            gold,
        ),
        (
            [
                [-1.5, 0.0, 0.0, 2.5],
                [0.0, 1.0, 0.0, 1.5],
                [0.0, 0.0, 1.5, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            paint,
        ),
        (
            [
                [0.6, 0.0, 0.0, 0.5],
                [0.0, 0.6, 0.0, -2.5],
                [0.0, 0.0, 2.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            unlit,
        ),
    ] {
        scene.add_instance(box_mesh, vec![m], model(rows)).unwrap();
    }
    scene
        .add_instance(
            plate,
            vec![masked],
            model([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 3.0],
                [0.0, 0.0, 1.0, -2.0],
                [0.0, 0.0, 0.0, 1.0],
            ]),
        )
        .unwrap();
    scene.set_lighting(lights(variant)).unwrap();
    scene
}

fn shadows() -> ShadowSettings {
    ShadowSettings {
        mode: ShadowMode::Pcf,
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

fn pinhole(extent: ImageExtent) -> Camera {
    Camera::look_at(
        extent,
        [0.0, 16.0, -10.0],
        [0.0, 0.0, -1.0],
        [0.0, 0.0, -1.0],
        0.8,
        0.1,
        200.0,
    )
    .unwrap()
}

fn orthographic(extent: ImageExtent) -> OrthographicCamera {
    // Looking straight down: right = +X, down = +Y, forward = +Z.
    OrthographicCamera::from_pose(
        extent,
        [32.0, 18.0],
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, -30.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
        0.1,
        100.0,
    )
    .unwrap()
}

fn lens(extent: ImageExtent) -> CameraModel {
    let (w, h) = (f64::from(extent.width()), f64::from(extent.height()));
    let s = (w * h).sqrt();
    let radial =
        RadialDistortion::new([w / 2.0, h / 2.0], w / 2.0, [-0.06, 0.004, 0.0], None).unwrap();
    let warp = FrameWarp::new([
        [2.0 / s, 0.0, -w / s + 0.002],
        [0.0, 2.0 / s, -h / s - 0.001],
        [0.0, 0.0, 1.0],
    ])
    .unwrap();
    CameraModel::Distorted(Box::new(LensDistortion::new(extent, radial, Some(warp))))
}

fn plate(gpu: &Gpu, extent: ImageExtent) -> ImageView {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: extent.width(),
            height: extent.height(),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let mut pixels = Vec::new();
    for y in 0..extent.height() {
        for x in 0..extent.width() {
            pixels.extend_from_slice(&[
                (x * 255 / extent.width()) as u8,
                (y * 255 / extent.height()) as u8,
                70,
                255,
            ]);
        }
    }
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(extent.width() * 4),
            rows_per_image: Some(extent.height()),
        },
        texture.size(),
    );
    ImageView::new(&texture, Transfer::Srgb, AlphaMode::Straight).unwrap()
}

fn render(
    gpu: &Gpu,
    renderer: &mut MeshRenderer,
    scene: &Scene,
    camera: &dyn CameraView,
) -> Vec<u8> {
    renderer
        .prepare_pbr(
            gpu,
            &scene.snapshot(),
            camera,
            PbrSettings {
                shadows: shadows(),
                ..Default::default()
            },
        )
        .unwrap();
    let target = OffscreenTarget::new(gpu, camera.extent()).unwrap();
    let mut pixels = Vec::new();
    target
        .render_rgba(
            gpu,
            |encoder, view| renderer.encode(encoder, view),
            &mut pixels,
        )
        .unwrap();
    pixels
}

fn composite(gpu: &Gpu, scene: &Scene, camera: &Camera, model: &CameraModel) -> Vec<u8> {
    let extent = camera.extent();
    let background = plate(gpu, extent);
    let mut config = CompositionOutput::new(extent);
    config.pbr.shadows = shadows();
    let mut compositor = FrameCompositor::new(gpu, OffscreenTarget::FORMAT, config).unwrap();
    compositor.set_sample_count(gpu, 4).unwrap();
    compositor
        .prepare_model(
            gpu,
            &scene.snapshot(),
            camera,
            model,
            &[ImageLayer::new(&background, ImageStage::DisplayBackground)],
        )
        .unwrap();
    let target = OffscreenTarget::new(gpu, extent).unwrap();
    let mut pixels = Vec::new();
    target
        .render_rgba(
            gpu,
            |encoder, view| compositor.encode(encoder, view.texture()).unwrap(),
            &mut pixels,
        )
        .unwrap();
    pixels
}

fn distinct_colours(pixels: &[u8]) -> usize {
    let mut seen: Vec<[u8; 3]> = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .step_by(7)
        .map(|p| [p[0] >> 3, p[1] >> 3, p[2] >> 3])
        .collect();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

#[test]
#[ignore = "requires a hardware GPU"]
fn cross_feature_matrix_is_deterministic_isolated_and_bounded() {
    let gpu = gpu();
    let extent = ImageExtent::new(640, 360).unwrap();
    let scene = matrix_scene(0);
    let other = matrix_scene(1);
    let perspective = pinhole(extent);
    let parallel = orthographic(extent);
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    for camera in [&perspective as &dyn CameraView, &parallel] {
        let first = render(&gpu, &mut renderer, &scene, camera);
        assert!(
            distinct_colours(&first) > 40,
            "the matrix must produce a varied image"
        );
        // Determinism, then state isolation: another scene in between changes nothing.
        assert_eq!(render(&gpu, &mut renderer, &scene, camera), first);
        let changed = render(&gpu, &mut renderer, &other, camera);
        assert_ne!(changed, first, "sky gain must change the image");
        assert_eq!(render(&gpu, &mut renderer, &scene, camera), first);
        let mut fresh = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
        assert_eq!(render(&gpu, &mut fresh, &scene, camera), first);
    }
    // Shadow passes: directional 1 + point 6 + spot 1 + shadowed area 6 + sphere area 6.
    render(&gpu, &mut renderer, &matrix_scene(0), &perspective);
    assert_eq!(renderer.shadow_passes_last_frame(), 20);
    // Composition with both camera models over the same footage plate.
    let pinhole_composite = composite(&gpu, &scene, &perspective, &CameraModel::Pinhole);
    let distorted_composite = composite(&gpu, &scene, &perspective, &lens(extent));
    assert_eq!(
        composite(&gpu, &scene, &perspective, &lens(extent)),
        distorted_composite
    );
    assert_ne!(pinhole_composite, distorted_composite);
    // Budgets and invalid typed input fail explicitly, leaving no partial frame.
    let tiny = ShadowSettings {
        max_bytes: 1024,
        ..shadows()
    };
    assert!(
        renderer
            .prepare_pbr(
                &gpu,
                &scene.snapshot(),
                &perspective,
                PbrSettings {
                    shadows: tiny,
                    ..Default::default()
                },
            )
            .is_err()
    );
    let bad_pcf = ShadowSettings {
        pcf_samples: 7,
        ..shadows()
    };
    assert!(
        renderer
            .prepare_pbr(
                &gpu,
                &scene.snapshot(),
                &perspective,
                PbrSettings {
                    shadows: bad_pcf,
                    ..Default::default()
                },
            )
            .is_err()
    );
    assert!(matches!(
        Lighting::new(
            vec![Light::Area {
                position: [0.0; 3],
                normal: [0.0, 0.0, 1.0],
                tangent: [1.0, 0.0, 0.0],
                shape: AreaShape::Disk { radius: 0.0 },
                radiance: [1.0; 3],
                two_sided: false,
                shadow_mode: None,
            }],
            None,
        ),
        Err(SceneError::InvalidLighting)
    ));
    // The renderer recovers after rejected input.
    assert_eq!(
        render(&gpu, &mut renderer, &scene, &perspective).len(),
        640 * 360 * 4
    );
}

struct Timer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
}

fn timed_gpu() -> (Gpu, Timer) {
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
    let timer = Timer {
        queries: device.create_query_set(&wgpu::QuerySetDescriptor {
            label: None,
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        }),
        resolve: device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }),
        readback: device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        }),
    };
    (
        Gpu {
            adapter,
            device,
            queue,
        },
        timer,
    )
}

impl Timer {
    fn measure(
        &self,
        gpu: &Gpu,
        target: &OffscreenTarget,
        mut record: impl FnMut(&mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> f64 {
        let mut pixels = Vec::new();
        target
            .render_rgba(
                gpu,
                |encoder, view| {
                    encoder.write_timestamp(&self.queries, 0);
                    record(encoder, view);
                    encoder.write_timestamp(&self.queries, 1);
                    encoder.resolve_query_set(&self.queries, 0..2, &self.resolve, 0);
                    encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readback, 0, 16);
                },
                &mut pixels,
            )
            .unwrap();
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.unwrap());
        gpu.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(30)),
            })
            .unwrap();
        let ticks = {
            let bytes = self.readback.slice(..).get_mapped_range().unwrap();
            u64::from_le_bytes(bytes[8..16].try_into().unwrap())
                - u64::from_le_bytes(bytes[0..8].try_into().unwrap())
        };
        self.readback.unmap();
        ticks as f64 * f64::from(gpu.queue.get_timestamp_period()) / 1e6
    }
}

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

/// Whole-frame GPU time for the matrix at 1080p: cold frames rebuild shadow maps
/// (new snapshot each frame), warm frames reuse them. Diagnostic, not a frame budget.
#[test]
#[ignore = "requires a hardware GPU with timestamp queries"]
fn cross_feature_matrix_frame_costs() {
    let (gpu, timer) = timed_gpu();
    println!("adapter: {:?}", gpu.adapter.get_info().name);
    let extent = ImageExtent::new(1920, 1080).unwrap();
    let target = OffscreenTarget::new(&gpu, extent).unwrap();
    let mut scene = matrix_scene(0);
    let perspective = pinhole(extent);
    let parallel = orthographic(extent);
    let layers = 20_u64;
    println!(
        "shadow maps: {layers} layers x 1024^2 x 4 B = {:.1} MiB; HDR target {:.1} MiB",
        (layers * 1024 * 1024 * 4) as f64 / 1048576.0,
        (1920.0 * 1080.0 * 8.0) / 1048576.0
    );
    println!("configuration,cold_ms,warm_ms,shadow_passes_cold");
    for (name, camera) in [
        ("pinhole", &perspective as &dyn CameraView),
        ("orthographic", &parallel),
    ] {
        let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
        let settings = PbrSettings {
            shadows: shadows(),
            ..Default::default()
        };
        let (mut cold, mut warm, mut passes) = (Vec::new(), Vec::new(), 0);
        for frame in 0..16 {
            if frame < 8 {
                scene.set_lighting(lights(0)).unwrap();
            }
            renderer
                .prepare_pbr(&gpu, &scene.snapshot(), camera, settings)
                .unwrap();
            let ms = timer.measure(&gpu, &target, |encoder, view| {
                renderer.encode(encoder, view)
            });
            if frame < 8 {
                passes = renderer.shadow_passes_last_frame();
                if frame > 0 {
                    cold.push(ms);
                }
            } else if frame > 8 {
                warm.push(ms);
            }
        }
        println!("{name},{:.3},{:.3},{passes}", median(cold), median(warm));
    }
    let background = plate(&gpu, extent);
    for (name, model) in [
        ("composited pinhole (4x MSAA)", CameraModel::Pinhole),
        ("composited distorted (4x MSAA)", lens(extent)),
    ] {
        let mut config = CompositionOutput::new(extent);
        config.pbr.shadows = shadows();
        let mut compositor = FrameCompositor::new(&gpu, OffscreenTarget::FORMAT, config).unwrap();
        compositor.set_sample_count(&gpu, 4).unwrap();
        let mut warm = Vec::new();
        for frame in 0..8 {
            compositor
                .prepare_model(
                    &gpu,
                    &scene.snapshot(),
                    &perspective,
                    &model,
                    &[ImageLayer::new(&background, ImageStage::DisplayBackground)],
                )
                .unwrap();
            let ms = timer.measure(&gpu, &target, |encoder, view| {
                compositor.encode(encoder, view.texture()).unwrap()
            });
            if frame > 0 {
                warm.push(ms);
            }
        }
        println!("{name},-,{:.3},-", median(warm));
    }
}
