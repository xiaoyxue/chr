//! Ground decals blended over a display background: alpha, instance order, occlusion,
//! received catcher shadows and no cast shadows. World metres, ground Z=0, upwards -Z.

use chr_renderer::{
    Gpu, OffscreenTarget, ShadowMode, ShadowSettings,
    camera::Camera,
    composition::{
        AlphaMode, CompositionOutput, FrameCompositor, ImageLayer, ImageStage, ImageView, Transfer,
    },
    coordinates::ImageExtent,
    scene::{Light, Lighting, MaterialId, MeshData, ModelMatrix, PbrMaterial, Scene},
};

const SIZE: u32 = 128;

fn gpu() -> Gpu {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    gpu
}

/// A square of half-size `half` centred at (x, 0) at height `height`.
fn quad(x: f32, half: f32, height: f32) -> MeshData {
    MeshData::new(
        vec![
            [x - half, -half, -height],
            [x + half, -half, -height],
            [x + half, half, -height],
            [x - half, half, -height],
        ],
        vec![0, 2, 1, 0, 3, 2],
        std::iter::once(0..6).collect(),
    )
    .unwrap()
}

struct Builder {
    scene: Scene,
}

impl Builder {
    /// Catcher ground and two equal directional lights at 45 degrees towards +X and -X.
    fn new() -> Self {
        let mut scene = Scene::new().unwrap();
        let catcher = scene
            .add_material(PbrMaterial {
                base_color: [0.5, 0.5, 0.5, 1.0],
                roughness: 1.0,
                shadow_catcher: true,
                ..Default::default()
            })
            .unwrap();
        let mut builder = Self { scene };
        builder.add(quad(0.0, 10.0, 0.0), catcher);
        let light = |direction| Light::Directional {
            direction,
            irradiance: [2.0; 3],
            angular_radius: 0.0,
            shadow_mode: None,
        };
        builder
            .scene
            .set_lighting(
                Lighting::new(vec![light([1.0, 0.0, 1.0]), light([-1.0, 0.0, 1.0])], None).unwrap(),
            )
            .unwrap();
        builder
    }

    fn decal(&mut self, mesh: MeshData, color: [f32; 4]) {
        let material = self
            .scene
            .add_material(PbrMaterial {
                base_color: color,
                unlit: true,
                double_sided: true,
                decal: true,
                ..Default::default()
            })
            .unwrap();
        self.add(mesh, material);
    }

    fn occluder(&mut self, mesh: MeshData) {
        let grey = self
            .scene
            .add_material(PbrMaterial {
                double_sided: true,
                ..Default::default()
            })
            .unwrap();
        self.add(mesh, grey);
    }

