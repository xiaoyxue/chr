use chr_renderer::{Gpu, OffscreenTarget, UyvyConverter, coordinates::ImageExtent};

fn clear(gpu: &Gpu, target: &OffscreenTarget, color: wgpu::Color) {
    target
        .render(gpu, |encoder, view| {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        })
        .unwrap();
}

#[test]
#[ignore = "requires a hardware GPU; run explicitly on Windows and Linux"]
fn uyvy_uses_bt709_limited_range() {
    let instance = Gpu::instance();
    let gpu = pollster::block_on(Gpu::request(&instance, None)).expect("GPU initialization");
    let (width, height) = (6, 4);
    let target = OffscreenTarget::new(&gpu, ImageExtent::new(width, height).unwrap()).unwrap();
    let converter = UyvyConverter::new(&gpu, &target).unwrap();
    let mut uyvy = Vec::new();
    // Expected (Cb, Y, Cr) per BT.709 8-bit limited range. Pure primaries and black/white
    // are unaffected by sRGB encoding, so the clear color equals the stored bytes.
    for (color, [cb, y, cr]) in [
        (wgpu::Color::WHITE, [128, 235, 128]),
        (wgpu::Color::BLACK, [128, 16, 128]),
        (wgpu::Color::RED, [102, 63, 240]),
        (wgpu::Color::GREEN, [42, 173, 26]),
        (wgpu::Color::BLUE, [240, 32, 118]),
    ] {
        clear(&gpu, &target, color);
        converter.convert(&gpu, &mut uyvy).unwrap();
        assert_eq!(uyvy.len(), (width * height * 2) as usize);
        let (groups, rest) = uyvy.as_chunks::<4>();
        assert!(rest.is_empty());
        for group in groups {
            assert_eq!(*group, [cb, y, cr, y], "{color:?}");
        }
    }
    assert!(
        UyvyConverter::new(
            &gpu,
            &OffscreenTarget::new(&gpu, ImageExtent::new(5, 4).unwrap()).unwrap()
        )
        .is_err()
    );
}

#[test]
#[ignore = "requires a hardware GPU; run explicitly on Windows and Linux"]
fn cpu_clean_feed_conversion_matches_the_gpu_converter() {
    use chr_renderer::{ChannelOrder, uyvy_from_bt709};
    let instance = Gpu::instance();
    let gpu = pollster::block_on(Gpu::request(&instance, None)).expect("GPU initialization");
    let (width, height) = (512, 256);
    // Every byte value in every channel, plus pseudo-random pairs for chroma means.
    let mut state = 0x2545_f491_u32;
    let rgba: Vec<u8> = (0..width * height * 4)
        .map(|index| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if index < 256 * 4 {
                (index / 4) as u8
            } else {
                (state >> 24) as u8
            }
        })
        .collect();
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("clean feed source"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        texture.size(),
    );
    let converter = UyvyConverter::from_bt709_texture(&gpu, &texture).unwrap();
    let mut gpu_uyvy = Vec::new();
    converter.convert(&gpu, &mut gpu_uyvy).unwrap();
    let mut cpu_uyvy = Vec::new();
    let extent = ImageExtent::new(width, height).unwrap();
    uyvy_from_bt709(&rgba, extent, ChannelOrder::Rgba, &mut cpu_uyvy).unwrap();
    let differing = gpu_uyvy
        .iter()
        .zip(&cpu_uyvy)
        .filter(|(a, b)| a != b)
        .count();
    let worst = gpu_uyvy
        .iter()
        .zip(&cpu_uyvy)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    println!(
        "CPU/GPU UYVY: {differing} of {} bytes differ, max {worst}",
        cpu_uyvy.len()
    );
    assert_eq!(cpu_uyvy.len(), gpu_uyvy.len());
    assert_eq!(differing, 0, "max difference {worst}");
}
