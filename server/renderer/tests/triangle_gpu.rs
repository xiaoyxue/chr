use chr_renderer::{
    Gpu, OffscreenTarget, TriangleRenderer, coordinates::ImageExtent, render_rotated_triangle_rgba,
    render_triangle_rgba,
};

#[test]
#[ignore = "requires a hardware GPU; run explicitly on Windows and Linux"]
fn triangle_pixels_preserve_snc_orientation_and_aspect() {
    let instance = Gpu::instance();
    let gpu = pollster::block_on(Gpu::request(&instance, None)).expect("GPU initialization");
    let info = gpu.adapter.get_info();
    eprintln!(
        "GPU: {} ({:?}, {:?})",
        info.name, info.backend, info.device_type
    );
    assert_ne!(
        info.device_type,
        wgpu::DeviceType::Cpu,
        "software GPU is not acceptance"
    );
    for (width, height) in [(127, 64), (64, 127)] {
        let extent = ImageExtent::new(width, height).unwrap();
        let data = render_triangle_rgba(&gpu, extent).expect("offscreen rendering");
        assert_eq!(data.len(), (width * height * 4) as usize);
        let pixel = |x: u32, y: u32| -> &[u8] {
            let offset = ((y * width + x) * 4) as usize;
            &data[offset..offset + 4]
        };
        assert_eq!(pixel(0, 0), [0, 0, 0, 255]);
        let centre = pixel(width / 2, height / 2);
        assert!(
            centre[..3].iter().all(|&channel| channel > 40),
            "{centre:?}"
        );
        let scale = (f64::from(width) * f64::from(height)).sqrt();
        for (snc, dominant) in [([0.0, -0.35], 0), ([-0.35, 0.4], 1), ([0.35, 0.4], 2)] {
            let x = ((snc[0] * scale + f64::from(width)) / 2.0) as u32;
            let y = ((snc[1] * scale + f64::from(height)) / 2.0) as u32;
            let rgb = pixel(x, y);
            assert!(
                rgb[dominant] > 170,
                "wrong triangle orientation/color: {rgb:?}"
            );
        }
        let mut covered = 0;
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (width, height, 0, 0);
        for y in 0..height {
            for x in 0..width {
                if pixel(x, y)[..3].iter().any(|&v| v > 0) {
                    covered += 1;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        let fraction = f64::from(covered) / f64::from(width * height);
        assert!(
            (0.14..0.16).contains(&fraction),
            "incorrect triangle area: {fraction}"
        );
        assert!(
            (max_x - min_x).abs_diff(max_y - min_y) <= 2,
            "SNC aspect distorted"
        );
    }
    let too_large = ImageExtent::new(gpu.device.limits().max_texture_dimension_2d + 1, 1).unwrap();
    assert!(render_triangle_rgba(&gpu, too_large).is_err());
}

#[test]
#[ignore = "requires a hardware GPU; run explicitly on Windows and Linux"]
fn rotation_turns_clockwise_about_the_image_centre() {
    let instance = Gpu::instance();
    let gpu = pollster::block_on(Gpu::request(&instance, None)).expect("GPU initialization");
    let (width, height) = (128, 96);
    let extent = ImageExtent::new(width, height).unwrap();
    let scale = (f64::from(width) * f64::from(height)).sqrt();
    let rgb_at = |data: &[u8], snc: [f64; 2]| -> [u8; 3] {
        let x = ((snc[0] * scale + f64::from(width)) / 2.0) as u32;
        let y = ((snc[1] * scale + f64::from(height)) / 2.0) as u32;
        let offset = ((y * width + x) * 4) as usize;
        [data[offset], data[offset + 1], data[offset + 2]]
    };

    // A half turn moves the red apex from above the centre to below it.
    let half = render_rotated_triangle_rgba(&gpu, extent, std::f64::consts::PI).unwrap();
    assert!(rgb_at(&half, [0.0, 0.35])[0] > 170, "apex not rotated");
    // Inside the unrotated triangle's base, outside every rotated one checked here.
    let base_corner = [0.45, 0.45];
    assert!(rgb_at(&render_triangle_rgba(&gpu, extent).unwrap(), base_corner)[2] > 100);
    assert_eq!(rgb_at(&half, base_corner), [0, 0, 0]);

    // A quarter turn is clockwise on screen: the apex (up) moves to the right.
    let quarter = render_rotated_triangle_rgba(&gpu, extent, std::f64::consts::FRAC_PI_2).unwrap();
    assert!(
        rgb_at(&quarter, [0.35, 0.0])[0] > 170,
        "rotation is not clockwise"
    );
    assert_eq!(rgb_at(&quarter, base_corner), [0, 0, 0]);

    // A reused target and renderer produce the same pixels as the one-shot helper.
    let target = OffscreenTarget::new(&gpu, extent).unwrap();
    let renderer = TriangleRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    renderer.resize(&gpu.queue, extent);
    let mut pixels = Vec::new();
    for radians in [0.0, std::f64::consts::PI] {
        renderer.set_rotation(&gpu.queue, radians);
        target
            .render_rgba(
                &gpu,
                |encoder, view| renderer.encode(encoder, view),
                &mut pixels,
            )
            .unwrap();
    }
    assert_eq!(pixels, half);
}
