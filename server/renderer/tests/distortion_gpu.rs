//! Pinhole and `radial-poly-native-px-v1` composition on synthetic scenes.
//! CG texel k shades native coordinate k + 0.5 (framebuffer sample positions).

use chr_renderer::{
    Gpu, OffscreenTarget,
    camera::Camera,
    composition::{
        AlphaMode, CompositionOutput, FrameCompositor, ImageLayer, ImageStage, ImageView, Transfer,
    },
    coordinates::ImageExtent,
    distortion::{CameraModel, FrameWarp, LensDistortion, RadialDistortion},
    scene::{Light, Lighting, MeshData, ModelMatrix, PbrMaterial, Scene},
};

const W: u32 = 320;
const H: u32 = 180;

fn gpu() -> Gpu {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    gpu
}

fn extent() -> ImageExtent {
    ImageExtent::new(W, H).unwrap()
}

fn camera() -> Camera {
    Camera::look_at(
        extent(),
        [0.0, 12.0, -8.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, -1.0],
        0.9,
        0.1,
        200.0,
    )
    .unwrap()
}

fn lens(k: [f64; 3], warp: bool) -> LensDistortion {
    let radial = RadialDistortion::new([160.0, 90.0], 160.0, k, None).unwrap();
    let s = f64::from(W * H).sqrt();
    // Pixels -> SNC with a small rotation, zoom and shift, like a broadcast postwarp.
    let (c, sn) = (0.02_f64.cos() * 1.01, 0.02_f64.sin() * 1.01);
    let frame_warp = warp.then(|| {
        FrameWarp::new([
            [2.0 * c / s, -2.0 * sn / s, -f64::from(W) / s + 0.01],
            [2.0 * sn / s, 2.0 * c / s, -f64::from(H) / s - 0.006],
            [2e-5, 0.0, 1.0],
        ])
        .unwrap()
    });
    LensDistortion::new(extent(), radial, frame_warp)
}

fn quad(corners: [[f32; 3]; 4]) -> MeshData {
    MeshData::new(
        corners.to_vec(),
        vec![0, 2, 1, 0, 3, 2],
        std::iter::once(0..6).collect(),
    )
    .unwrap()
}

fn texture(gpu: &Gpu, pixels: &[u8]) -> ImageView {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: W,
            height: H,
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
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(W * 4),
            rows_per_image: Some(H),
        },
        texture.size(),
    );
    ImageView::new(&texture, Transfer::Srgb, AlphaMode::Straight).unwrap()
}

/// A horizontal/vertical gradient standing in for already-distorted footage.
fn gradient(gpu: &Gpu) -> ImageView {
    let mut pixels = Vec::new();
    for y in 0..H {
        for x in 0..W {
            pixels.extend_from_slice(&[(x * 255 / W) as u8, (y * 255 / H) as u8, 90, 255]);
        }
    }
    texture(gpu, &pixels)
}

