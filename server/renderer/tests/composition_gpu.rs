use chr_renderer::{
    Gpu, PbrSettings, ToneMap,
    camera::{Camera, CameraView, OrthographicCamera},
    composition::{
        AlphaMode, CompositionOutput, FrameCompositor, ImageFit, ImageLayer, ImageStage, ImageView,
        PixelRect, Transfer,
    },
    coordinates::{ImageExtent, Ti},
    scene::{MeshData, ModelMatrix, PbrMaterial, Scene},
};
use std::{sync::mpsc, time::Duration};

fn camera(extent: ImageExtent) -> Camera {
    Camera::look_at(
        extent,
        [0.0, 0.0, -5.0],
        [0.0; 3],
        [0.0, -1.0, 0.0],
        1.0,
        0.1,
        100.0,
    )
    .unwrap()
}

fn image(
    gpu: &Gpu,
    extent: ImageExtent,
    format: wgpu::TextureFormat,
    bytes: &[u8],
    transfer: Transfer,
    alpha: AlphaMode,
) -> ImageView {
    let bytes_per_pixel = if format == wgpu::TextureFormat::Rgba32Float {
        16
    } else {
        4
    };
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("synthetic image"),
        size: wgpu::Extent3d {
            width: extent.width(),
            height: extent.height(),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        texture.as_image_copy(),
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(extent.width() * bytes_per_pixel),
            rows_per_image: Some(extent.height()),
        },
        texture.size(),
    );
    ImageView::new(&texture, transfer, alpha).unwrap()
}

fn render(
    gpu: &Gpu,
    composer: &mut FrameCompositor,
    scene: &Scene,
    camera: &dyn CameraView,
    layers: &[ImageLayer<'_>],
    extent: ImageExtent,
    format: wgpu::TextureFormat,
) -> Vec<u8> {
    composer
        .prepare(gpu, &scene.snapshot(), camera, layers)
        .unwrap();
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("composition result"),
        size: wgpu::Extent3d {
            width: extent.width(),
            height: extent.height(),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let row = (extent.width() * 4).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("composition readback"),
        size: u64::from(row) * u64::from(extent.height()),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    composer.encode(&mut encoder, &texture).unwrap();
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(extent.height()),
            },
        },
        texture.size(),
    );
    let submission = gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(Duration::from_secs(30)),
        })
        .unwrap();
    rx.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
    let mut result = Vec::new();
    {
        let bytes = buffer.slice(..).get_mapped_range().unwrap();
        for source in bytes.chunks_exact(row as usize) {
            result.extend_from_slice(&source[..extent.width() as usize * 4]);
        }
    }
    buffer.unmap();
    result
}

fn pixel(bytes: &[u8], extent: ImageExtent, x: u32, y: u32) -> [u8; 4] {
    let index = (y as usize * extent.width() as usize + x as usize) * 4;
    bytes[index..index + 4].try_into().unwrap()
}

