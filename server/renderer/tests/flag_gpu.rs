//! Waving flags drawn from their compute-evaluated surface: placement by the instance
//! model, media-time animation, native cloth following its clock, shadows that follow
//! the surface, and the simple and PBR looks. World metres, ground Z=0, upwards -Z.

use std::sync::Arc;

use chr_renderer::{
    Gpu, MeshRenderer, OffscreenTarget, PbrSettings, ShadowMode, ShadowSettings,
    camera::Camera,
    coordinates::ImageExtent,
    scene::{
        Cloth, Filter, Flag, FlagMotion, FlagShading, Light, Lighting, MaterialMaps, MeshData,
        ModelMatrix, PbrMaterial, RevealDirection, Scene, TextureData, TextureSampler, Wave, Wrap,
    },
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
        motion: FlagMotion::Wave(Wave {
            amplitude,
            frequency: 1.35,
            speed: 1.0,
        }),
        shading,
        artwork_turns: 0,
        revealed: 1.0,
        reveal_direction: RevealDirection::CenterHorizontal,
    }
}

/// A red flag hanging upright in the XZ plane, its top edge 4 m up, facing -Y,
/// above a shadow-catcher ground, optionally lit by a sun from -Y.
fn scene(flag: Flag, lit: bool) -> Scene {
    scene_with(flag, lit, [0.8, 0.05, 0.05, 1.0], MaterialMaps::default())
}

/// [`scene`] with the flag's base colour and maps.
fn scene_with(flag: Flag, lit: bool, base_color: [f32; 4], maps: MaterialMaps) -> Scene {
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
        .add_material_with_maps(
            PbrMaterial {
                base_color,
                roughness: 0.8,
                ..Default::default()
            },
            maps,
        )
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

/// A native flag in a breeze towards its reading side (-Y in [`scene`]), at `seconds`
/// of its clock.
fn native(seconds: f64) -> Flag {
    Flag {
        motion: FlagMotion::Cloth(Cloth {
            wind_speed: 9.0,
            wind_direction: 90.0,
            auto_direction: true,
            tension: 0.65,
            seconds,
        }),
        ..flag(0.0, FlagShading::Pbr)
    }
}

#[test]
#[ignore = "requires a hardware GPU"]
fn native_flags_follow_their_clock_and_redraw_their_shadows() {
    let gpu = gpu();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let mut scene = scene(native(0.0), true);
    let (id, _) = scene
        .snapshot()
        .instances()
        .find(|(_, instance)| instance.flag().is_some())
        .unwrap();
    let first = render(&gpu, &mut renderer, &scene, 0.0);
    assert!(renderer.shadow_passes_last_frame() > 0);
    // The rest arc's top edge hangs pinned, 4 m up, tilted away from the sun.
    let top = [0.0, 0.0, -3.97];
    let [r, g, b] = at(&first, top);
    assert!(
        r > 20 && r > 3 * g.max(b),
        "red flag at its top edge: {r} {g} {b}"
    );
    let same = render(&gpu, &mut renderer, &scene, 5.0);
    assert_eq!(
        renderer.shadow_passes_last_frame(),
        0,
        "animation time alone moves no cloth"
    );
    assert_eq!(first, same);
    let mut previous = first;
    for frame in 1..=25 {
        scene
            .set_flag(id, Some(native(f64::from(frame) / 25.0)))
            .unwrap();
        let pixels = render(&gpu, &mut renderer, &scene, 0.0);
        assert!(
            renderer.shadow_passes_last_frame() > 0,
            "shadows follow the cloth"
        );
        assert_ne!(pixels, previous, "frame {frame} moved");
        previous = pixels;
    }
    let changed = same.iter().zip(&previous).filter(|(a, b)| a != b).count();
    assert!(
        changed > 400,
        "the cloth moved in a second: {changed} bytes changed"
    );
    // Its shading follows the cloth below, but the top edge stays where it was pinned.
    let [r, g, b] = at(&previous, top);
    assert!(
        r > 20 && r > 3 * g.max(b),
        "the top edge is pinned: {r} {g} {b}"
    );
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

/// Artwork of 2 x 2 texels: red, green on top, blue, white below.
fn quadrants() -> MaterialMaps {
    let pixels = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255; 4],
    ];
    let texture = TextureData::new(ImageExtent::new(2, 2).unwrap(), pixels.concat())
        .unwrap()
        .with_sampler(TextureSampler {
            wrap_u: Wrap::Clamp,
            wrap_v: Wrap::Clamp,
            mag: Filter::Nearest,
            min: Filter::Nearest,
            mip: None,
        });
    MaterialMaps {
        base_color: Some(Arc::new(texture)),
        ..Default::default()
    }
}

