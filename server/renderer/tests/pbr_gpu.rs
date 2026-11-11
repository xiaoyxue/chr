use chr_renderer::{
    Gpu, MeshRenderer, OffscreenTarget, PbrSettings, ToneMap,
    camera::Camera,
    coordinates::ImageExtent,
    scene::{
        Light, Lighting, MaterialMaps, MeshAttributes, MeshData, ModelMatrix, PbrMaterial, Scene,
        TextureData,
    },
};
use std::sync::Arc;

fn capture(
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
fn texture(pixel: [u8; 4]) -> Arc<TextureData> {
    Arc::new(TextureData::new(ImageExtent::new(1, 1).unwrap(), pixel.to_vec()).unwrap())
}
fn encoded(value: f32) -> u8 {
    let value = value / (1.0 + value);
    ((if value <= 0.0031308 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }) * 255.0)
        .round() as u8
}

#[test]
#[ignore = "requires a hardware GPU"]
fn pbr_maps_colour_space_exposure_mask_and_light_isolation() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let extent = ImageExtent::new(64, 64).unwrap();
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
    let mesh = scene
        .add_mesh(
            MeshData::new(
                vec![
                    [-2.0, -2.0, 0.0],
                    [2.0, -2.0, 0.0],
                    [2.0, 2.0, 0.0],
                    [-2.0, 2.0, 0.0],
                ],
                vec![0, 2, 1, 0, 3, 2],
                std::iter::once(0..6).collect(),
            )
            .unwrap()
            .with_attributes(MeshAttributes {
                tex_coords: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
    let mut maps = MaterialMaps {
        base_color: Some(texture([128, 128, 128, 255])),
        ..Default::default()
    };
    let mut factors = PbrMaterial {
        roughness: 1.0,
        ..Default::default()
    };
    let material = scene.add_material_with_maps(factors, maps.clone()).unwrap();
    let instance = scene
        .add_instance(mesh, vec![material], ModelMatrix::IDENTITY)
        .unwrap();
    scene
        .set_lighting(
            Lighting::new(
                vec![Light::Directional {
                    direction: [0.0, 0.0, 1.0],
                    irradiance: [std::f32::consts::PI; 3],
                }],
                None,
            )
            .unwrap(),
        )
        .unwrap();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let center = (32 * 64 + 32) * 4;
    let reference = capture(&gpu, &mut renderer, &scene, &camera, 1.0);
    let linear = ((128.0_f32 / 255.0 + 0.055) / 1.055).powf(2.4);
    let expected = encoded(0.96 * linear + 0.01);
    assert!(
        reference[center].abs_diff(expected) <= 3,
        "{} vs {}",
        reference[center],
        expected
    );
    let original = scene.snapshot();
    let mut mirrored = ModelMatrix::IDENTITY.rows();
    mirrored[0][0] = -1.0;
    scene
        .set_model(instance, ModelMatrix::new(mirrored).unwrap())
        .unwrap();
    assert_eq!(
        capture(&gpu, &mut renderer, &scene, &camera, 1.0),
        reference
    );
    scene.set_model(instance, ModelMatrix::IDENTITY).unwrap();
    for (light, gain) in [
        (
            Light::Point {
                position: [0.0, 0.0, -2.0],
                intensity: [4.0 * std::f32::consts::PI; 3],
            },
            1.0,
        ),
        (
            Light::Point {
                position: [0.0, 0.0, -4.0],
                intensity: [4.0 * std::f32::consts::PI; 3],
            },
            0.25,
        ),
        (
            Light::Spot {
                position: [0.0, 0.0, -2.0],
                direction: [0.0, 0.0, 1.0],
                intensity: [4.0 * std::f32::consts::PI; 3],
                inner_angle: 0.2,
                outer_angle: 0.5,
            },
            1.0,
        ),
        (
            Light::Spot {
                position: [0.0, 0.0, -2.0],
                direction: [0.0, 0.0, -1.0],
                intensity: [4.0 * std::f32::consts::PI; 3],
                inner_angle: 0.2,
                outer_angle: 0.5,
            },
            0.0,
        ),
    ] {
        scene
            .set_lighting(Lighting::new(vec![light], None).unwrap())
            .unwrap();
        let pixel = capture(&gpu, &mut renderer, &scene, &camera, 1.0)[center];
        assert!(pixel.abs_diff(encoded(gain * (0.96 * linear + 0.01))) <= 3);
    }

    scene.set_lighting(original.lighting().clone()).unwrap();
    let brighter = capture(&gpu, &mut renderer, &scene, &camera, 2.0);
    assert!(brighter[center].abs_diff(encoded(2.0 * (0.96 * linear + 0.01))) <= 3);
    maps.occlusion = Some(texture([0, 0, 0, 255]));
    scene.set_material_maps(material, maps.clone()).unwrap();
    assert_eq!(
        capture(&gpu, &mut renderer, &scene, &camera, 1.0),
        reference,
        "AO must not darken punctual light"
    );
    maps.normal = Some(texture([255, 128, 128, 255]));
    scene.set_material_maps(material, maps.clone()).unwrap();
    assert!(capture(&gpu, &mut renderer, &scene, &camera, 1.0)[center] < reference[center] / 2);
    maps.normal = None;
    maps.metallic_roughness = Some(texture([255, 255, 255, 255]));
    factors.metallic = 1.0;
    scene.set_material_maps(material, maps.clone()).unwrap();
    scene.set_material(material, factors).unwrap();
    assert!(capture(&gpu, &mut renderer, &scene, &camera, 1.0)[center] < reference[center]);
    scene.set_lighting(Lighting::default()).unwrap();
    assert_eq!(
        capture(&gpu, &mut renderer, &scene, &camera, 1.0)[center],
        0
    );
    factors.emissive = [2.0, 0.0, 0.0];
    scene.set_material(material, factors).unwrap();
    assert!(capture(&gpu, &mut renderer, &scene, &camera, 1.0)[center].abs_diff(encoded(2.0)) <= 1);
    factors.alpha_cutoff = Some(0.5);
    scene.set_material(material, factors).unwrap();
    maps.base_color = Some(texture([255, 255, 255, 0]));
    scene.set_material_maps(material, maps).unwrap();
    let masked = capture(&gpu, &mut renderer, &scene, &camera, 1.0);
    scene.set_visible(instance, false).unwrap();
    assert_eq!(masked, capture(&gpu, &mut renderer, &scene, &camera, 1.0));
    renderer
        .prepare_pbr(
            &gpu,
            &original,
            &camera,
            PbrSettings {
                exposure: 1.0,
                tone_map: ToneMap::Reinhard,
                ..Default::default()
            },
        )
        .unwrap();
    let mut restored = Vec::new();
    OffscreenTarget::new(&gpu, extent)
        .unwrap()
        .render_rgba(
            &gpu,
            |encoder, view| renderer.encode(encoder, view),
            &mut restored,
        )
        .unwrap();
    assert_eq!(restored, reference);
}

#[test]
#[ignore = "requires a hardware GPU"]
fn colour_mipmaps_filter_in_linear_light() {
    use chr_renderer::scene::{Filter, TextureSampler};
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let extent = ImageExtent::new(32, 32).unwrap();
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
    let image = TextureData::new(
        ImageExtent::new(2, 1).unwrap(),
        vec![255, 0, 0, 255, 0, 255, 0, 255],
    )
    .unwrap()
    .with_sampler(TextureSampler {
        mip: Some(Filter::Linear),
        ..Default::default()
    });
    let material = scene
        .add_material_with_maps(
            PbrMaterial {
                unlit: true,
                ..Default::default()
            },
            MaterialMaps {
                base_color: Some(Arc::new(image)),
                ..Default::default()
            },
        )
        .unwrap();
    let mesh = scene
        .add_mesh(
            MeshData::new(
                vec![
                    [-2.0, -2.0, 0.0],
                    [2.0, -2.0, 0.0],
                    [2.0, 2.0, 0.0],
                    [-2.0, 2.0, 0.0],
                ],
                vec![0, 2, 1, 0, 3, 2],
                std::iter::once(0..6).collect(),
            )
            .unwrap()
            .with_attributes(MeshAttributes {
                tex_coords: vec![[0.0, 0.0], [128.0, 0.0], [128.0, 128.0], [0.0, 128.0]],
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
    scene
        .add_instance(mesh, vec![material], ModelMatrix::IDENTITY)
        .unwrap();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let pixels = capture(&gpu, &mut renderer, &scene, &camera, 1.0);
    let center = (16 * 32 + 16) * 4;
    assert!(pixels[center].abs_diff(encoded(0.5)) <= 2);
    assert!(pixels[center + 1].abs_diff(encoded(0.5)) <= 2);
    assert_eq!(pixels[center + 2], 0);
}