fn encoded(value: f32) -> u8 {
    let value = if value <= 0.0031308 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn linear_byte(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[test]
#[ignore = "requires a hardware GPU"]
fn image_domains_transfer_and_native_fitting_are_explicit() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let native = ImageExtent::new(4, 2).unwrap();
    let output = ImageExtent::new(8, 8).unwrap();
    let camera = camera(native);
    let scene = Scene::new().unwrap();
    let gray = image(
        &gpu,
        native,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &[128_u8, 128, 128, 255].repeat(8),
        Transfer::Srgb,
        AlphaMode::Straight,
    );
    let hdr = image(
        &gpu,
        ImageExtent::new(1, 1).unwrap(),
        wgpu::TextureFormat::Rgba32Float,
        bytemuck::cast_slice(&[[4.0_f32, 0.0, 0.0, 1.0]]),
        Transfer::Linear,
        AlphaMode::Premultiplied,
    );
    let layers = [
        ImageLayer::new(&gray, ImageStage::DisplayBackground),
        ImageLayer {
            image: &hdr,
            stage: ImageStage::SceneBackground,
            rect: PixelRect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0,
            },
            opacity: 1.0,
        },
    ];
    for exposure in [0.25, 2.0] {
        let mut config = CompositionOutput::new(output);
        config.fit = ImageFit::Contain;
        config.pbr = PbrSettings {
            exposure,
            tone_map: ToneMap::Reinhard,
            ..Default::default()
        };
        let mut composer =
            FrameCompositor::new(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb, config).unwrap();
        let pixels = render(
            &gpu,
            &mut composer,
            &scene,
            &camera,
            &layers,
            output,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        assert_eq!(
            pixel(&pixels, output, 0, 0),
            [0, 0, 0, 255],
            "contain fitting leaves explicit letterbox"
        );
        assert_eq!(
            pixel(&pixels, output, 6, 3),
            [128, 128, 128, 255],
            "display plate must bypass scene exposure/tone-map"
        );
        let expected = encoded(4.0 * exposure / (1.0 + 4.0 * exposure));
        assert!(
            pixel(&pixels, output, 1, 3)[0].abs_diff(expected) <= 1,
            "scene-linear layers share the one output transform"
        );
    }
    let view = image(
        &gpu,
        native,
        wgpu::TextureFormat::Rgba8Unorm,
        &[128_u8, 64, 192, 255].repeat(8),
        Transfer::Bt709,
        AlphaMode::Straight,
    );
    let mut config = CompositionOutput::new(native);
    config.transfer = Transfer::Bt709;
    let mut composer = FrameCompositor::new(&gpu, wgpu::TextureFormat::Rgba8Unorm, config).unwrap();
    let pixels = render(
        &gpu,
        &mut composer,
        &scene,
        &camera,
        &[ImageLayer::new(&view, ImageStage::DisplayBackground)],
        native,
        wgpu::TextureFormat::Rgba8Unorm,
    );
    assert_eq!(
        pixel(&pixels, native, 1, 1),
        [128, 64, 192, 255],
        "BT.709 display data must round-trip without sRGB reinterpretation"
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn alpha_is_linear_premultiplied_before_filtering_and_encoding() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let extent = ImageExtent::new(4, 2).unwrap();
    let camera = camera(extent);
    let scene = Scene::new().unwrap();
    let source = image(
        &gpu,
        ImageExtent::new(2, 1).unwrap(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &[255, 0, 0, 0, 0, 0, 255, 255],
        Transfer::Srgb,
        AlphaMode::Straight,
    );
    let layer = ImageLayer {
        image: &source,
        stage: ImageStage::DisplayForeground,
        rect: PixelRect::full(extent),
        opacity: 1.0,
    };
    let mut config = CompositionOutput::new(extent);
    config.clear_color = [0.0; 4];
    let mut composer =
        FrameCompositor::new(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb, config).unwrap();
    let pixels = render(
        &gpu,
        &mut composer,
        &scene,
        &camera,
        &[layer],
        extent,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    );
    let edge = pixel(&pixels, extent, 1, 0);
    assert_eq!(
        edge[0], 0,
        "transparent red texels must not bleed into the filtered blue edge"
    );
    assert_eq!(edge[2], 255);
    assert!(edge[3].abs_diff(64) <= 1);
    config.alpha = AlphaMode::Premultiplied;
    let mut composer =
        FrameCompositor::new(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb, config).unwrap();
    let pixels = render(
        &gpu,
        &mut composer,
        &scene,
        &camera,
        &[layer],
        extent,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    );
    assert!(
        pixel(&pixels, extent, 1, 0)[2].abs_diff(encoded(0.25)) <= 1,
        "premultiplication occurs in linear light, not encoded RGB"
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn image_replacement_and_native_resize_do_not_reuse_stale_layers() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let native = ImageExtent::new(4, 2).unwrap();
    let output = ImageExtent::new(8, 8).unwrap();
    let scene = Scene::new().unwrap();
    let mut composer = FrameCompositor::new(
        &gpu,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        CompositionOutput::new(output),
    )
    .unwrap();
    for color in [[255_u8, 0, 0, 255], [0, 0, 255, 255]] {
        let view = image(
            &gpu,
            native,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            &color.repeat(8),
            Transfer::Srgb,
            AlphaMode::Straight,
        );
        let result = render(
            &gpu,
            &mut composer,
            &scene,
            &camera(native),
            &[ImageLayer::new(&view, ImageStage::DisplayBackground)],
            output,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        assert_eq!(pixel(&result, output, 3, 3), color);
    }
    let changed = ImageExtent::new(2, 4).unwrap();
    let view = image(
        &gpu,
        changed,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &[0_u8, 0, 255, 255].repeat(8),
        Transfer::Srgb,
        AlphaMode::Straight,
    );
    let result = render(
        &gpu,
        &mut composer,
        &scene,
        &camera(changed),
        &[ImageLayer::new(&view, ImageStage::DisplayBackground)],
        output,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    );
    assert_eq!(pixel(&result, output, 0, 3), [0, 0, 0, 255]);
    assert_eq!(pixel(&result, output, 3, 0), [0, 0, 255, 255]);
    let mut limited = CompositionOutput::new(ImageExtent::new(4, 4).unwrap());
    limited.max_intermediate_bytes = 512;
    let mut composer =
        FrameCompositor::new(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb, limited).unwrap();
    assert!(
        composer
            .prepare(
                &gpu,
                &scene.snapshot(),
                &camera(ImageExtent::new(16, 16).unwrap()),
                &[]
            )
            .is_err(),
        "the budget must include native and output canvases"
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn virtual_depth_msaa_and_target_validation_survive_composition() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let extent = ImageExtent::new(64, 64).unwrap();
    let camera = camera(extent);
    let mut scene = Scene::new().unwrap();
    let mesh = scene
        .add_mesh(
            MeshData::new(
                vec![
                    [-1.0, -1.0, 0.0],
                    [1.0, -1.0, 0.0],
                    [1.0, 1.0, 0.0],
                    [-1.0, 1.0, 0.0],
                ],
                vec![0, 2, 1, 0, 3, 2],
                std::iter::once(0..6).collect(),
            )
            .unwrap(),
        )
        .unwrap();
    let red = scene
        .add_material(PbrMaterial {
            base_color: [1.0, 0.0, 0.0, 1.0],
            unlit: true,
            ..Default::default()
        })
        .unwrap();
    let green = scene
        .add_material(PbrMaterial {
            base_color: [0.0, 1.0, 0.0, 1.0],
            unlit: true,
            ..Default::default()
        })
        .unwrap();
    let mut model = ModelMatrix::IDENTITY.rows();
    model[2][3] = -1.0;
    scene
        .add_instance(mesh, vec![red], ModelMatrix::new(model).unwrap())
        .unwrap();
    scene
        .add_instance(mesh, vec![green], ModelMatrix::IDENTITY)
        .unwrap();
    let background = image(
        &gpu,
        extent,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &[0_u8, 0, 128, 255].repeat(64 * 64),
        Transfer::Srgb,
        AlphaMode::Straight,
    );
    let mut config = CompositionOutput::new(extent);
    config.pbr.tone_map = ToneMap::Reinhard;
    let mut composer =
        FrameCompositor::new(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb, config).unwrap();
    let layers = [ImageLayer::new(&background, ImageStage::DisplayBackground)];
    let orthographic = OrthographicCamera::from_pose(
        extent,
        [5.0, 5.0],
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, -5.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
        0.1,
        100.0,
    )
    .unwrap();
    for view in [&camera as &dyn CameraView, &orthographic as &dyn CameraView] {
        for count in [1, 4, 1] {
            composer.set_sample_count(&gpu, count).unwrap();
            let pixels = render(
                &gpu,
                &mut composer,
                &scene,
                view,
                &layers,
                extent,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            );
            assert_eq!(
                pixel(&pixels, extent, 32, 32),
                [encoded(0.5), 0, 0, 255],
                "closer virtual geometry must occlude the farther instance"
            );
            assert_eq!(pixel(&pixels, extent, 0, 0), [0, 0, 128, 255]);
            if count == 4 {
                let mut edges = 0;
                for p in pixels.as_chunks::<4>().0 {
                    if p[0] > 0 && p[0] < encoded(0.5) && p[2] > 0 && p[2] < 128 {
                        let red_coverage = linear_byte(p[0]) / 0.5;
                        let blue_coverage = 1.0 - linear_byte(p[2]) / linear_byte(128);
                        assert!(
                            (red_coverage - blue_coverage).abs() < 0.035,
                            "MSAA HDR must be unpremultiplied for tone mapping and premultiplied again for composition"
                        );
                        edges += 1;
                    }
                }
                assert!(edges > 0, "fixture must exercise partial MSAA coverage");
            }
        }
    }
    let foreground = image(
        &gpu,
        ImageExtent::new(1, 1).unwrap(),
        wgpu::TextureFormat::Rgba8Unorm,
        &[0, 255, 0, 128],
        Transfer::Linear,
        AlphaMode::Straight,
    );
    let mut center_colors = Vec::new();
    for stage in [ImageStage::SceneForeground, ImageStage::DisplayForeground] {
        let foreground = ImageLayer {
            image: &foreground,
            stage,
            rect: PixelRect::full(extent),
            opacity: 1.0,
        };
        let image = render(
            &gpu,
            &mut composer,
            &scene,
            &camera,
            &[layers[0], foreground],
            extent,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        center_colors.push(pixel(&image, extent, 32, 32));
    }
    assert!(center_colors[0][0] > 150 && center_colors[0][1] < 160);
    assert!(
        center_colors[1][0] < 140 && center_colors[1][1] > 180,
        "display foregrounds blend after the scene transform, not before it"
    );
    let invalid = ImageLayer {
        rect: PixelRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 1.0,
        },
        ..layers[0]
    };
    assert!(
        composer
            .prepare(&gpu, &scene.snapshot(), &camera, &[invalid])
            .is_err()
    );
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 8,
            height: 8,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    assert!(
        composer
            .encode(
                &mut gpu.device.create_command_encoder(&Default::default()),
                &target
            )
            .is_err()
    );
    assert!(
        ImageView::new(&target, Transfer::Srgb, AlphaMode::Straight).is_err(),
        "input views must be sampleable"
    );
    config.transfer = Transfer::Bt709;
    assert!(
        FrameCompositor::new(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb, config).is_err(),
        "output format cannot silently reinterpret its transfer"
    );
}

#[test]
#[ignore = "requires a hardware GPU"]
fn calibrated_cg_and_native_plate_share_output_fitting_without_pixel_shift() {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None)).unwrap();
    assert_ne!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    let native = ImageExtent::new(64, 64).unwrap();
    let output = ImageExtent::new(128, 96).unwrap();
    let ti = Ti::new([[0.02, 0.0, 0.0], [0.0, 0.02, 0.0], [0.001, -0.0005, 1.0]]).unwrap();
    let camera = Camera::from_ti(native, ti, [0.0, 0.0, -5.0], 0.1, 100.0).unwrap();
    let positions = [[40.0, 40.0], [41.0, 40.0], [41.0, 41.0], [40.0, 41.0]]
        .map(|pixel| {
            let ray = camera.ray(pixel).unwrap();
            let t = -ray.origin[2] / ray.direction[2];
            std::array::from_fn(|i| (ray.origin[i] + t * ray.direction[i]) as f32)
        })
        .to_vec();
    let mut scene = Scene::new().unwrap();
    let mesh = scene
        .add_mesh(
            MeshData::new(
                positions,
                vec![0, 2, 1, 0, 3, 2],
                std::iter::once(0..6).collect(),
            )
            .unwrap(),
        )
        .unwrap();
    let material = scene
        .add_material(PbrMaterial {
            unlit: true,
            double_sided: true,
            ..Default::default()
        })
        .unwrap();
    scene
        .add_instance(mesh, vec![material], ModelMatrix::IDENTITY)
        .unwrap();
    let mut data = vec![0_u8; 64 * 64 * 4];
    for pixel in data.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    data[(40 * 64 + 40) * 4] = 255;
    let marker = image(
        &gpu,
        native,
        wgpu::TextureFormat::Rgba8Unorm,
        &data,
        Transfer::Linear,
        AlphaMode::Straight,
    );
    let layers = [ImageLayer::new(&marker, ImageStage::DisplayBackground)];
    let mut config = CompositionOutput::new(output);
    config.transfer = Transfer::Linear;
    config.pbr.tone_map = ToneMap::Reinhard;
    let mut composer = FrameCompositor::new(&gpu, wgpu::TextureFormat::Rgba8Unorm, config).unwrap();
    let plate = render(
        &gpu,
        &mut composer,
        &Scene::new().unwrap(),
        &camera,
        &layers,
        output,
        wgpu::TextureFormat::Rgba8Unorm,
    );
    let composed = render(
        &gpu,
        &mut composer,
        &scene,
        &camera,
        &layers,
        output,
        wgpu::TextureFormat::Rgba8Unorm,
    );
    assert!(plate.as_chunks::<4>().0.iter().any(|p| p[0] > 100));
    for (background, cg) in plate
        .as_chunks::<4>()
        .0
        .iter()
        .zip(composed.as_chunks::<4>().0)
    {
        assert!(
            (i16::from(background[0]) - 2 * i16::from(cg[1])).abs() <= 2,
            "native image and Ti-projected CG must align under the same output fit"
        );
    }
}