/// The quadrant colour a pixel shows: 0 red, 1 green, 2 blue, 3 white.
fn quadrant([r, g, b]: [u8; 3]) -> usize {
    let channels = [r, g, b];
    if channels.iter().all(|&c| c > 40)
        && channels.iter().max().unwrap() - channels.iter().min().unwrap() < 40
    {
        return 3;
    }
    (0..3).max_by_key(|&i| channels[i]).unwrap()
}

/// The world point of flag coordinates `(u, v)` of a flat 3.2 x 2 m flag in [`scene`].
fn on_flag(u: f64, v: f64) -> [f64; 3] {
    [3.2 * u - 1.6, 0.0, 2.0 * v - 4.0]
}

#[test]
#[ignore = "requires a hardware GPU"]
fn artwork_turns_and_reveals_follow_the_flag_with_either_material() {
    let gpu = gpu();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    for (shading, lit) in [(FlagShading::Simple, false), (FlagShading::Pbr, true)] {
        for turns in 0..4 {
            let flat = Flag {
                artwork_turns: turns,
                ..flag(0.0, shading)
            };
            let pixels = render(
                &gpu,
                &mut renderer,
                &scene_with(flat, lit, [1.0; 4], quadrants()),
                0.0,
            );
            for (u, v) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let [s, t] = flat.artwork_uv(u, v);
                let expected = 2 * usize::from(t > 0.5) + usize::from(s > 0.5);
                let colour = at(&pixels, on_flag(u, v));
                assert_eq!(
                    quadrant(colour),
                    expected,
                    "{shading:?}, {turns} turns, at ({u}, {v}): {colour:?}"
                );
            }
        }
        let mut shown = |revealed, direction, turns| {
            let flag = Flag {
                revealed,
                reveal_direction: direction,
                artwork_turns: turns,
                ..flag(0.0, shading)
            };
            render(
                &gpu,
                &mut renderer,
                &scene_with(flag, lit, [1.0; 4], quadrants()),
                0.0,
            )
        };
        // Grid points at least 0.1 from every half-progress edge and quadrant border.
        let points = [0.1, 0.4, 0.6, 0.9].map(|u| [0.1, 0.4, 0.6, 0.9].map(|v| (u, v)));
        for direction in DIRECTIONS {
            for turns in 0..4 {
                let (hidden, half, full) = (
                    shown(0.0, direction, turns),
                    shown(0.5, direction, turns),
                    shown(1.0, direction, turns),
                );
                let reference = Flag {
                    revealed: 0.5,
                    reveal_direction: direction,
                    artwork_turns: turns,
                    ..flag(0.0, shading)
                };
                for (u, v) in points.into_iter().flatten() {
                    let point = on_flag(u, v);
                    let [s, t] = reference.artwork_uv(u, v);
                    let artwork = 2 * usize::from(t > 0.5) + usize::from(s > 0.5);
                    let case = format!("{shading:?}, {direction:?}, {turns} turns, ({u}, {v})");
                    assert_eq!(quadrant(at(&full, point)), artwork, "{case}: complete");
                    assert_ne!(at(&full, point), at(&hidden, point), "{case}: hidden");
                    if reference.revealed_uv(u, v) == [u, v] {
                        assert_eq!(quadrant(at(&half, point)), artwork, "{case}: opened");
                    } else {
                        assert_eq!(at(&half, point), at(&hidden, point), "{case}: not yet");
                    }
                }
            }
        }
    }
}

const DIRECTIONS: [RevealDirection; 6] = [
    RevealDirection::CenterHorizontal,
    RevealDirection::CenterVertical,
    RevealDirection::LeftToRight,
    RevealDirection::RightToLeft,
    RevealDirection::TopToBottom,
    RevealDirection::BottomToTop,
];
