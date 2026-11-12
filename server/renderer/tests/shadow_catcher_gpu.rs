//! Shadow catchers composited over a display background, and spot shadows whose scene
//! bounds extend behind the light. World metres, ground Z=0, upwards -Z.

use chr_renderer::{
    Gpu, MeshRenderer, OffscreenTarget, PbrSettings, ShadowMode, ShadowSettings, ToneMap,
    camera::Camera,
    composition::{
        AlphaMode, CompositionOutput, FrameCompositor, ImageLayer, ImageStage, ImageView, Transfer,
    },
    coordinates::ImageExtent,
    scene::{Light, Lighting, MeshData, ModelMatrix, PbrMaterial, Scene},
};

const SIZE: u32 = 128;

fn gpu() -> Gpu {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    gpu
}

fn quad(half: f32, height: f32) -> MeshData {
    MeshData::new(
        vec![
            [-half, -half, -height],
            [half, -half, -height],
            [half, half, -height],
            [-half, half, -height],
        ],
        vec![0, 2, 1, 0, 3, 2],
        std::iter::once(0..6).collect(),
    )
    .unwrap()
}

/// Catcher ground, a 1 m plate 1 m above the origin, and directional lights at 45 degrees.
fn catcher_scene(directions: &[[f64; 3]]) -> Scene {
    let mut scene = Scene::new().unwrap();
    let ground = scene.add_mesh(quad(10.0, 0.0)).unwrap();
    let catcher = scene
        .add_material(PbrMaterial {
            base_color: [0.5, 0.5, 0.5, 1.0],
            roughness: 1.0,
            shadow_catcher: true,
            ..Default::default()
        })
        .unwrap();
    scene
        .add_instance(ground, vec![catcher], ModelMatrix::IDENTITY)
        .unwrap();
    let plate = scene.add_mesh(quad(0.5, 1.0)).unwrap();
    let grey = scene
        .add_material(PbrMaterial {
            double_sided: true,
            ..Default::default()
        })
        .unwrap();
    scene
        .add_instance(plate, vec![grey], ModelMatrix::IDENTITY)
        .unwrap();
    let lights = directions
        .iter()
        .map(|&direction| Light::Directional {
            direction,
            irradiance: [2.0; 3],
            angular_radius: 0.0,
            shadow_mode: None,
        })
        .collect();
    scene
        .set_lighting(Lighting::new(lights, None).unwrap())
        .unwrap();
    scene
}

fn top_camera() -> Camera {
    Camera::look_at(
        ImageExtent::new(SIZE, SIZE).unwrap(),
        [0.0, 0.0, -10.0],
        [0.0; 3],
        [0.0, -1.0, 0.0],
        0.6,
        0.1,
        100.0,
    )
    .unwrap()
}

fn pixel(camera: &Camera, world: [f64; 3]) -> usize {
    let (p, _) = camera.project(world).unwrap();
    4 * (p[1].round() as usize * SIZE as usize + p[0].round() as usize)
}

fn shadows(mode: ShadowMode) -> ShadowSettings {
    ShadowSettings {
        mode,
        ..Default::default()
    }
}