    fn add(&mut self, mesh: MeshData, material: MaterialId) {
        let mesh = self.scene.add_mesh(mesh).unwrap();
        self.scene
            .add_instance(mesh, vec![material], ModelMatrix::IDENTITY)
            .unwrap();
    }
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

/// Linear red of the composited pixel at a ground point.
fn red(pixels: &[u8], camera: &Camera, world: [f64; 3]) -> f64 {
    let (p, _) = camera.project(world).unwrap();
    let code =
        f64::from(pixels[4 * (p[1].round() as usize * SIZE as usize + p[0].round() as usize)])
            / 255.0;
    if code <= 0.04045 {
        code / 12.92
    } else {
        ((code + 0.055) / 1.055).powf(2.4)
    }
}

/// Composite over a uniform sRGB background of code 188 (linear 0.5), PCF shadows.
fn composite(gpu: &Gpu, scene: &Scene, camera: &Camera) -> Vec<u8> {
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
    let mut config = CompositionOutput::new(camera.extent());
    config.pbr.shadows = ShadowSettings {
        mode: ShadowMode::Pcf,
        ..Default::default()
    };
    let mut compositor = FrameCompositor::new(gpu, OffscreenTarget::FORMAT, config).unwrap();
    compositor
        .prepare(
            gpu,
            &scene.snapshot(),
            camera,
            &[ImageLayer::new(&plate, ImageStage::DisplayBackground)],
        )
        .unwrap();
    let target = OffscreenTarget::new(gpu, camera.extent()).unwrap();
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

#[test]
#[ignore = "requires a hardware GPU"]
fn decals_blend_by_alpha_in_instance_order_under_occluders() {
    let gpu = gpu();
    let camera = top_camera();
    let centre = [0.0, 0.0, 0.0];
    let render = |decals: &[[f32; 4]], occluded: bool| {
        let mut builder = Builder::new();
        for &color in decals {
            builder.decal(quad(0.0, 2.0, 0.02), color);
        }
        if occluded {
            builder.occluder(quad(0.0, 0.5, 1.0));
        }
        composite(&gpu, &builder.scene, &camera)
    };
    let plate = red(&render(&[], false), &camera, centre);
    let opaque = red(&render(&[[1.0, 0.0, 0.0, 1.0]], false), &camera, centre);
    let half = red(&render(&[[1.0, 0.0, 0.0, 0.5]], false), &camera, centre);
    assert!((plate - 0.5).abs() < 0.01, "plate {plate}");
    assert!(
        (half - (opaque + plate) / 2.0).abs() < 0.01,
        "alpha*c + (1-alpha)*plate: {half} vs {opaque} and {plate}"
    );
    let red_on_top = red(
        &render(&[[0.0, 0.0, 1.0, 1.0], [1.0, 0.0, 0.0, 1.0]], false),
        &camera,
        centre,
    );
    let blue_on_top = red(
        &render(&[[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]], false),
        &camera,
        centre,
    );
    assert!(
        (red_on_top - opaque).abs() < 0.005,
        "later instances are on top"
    );
    assert!(blue_on_top < 0.01, "{blue_on_top}");
    let hidden = render(&[[1.0, 0.0, 0.0, 1.0]], true);
    let occluder = render(&[], true);
    let at = |pixels: &[u8]| red(pixels, &camera, [0.0, 0.0, -1.0]);
    assert!(
        (at(&hidden) - at(&occluder)).abs() < 0.005,
        "an occluder hides decals"
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn catcher_shadows_darken_decals_and_decals_cast_none() {
    let gpu = gpu();
    let camera = top_camera();
    // The occluder 1 m up shadows (1, 0) from the light travelling towards +X.
    let mut shadowed = Builder::new();
    shadowed.decal(quad(1.5, 1.5, 0.02), [1.0, 0.0, 0.0, 1.0]);
    shadowed.occluder(quad(0.0, 0.5, 1.0));
    let pixels = composite(&gpu, &shadowed.scene, &camera);
    let lit = red(&pixels, &camera, [2.5, 0.0, 0.0]);
    let shade = red(&pixels, &camera, [1.0, 0.0, 0.0]);
    // The catcher halves the decal's radiance before the tone map, like a decal of
    // half the colour in the open.
    let mut dimmer = Builder::new();
    dimmer.decal(quad(1.5, 1.5, 0.02), [0.5, 0.0, 0.0, 1.0]);
    let half = red(
        &composite(&gpu, &dimmer.scene, &camera),
        &camera,
        [1.0, 0.0, 0.0],
    );
    assert!(lit > 0.5, "lit decal {lit}");
    assert!(
        (shade - half).abs() < 0.01 && shade < lit,
        "one of two lights blocked halves the decal: {shade} vs {half}, lit {lit}"
    );

    // A decal where the occluder was: nothing shadows (1, 0) any more.
    let mut floating = Builder::new();
    floating.decal(quad(0.0, 0.5, 1.0), [0.0, 0.0, 1.0, 1.0]);
    let pixels = composite(&gpu, &floating.scene, &camera);
    let open = red(&pixels, &camera, [1.0, 0.0, 0.0]);
    assert!((open - 0.5).abs() < 0.01, "decals cast no shadow: {open}");
}
