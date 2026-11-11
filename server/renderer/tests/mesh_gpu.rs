use chr_renderer::{
    Gpu, MeshRenderer, OffscreenTarget,
    camera::Camera,
    coordinates::ImageExtent,
    scene::{MeshData, ModelMatrix, PbrMaterial, Scene},
};

#[test]
#[ignore = "requires a hardware GPU"]
fn meshes_share_depth_and_survive_visibility_resize_and_old_snapshots() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    let info = gpu.adapter.get_info();
    eprintln!("mesh GPU: {} ({:?})", info.name, info.device_type);
    assert_ne!(info.device_type, wgpu::DeviceType::Cpu);
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
                vec![0, 1, 2, 0, 2, 3],
                std::iter::once(0..6).collect(),
            )
            .unwrap(),
        )
        .unwrap();
    let green = scene
        .add_material(PbrMaterial {
            base_color: [0.0, 1.0, 0.0, 1.0],
            ..Default::default()
        })
        .unwrap();
    let red = scene
        .add_material(PbrMaterial {
            base_color: [1.0, 0.0, 0.0, 1.0],
            ..Default::default()
        })
        .unwrap();
    let mut near = ModelMatrix::IDENTITY.rows();
    near[2][3] = -2.0;
    near[0][0] = -1.0;
    let front = scene
        .add_instance(mesh, vec![green], ModelMatrix::new(near).unwrap())
        .unwrap();
    let rear = scene
        .add_instance(mesh, vec![red], ModelMatrix::IDENTITY)
        .unwrap();
    let old = scene.snapshot();
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    for (w, h) in [(96, 64), (64, 96)] {
        let extent = ImageExtent::new(w, h).unwrap();
        let camera = Camera::look_at(
            extent,
            [0.0, 0.0, -10.0],
            [0.0; 3],
            [0.0, -1.0, 0.0],
            1.0,
            0.1,
            100.0,
        )
        .unwrap();
        let target = OffscreenTarget::new(&gpu, extent).unwrap();
        for (view, expected) in [
            (old.clone(), [0, 255, 0, 255]),
            {
                scene.set_visible(front, false).unwrap();
                (scene.snapshot(), [255, 0, 0, 255])
            },
            (old.clone(), [0, 255, 0, 255]),
        ] {
            renderer.prepare(&gpu, &view, &camera).unwrap();
            let mut pixels = Vec::new();
            target
                .render_rgba(
                    &gpu,
                    |encoder, view| renderer.encode(encoder, view),
                    &mut pixels,
                )
                .unwrap();
            let offset = ((h / 2 * w + w / 2) * 4) as usize;
            assert_eq!(pixels[offset..offset + 4], expected);
        }
    }
    scene.remove_instance(front).unwrap();
    scene.remove_instance(rear).unwrap();
    scene.remove_mesh(mesh).unwrap();
    let replacement = scene
        .add_mesh(
            MeshData::new(
                vec![[50.0, 0.0, 0.0], [52.0, 0.0, 0.0], [50.0, 2.0, 0.0]],
                vec![0, 1, 2],
                std::iter::once(0..3).collect(),
            )
            .unwrap(),
        )
        .unwrap();
    scene
        .add_instance(replacement, vec![green], ModelMatrix::IDENTITY)
        .unwrap();
    let extent = ImageExtent::new(64, 64).unwrap();
    let target = OffscreenTarget::new(&gpu, extent).unwrap();
    let camera = Camera::look_at(
        extent,
        [0.0, 0.0, -10.0],
        [0.0; 3],
        [0.0, -1.0, 0.0],
        1.0,
        0.1,
        100.0,
    )
    .unwrap();
    for (view, green_center) in [(scene.snapshot(), false), (old, true)] {
        renderer.prepare(&gpu, &view, &camera).unwrap();
        let mut pixels = Vec::new();
        target
            .render_rgba(
                &gpu,
                |encoder, view| renderer.encode(encoder, view),
                &mut pixels,
            )
            .unwrap();
        assert_eq!(pixels[(32 * 64 + 32) * 4 + 1] == 255, green_center);
    }
}
