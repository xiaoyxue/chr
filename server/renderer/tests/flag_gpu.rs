//! Waving flags drawn from their compute-evaluated surface: placement by the instance
//! model, media-time animation, shadows that follow the surface, and the simple and
//! PBR looks. World metres, ground Z=0, upwards -Z.

use chr_renderer::{
    Gpu, MeshRenderer, OffscreenTarget, PbrSettings, ShadowMode, ShadowSettings,
    camera::Camera,
    coordinates::ImageExtent,
    scene::{Flag, FlagShading, Light, Lighting, MeshData, ModelMatrix, PbrMaterial, Scene},
};

const SIZE: u32 = 160;

fn gpu() -> Gpu {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    gpu
}

fn flag(amplitude: f32, shading: FlagShading) -> Flag {
    Flag {
        width: 3.2,
        height: 2.0,
        amplitude,
        frequency: 1.35,
        speed: 1.0,
        shading,
    }
}

/// A red flag hanging upright in the XZ plane, its top edge 4 m up, facing -Y,
/// above a shadow-catcher ground, optionally lit by a sun from -Y.
fn scene(flag: Flag, lit: bool) -> Scene {
    let mut scene = Scene::new().unwrap();
    let ground = scene
        .add_mesh(
            MeshData::new(
                vec![
                    [-10.0, -10.0, 0.0],
                    [10.0, -10.0, 0.0],
                    [10.0, 10.0, 0.0],
                    [-10.0, 10.0, 0.0],
                ],
                vec![0, 2, 1, 0, 3, 2],
                std::iter::once(0..6).collect(),
            )
            .unwrap(),
        )
        .unwrap();
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
    let grid = scene.add_mesh(MeshData::flag_grid()).unwrap();
    let red = scene
        .add_material(PbrMaterial {
            base_color: [0.8, 0.05, 0.05, 1.0],
            roughness: 0.8,
            ..Default::default()
        })
        .unwrap();
    // Columns: local X along world X, local Y (down the flag) along world +Z, and
    // their cross product along world -Y; the origin is the top-left corner.
    let model = ModelMatrix::new([
        [1.0, 0.0, 0.0, -1.6],
        [0.0, 0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0, -4.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
    .unwrap();
    let id = scene.add_instance(grid, vec![red], model).unwrap();
    scene.set_flag(id, Some(flag)).unwrap();
    if lit {
        let sun = Light::Directional {
            direction: [0.0, 1.0, 1.0],
            irradiance: [3.0; 3],
            angular_radius: 0.0,
            shadow_mode: None,
        };
        scene
            .set_lighting(Lighting::new(vec![sun], None).unwrap())
            .unwrap();
    }
    scene
}

fn camera() -> Camera {
    Camera::look_at(
        ImageExtent::new(SIZE, SIZE).unwrap(),
        [0.0, -12.0, -3.0],
        [0.0, 0.0, -2.5],
        [0.0, 0.0, -1.0],
        0.6,
        0.1,
        100.0,
    )
    .unwrap()
}

fn render(gpu: &Gpu, renderer: &mut MeshRenderer, scene: &Scene, seconds: f64) -> Vec<u8> {
    renderer.set_animation_time(seconds);
    renderer
        .prepare_pbr(
            gpu,
            &scene.snapshot(),
            &camera(),
            PbrSettings {
                shadows: ShadowSettings {
                    mode: ShadowMode::Pcf,
                    resolution: 512,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    let mut pixels = Vec::new();
    OffscreenTarget::new(gpu, camera().extent())
        .unwrap()
        .render_rgba(
            gpu,
            |encoder, view| renderer.encode(encoder, view),
            &mut pixels,
        )
        .unwrap();
    pixels
}

/// The RGB codes at a world point.
fn at(pixels: &[u8], world: [f64; 3]) -> [u8; 3] {
    let (p, _) = camera().project(world).unwrap();
    let i = 4 * (p[1].round() as usize * SIZE as usize + p[0].round() as usize);
    [pixels[i], pixels[i + 1], pixels[i + 2]]
}

/// A point on the pinned centre line, which never moves.
const CENTRE: [f64; 3] = [0.0, 0.0, -3.0];

#[test]
#[ignore = "requires a hardware GPU"]
fn flags_wave_with_media_time_and_redraw_their_shadows() {
    let gpu = gpu();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let waving = scene(flag(0.5, FlagShading::Pbr), true);
    let first = render(&gpu, &mut renderer, &waving, 0.0);
    assert!(renderer.shadow_passes_last_frame() > 0);
    let [r, g, b] = at(&first, CENTRE);
    assert!(
        r > 60 && r > 3 * g.max(b),
        "lit red flag at the pinned centre: {r} {g} {b}"
    );
    let same = render(&gpu, &mut renderer, &waving, 0.0);
    assert_eq!(
        renderer.shadow_passes_last_frame(),
        0,
        "cached until it moves"
    );
    assert_eq!(first, same);
    let later = render(&gpu, &mut renderer, &waving, 0.4);
    assert!(
        renderer.shadow_passes_last_frame() > 0,
        "shadows follow the surface"
    );
    let changed = first.iter().zip(&later).filter(|(a, b)| a != b).count();
    assert!(changed > 400, "the flag moved: {changed} bytes changed");
    assert_eq!(
        at(&first, CENTRE),
        at(&later, CENTRE),
        "the centre line is pinned"
    );

    let still = scene(flag(0.0, FlagShading::Pbr), true);
    let a = render(&gpu, &mut renderer, &still, 0.0);
    let b = render(&gpu, &mut renderer, &still, 5.0);
    assert_eq!(
        renderer.shadow_passes_last_frame(),
        0,
        "a flat flag never moves"
    );
    assert_eq!(a, b);
}

#[test]
#[ignore = "requires a hardware GPU"]
fn the_simple_look_ignores_scene_lights_and_pbr_follows_them() {
    let gpu = gpu();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let mut dark = |shading| {
        let pixels = render(&gpu, &mut renderer, &scene(flag(0.3, shading), false), 1.0);
        at(&pixels, CENTRE)
    };
    let [r, g, b] = dark(FlagShading::Simple);
    assert!(
        r > 60 && r > 3 * g.max(b),
        "simple flag is lit without lights: {r} {g} {b}"
    );
    assert!(dark(FlagShading::Pbr)[0] < 10, "a PBR flag needs light");

    renderer
        .prepare(
            &gpu,
            &scene(flag(0.3, FlagShading::Pbr), false).snapshot(),
            &camera(),
        )
        .unwrap();
    let mut pixels = Vec::new();
    OffscreenTarget::new(&gpu, camera().extent())
        .unwrap()
        .render_rgba(
            &gpu,
            |encoder, view| renderer.encode(encoder, view),
            &mut pixels,
        )
        .unwrap();
    let [r, g, b] = at(&pixels, CENTRE);
    // Linear 0.8 and 0.05 encode to sRGB 231 and 63.
    assert!(
        r.abs_diff(231) <= 2 && g.abs_diff(63) <= 2 && b.abs_diff(63) <= 2,
        "unlit view shows the base colour: {r} {g} {b}"
    );
}