fn render(
    gpu: &Gpu,
    scene: &Scene,
    model: &CameraModel,
    plate: Option<&ImageView>,
    config: CompositionOutput,
) -> Vec<u8> {
    let mut compositor = FrameCompositor::new(gpu, OffscreenTarget::FORMAT, config).unwrap();
    let layers: Vec<_> = plate
        .into_iter()
        .map(|p| ImageLayer::new(p, ImageStage::DisplayBackground))
        .collect();
    compositor
        .prepare_model(gpu, &scene.snapshot(), &camera(), model, &layers)
        .unwrap();
    let target = OffscreenTarget::new(gpu, extent()).unwrap();
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

/// Lit ground with a box, so CG covers part of the frame with real shading.
fn shaded_scene() -> Scene {
    let mut scene = Scene::new().unwrap();
    let ground = scene
        .add_mesh(quad([
            [-20.0, -20.0, 0.0],
            [20.0, -20.0, 0.0],
            [20.0, 20.0, 0.0],
            [-20.0, 20.0, 0.0],
        ]))
        .unwrap();
    let material = scene
        .add_material(PbrMaterial {
            base_color: [0.3, 0.6, 0.2, 1.0],
            roughness: 0.7,
            double_sided: true,
            ..Default::default()
        })
        .unwrap();
    scene
        .add_instance(ground, vec![material], ModelMatrix::IDENTITY)
        .unwrap();
    scene
        .set_lighting(
            Lighting::new(
                vec![Light::Directional {
                    direction: [0.3, -0.4, 1.0],
                    irradiance: [3.0; 3],
                    angular_radius: 0.0,
                    shadow_mode: None,
                }],
                None,
            )
            .unwrap(),
        )
        .unwrap();
    scene
}

#[test]
#[ignore = "requires a hardware GPU"]
fn zero_coefficients_reproduce_the_pinhole_model() {
    let gpu = gpu();
    let plate = gradient(&gpu);
    let scene = shaded_scene();
    let config = CompositionOutput::new(extent());
    let pinhole = render(&gpu, &scene, &CameraModel::Pinhole, Some(&plate), config);
    let identity = CameraModel::Distorted(Box::new(lens([0.0; 3], false)));
    let distorted = render(&gpu, &scene, &identity, Some(&plate), config);
    let worst = pinhole
        .iter()
        .zip(&distorted)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    assert!(worst <= 1, "max byte difference {worst}");
}

/// Small camera-facing unlit squares at ground points spread over the frame.
fn marker_scene(camera: &Camera) -> (Scene, Vec<[f64; 3]>) {
    let mut scene = Scene::new().unwrap();
    let white = scene
        .add_material(PbrMaterial {
            unlit: true,
            double_sided: true,
            ..Default::default()
        })
        .unwrap();
    let pose = camera.camera_to_world();
    let right = [pose[0][0], pose[1][0], pose[2][0]];
    let down = [pose[0][1], pose[1][1], pose[2][1]];
    let mut centres = Vec::new();
    for fx in [0.1, 0.3, 0.5, 0.7, 0.9] {
        for fy in [0.12, 0.5, 0.88] {
            let ray = camera.ray([f64::from(W) * fx, f64::from(H) * fy]).unwrap();
            let t = -ray.origin[2] / ray.direction[2];
            let centre: [f64; 3] = std::array::from_fn(|i| ray.origin[i] + t * ray.direction[i]);
            // About 4 px across at this depth.
            let half = 2.0 * t / camera.intrinsics().fx;
            let corner = |a: f64, b: f64| -> [f32; 3] {
                std::array::from_fn(|i| {
                    (centre[i] + a * half * right[i] + b * half * down[i]) as f32
                })
            };
            let mesh = scene
                .add_mesh(quad([
                    corner(-1.0, -1.0),
                    corner(1.0, -1.0),
                    corner(1.0, 1.0),
                    corner(-1.0, 1.0),
                ]))
                .unwrap();
            scene
                .add_instance(mesh, vec![white], ModelMatrix::IDENTITY)
                .unwrap();
            centres.push(centre);
        }
    }
    (scene, centres)
}

/// Intensity-weighted centroid in native coordinates (texel index + 0.5).
fn centroid(pixels: &[u8], around: [f64; 2]) -> [f64; 2] {
    let (mut sum, mut sx, mut sy) = (0.0, 0.0, 0.0);
    let (cx, cy) = (around[0] as i64, around[1] as i64);
    for y in (cy - 8).max(0)..(cy + 9).min(i64::from(H)) {
        for x in (cx - 8).max(0)..(cx + 9).min(i64::from(W)) {
            let value = f64::from(pixels[((y * i64::from(W) + x) * 4) as usize]);
            sum += value;
            sx += value * (x as f64 + 0.5);
            sy += value * (y as f64 + 0.5);
        }
    }
    assert!(sum > 0.0, "no marker near {around:?}");
    [sx / sum, sy / sum]
}

#[test]
#[ignore = "requires a hardware GPU"]
fn markers_follow_both_camera_models() {
    let gpu = gpu();
    let camera = camera();
    let (scene, centres) = marker_scene(&camera);
    let config = CompositionOutput::new(extent());
    let distorted = CameraModel::Distorted(Box::new(lens([-0.08, 0.01, 0.0], true)));
    let mut largest_shift: f64 = 0.0;
    for model in [CameraModel::Pinhole, distorted] {
        let pixels = render(&gpu, &scene, &model, None, config);
        for &centre in &centres {
            let expected = model.project(&camera, centre).unwrap();
            let found = centroid(&pixels, expected.native);
            let error = (found[0] - expected.native[0]).hypot(found[1] - expected.native[1]);
            assert!(
                error < 0.3,
                "{model:?}: {found:?} vs {:?} ({error})",
                expected.native
            );
            largest_shift = largest_shift.max(
                (expected.native[0] - expected.ideal[0])
                    .hypot(expected.native[1] - expected.ideal[1]),
            );
        }
    }
    assert!(
        largest_shift > 5.0,
        "distortion must move edge markers: {largest_shift}"
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn footage_is_not_warped_and_non_invertible_pixels_get_no_cg() {
    let gpu = gpu();
    let plate = gradient(&gpu);
    let config = CompositionOutput::new(extent());
    // Strong barrel distortion: the corners have no ideal preimage.
    let strong = lens([-0.3, 0.0, 0.0], false);
    let corner = [0.5, 0.5];
    assert!(strong.ideal_pixel(corner).is_err());
    let model = CameraModel::Distorted(Box::new(strong));
    let empty = Scene::new().unwrap();
    let pinhole_plate = render(&gpu, &empty, &CameraModel::Pinhole, Some(&plate), config);
    let distorted_plate = render(&gpu, &empty, &model, Some(&plate), config);
    assert_eq!(pinhole_plate, distorted_plate, "footage must not be warped");
    // A ground plane fills the ideal view; corners still show unwarped footage.
    let filled = render(&gpu, &shaded_scene(), &model, Some(&plate), config);
    let index = |x: u32, y: u32| ((y * W + x) * 4) as usize;
    assert_eq!(filled[index(0, 0)..index(0, 0) + 4], pinhole_plate[0..4]);
    let centre = index(W / 2, H / 2);
    assert_ne!(
        filled[centre..centre + 4],
        pinhole_plate[centre..centre + 4]
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn guard_band_counts_against_the_composition_budget() {
    let gpu = gpu();
    let mut config = CompositionOutput::new(extent());
    // Exactly the pinhole native and output canvases; the guard band does not fit.
    config.max_intermediate_bytes = u64::from(W * H) * 8 * 2;
    let mut compositor = FrameCompositor::new(&gpu, OffscreenTarget::FORMAT, config).unwrap();
    let scene = shaded_scene().snapshot();
    compositor
        .prepare_model(&gpu, &scene, &camera(), &CameraModel::Pinhole, &[])
        .unwrap();
    let distorted = CameraModel::Distorted(Box::new(lens([-0.08, 0.0, 0.0], false)));
    assert!(
        compositor
            .prepare_model(&gpu, &scene, &camera(), &distorted, &[])
            .is_err()
    );
}