/// Composite over a uniform sRGB background of code 188 (linear 0.5).
fn composite(gpu: &Gpu, scene: &Scene, camera: &Camera, mode: ShadowMode) -> Vec<u8> {
    let extent = camera.extent();
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &[188, 188, 188, 255].repeat((SIZE * SIZE) as usize),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIZE * 4),
            rows_per_image: Some(SIZE),
        },
        texture.size(),
    );
    let plate = ImageView::new(&texture, Transfer::Srgb, AlphaMode::Straight).unwrap();
    let mut config = CompositionOutput::new(extent);
    config.pbr.shadows = shadows(mode);
    let mut compositor = FrameCompositor::new(gpu, OffscreenTarget::FORMAT, config).unwrap();
    compositor
        .prepare(
            gpu,
            &scene.snapshot(),
            camera,
            &[ImageLayer::new(&plate, ImageStage::DisplayBackground)],
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

fn standalone(gpu: &Gpu, scene: &Scene, camera: &Camera, shadows: ShadowSettings) -> Vec<u8> {
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    renderer
        .prepare_pbr(
            gpu,
            &scene.snapshot(),
            camera,
            PbrSettings {
                tone_map: ToneMap::Reinhard,
                shadows,
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

#[test]
#[ignore = "requires a hardware GPU"]
fn shadow_catchers_multiply_the_background_by_the_shadowed_light_fraction() {
    let gpu = gpu();
    let camera = top_camera();
    // Light A travels towards +X, so the plate shadows (1, 0); light B shadows (-1, 0).
    let a = [1.0, 0.0, 1.0];
    let b = [-1.0, 0.0, 1.0];
    let (in_a, in_b, open, plate) = (
        pixel(&camera, [1.0, 0.0, 0.0]),
        pixel(&camera, [-1.0, 0.0, 0.0]),
        pixel(&camera, [2.5, 1.5, 0.0]),
        pixel(&camera, [0.0, 0.0, -1.0]),
    );
    let both = composite(&gpu, &catcher_scene(&[a, b]), &camera, ShadowMode::Pcf);
    // Lit ground keeps the background; one of two equal lights blocked halves it in
    // linear light: sRGB 188 (0.5) becomes 137 (0.25).
    assert!(both[open].abs_diff(188) <= 1, "{}", both[open]);
    for index in [in_a, in_b] {
        assert!(both[index].abs_diff(137) <= 2, "{}", both[index]);
    }
    assert_eq!(both[open..open + 3], [both[open]; 3]);
    // Real geometry stays opaque CG, not background.
    assert!(both[plate].abs_diff(188) > 10, "{}", both[plate]);
    let single = composite(&gpu, &catcher_scene(&[a]), &camera, ShadowMode::Pcf);
    assert!(single[in_a] <= 2, "fully shadowed: {}", single[in_a]);
    assert!(single[in_b].abs_diff(188) <= 1);
    let unshadowed = composite(&gpu, &catcher_scene(&[a, b]), &camera, ShadowMode::Off);
    for index in [in_a, in_b, open] {
        assert!(
            unshadowed[index].abs_diff(188) <= 1,
            "{}",
            unshadowed[index]
        );
    }
}

#[test]
#[ignore = "requires a hardware GPU"]
fn standalone_catchers_draw_no_colour_and_darken_the_clear_colour() {
    let gpu = gpu();
    let camera = top_camera();
    let scene = catcher_scene(&[[1.0, 0.0, 1.0]]);
    let mut empty = Scene::new().unwrap();
    empty
        .set_lighting(scene.snapshot().lighting().clone())
        .unwrap();
    let clear = standalone(&gpu, &empty, &camera, shadows(ShadowMode::Off));
    let open = pixel(&camera, [2.5, 1.5, 0.0]);
    let shadowed = pixel(&camera, [1.0, 0.0, 0.0]);
    let off = standalone(&gpu, &scene, &camera, shadows(ShadowMode::Off));
    assert_eq!(off[open..open + 4], clear[open..open + 4]);
    assert_eq!(off[shadowed..shadowed + 4], clear[shadowed..shadowed + 4]);
    let on = standalone(&gpu, &scene, &camera, shadows(ShadowMode::Pcf));
    assert_eq!(on[open..open + 4], clear[open..open + 4]);
    assert!(
        on[shadowed] < clear[shadowed] / 2,
        "{} vs {}",
        on[shadowed],
        clear[shadowed]
    );
}

/// Ground extending behind a spot used to collapse its fitted near plane to 1 cm, where a
/// perspective-depth bias of 0.0005 spanned many metres and erased the shadow.
#[test]
#[ignore = "requires a hardware GPU"]
fn spot_shadows_survive_geometry_behind_the_light() {
    let gpu = gpu();
    let mut scene = Scene::new().unwrap();
    let ground = scene.add_mesh(quad(100.0, 0.0)).unwrap();
    let plate = scene.add_mesh(quad(1.0, 8.0)).unwrap();
    let material = scene
        .add_material(PbrMaterial {
            roughness: 1.0,
            double_sided: true,
            ..Default::default()
        })
        .unwrap();
    scene
        .add_instance(ground, vec![material], ModelMatrix::IDENTITY)
        .unwrap();
    scene
        .add_instance(plate, vec![material], ModelMatrix::IDENTITY)
        .unwrap();
    scene
        .set_lighting(
            Lighting::new(
                vec![Light::Spot {
                    position: [40.0, 0.0, -30.0],
                    direction: [-40.0, 0.0, 30.0],
                    intensity: [36000.0; 3],
                    inner_angle: 0.4,
                    outer_angle: 0.6,
                    source_radius: 0.0,
                    shadow_mode: None,
                }],
                None,
            )
            .unwrap(),
        )
        .unwrap();
    let camera = Camera::look_at(
        ImageExtent::new(SIZE, SIZE).unwrap(),
        [-7.0, 0.0, -40.0],
        [-7.0, 0.0, 0.0],
        [0.0, -1.0, 0.0],
        0.7,
        0.1,
        200.0,
    )
    .unwrap();
    // The plate's shadow centre on the ground, and a lit point beside it.
    let shadowed = pixel(&camera, [-14.5, 0.0, 0.0]);
    let lit = pixel(&camera, [-14.5, 6.0, 0.0]);
    for mode in [ShadowMode::Hard, ShadowMode::Pcf] {
        let pixels = standalone(&gpu, &scene, &camera, shadows(mode));
        assert!(pixels[lit] > 60, "{mode:?} lit {}", pixels[lit]);
        assert!(
            pixels[shadowed] < pixels[lit] / 4,
            "{mode:?}: shadow {} vs lit {}",
            pixels[shadowed],
            pixels[lit]
        );
    }
}
