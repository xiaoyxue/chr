//! Material textures seen at grazing angles, as a broadcast camera sees ground ads.
//! World metres, ground Z=0, upwards -Z.

use std::sync::Arc;

use chr_renderer::{
    Gpu, MeshRenderer, OffscreenTarget, PbrSettings,
    camera::Camera,
    coordinates::ImageExtent,
    scene::{
        Filter, MaterialMaps, MeshAttributes, MeshData, ModelMatrix, PbrMaterial, Scene,
        TextureData, TextureSampler, Wrap,
    },
};

const SIZE: u32 = 256;
const TEXELS: u32 = 256;

/// Black and white stripes, 16 texels per period, running across U.
fn stripes() -> TextureData {
    let pixels = (0..TEXELS * TEXELS)
        .flat_map(|i| {
            let v = if (i % TEXELS / 8).is_multiple_of(2) {
                0
            } else {
                255
            };
            [v, v, v, 255]
        })
        .collect();
    TextureData::new(ImageExtent::new(TEXELS, TEXELS).unwrap(), pixels)
        .unwrap()
        .with_sampler(TextureSampler {
            wrap_u: Wrap::Clamp,
            wrap_v: Wrap::Clamp,
            mag: Filter::Linear,
            min: Filter::Linear,
            mip: Some(Filter::Linear),
        })
}

#[test]
#[ignore = "requires a hardware GPU"]
fn grazing_ground_textures_keep_detail_across_the_view() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let mut scene = Scene::new().unwrap();
    // A 16 m ground square; its stripes run across the view, its depth is foreshortened
    // about twentyfold, like a pitch-side ad seen from a high broadcast camera.
    let mesh = MeshData::new(
        vec![
            [-8.0, -8.0, 0.0],
            [8.0, -8.0, 0.0],
            [8.0, 8.0, 0.0],
            [-8.0, 8.0, 0.0],
        ],
        vec![0, 2, 1, 0, 3, 2],
        std::iter::once(0..6).collect(),
    )
    .and_then(|mesh| {
        mesh.with_attributes(MeshAttributes {
            normals: vec![[0.0, 0.0, -1.0]; 4],
            tex_coords: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            ..Default::default()
        })
    })
    .unwrap();
    let mesh = scene.add_mesh(mesh).unwrap();
    let material = scene
        .add_material_with_maps(
            PbrMaterial {
                unlit: true,
                double_sided: true,
                ..Default::default()
            },
            MaterialMaps {
                base_color: Some(Arc::new(stripes())),
                ..Default::default()
            },
        )
        .unwrap();
    scene
        .add_instance(mesh, vec![material], ModelMatrix::IDENTITY)
        .unwrap();
    let camera = Camera::look_at(
        ImageExtent::new(SIZE, SIZE).unwrap(),
        [0.0, 40.0, -2.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, -1.0],
        0.6,
        0.1,
        100.0,
    )
    .unwrap();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    renderer
        .prepare_pbr(&gpu, &scene.snapshot(), &camera, PbrSettings::default())
        .unwrap();
    let mut pixels = Vec::new();
    OffscreenTarget::new(&gpu, camera.extent())
        .unwrap()
        .render_rgba(
            &gpu,
            |encoder, view| renderer.encode(encoder, view),
            &mut pixels,
        )
        .unwrap();
    // Each stripe spans about five pixels across the view, so it stays resolvable;
    // an isotropic footprint would pick the depth axis's mip and average it to grey.
    let column = |x: f64| camera.project([x, 0.0, 0.0]).unwrap().0;
    let [left, right] = [column(-6.0), column(6.0)];
    let row = left[1].round() as usize;
    let codes: Vec<u8> = (left[0].round() as usize..right[0].round() as usize)
        .map(|x| pixels[4 * (row * SIZE as usize + x)])
        .collect();
    let contrast = codes.iter().max().unwrap() - codes.iter().min().unwrap();
    assert!(
        contrast > 120,
        "stripes blurred to contrast {contrast}: {codes:?}"
    );
}
