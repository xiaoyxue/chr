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
