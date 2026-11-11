//! Synthetic renderer-only composition, not a live transport or SDI acceptance test.
use chr_renderer::{
    Gpu, OffscreenTarget, ToneMap,
    camera::Camera,
    composition::{
        AlphaMode, CompositionOutput, FrameCompositor, ImageLayer, ImageStage, ImageView,
        PixelRect, Transfer,
    },
    coordinates::ImageExtent,
    scene::{Light, Lighting, MeshData, ModelMatrix, PbrMaterial, Scene},
};
use std::{error::Error, fs, io::BufWriter, path::Path};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

fn upload(gpu: &Gpu, extent: ImageExtent, pixels: &[u8]) -> Result<ImageView> {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("synthetic display image"),
        size: wgpu::Extent3d {
            width: extent.width(),
            height: extent.height(),
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
            bytes_per_row: Some(extent.width() * 4),
            rows_per_image: Some(extent.height()),
        },
        texture.size(),
    );
    ImageView::new(&texture, Transfer::Srgb, AlphaMode::Straight)
}

fn write_png(path: &Path, extent: ImageExtent, pixels: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), extent.width(), extent.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(pixels)?;
    writer.finish()?;
    Ok(())
}

fn main() -> Result<()> {
    let directory = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "output/renderer/composition".into());
    let directory = Path::new(&directory);
    fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    let gpu =
        pollster::block_on(Gpu::request(&Gpu::instance(), None)).map_err(|e| e.to_string())?;
    if gpu.adapter.get_info().device_type == wgpu::DeviceType::Cpu {
        return Err("composition milestone requires hardware".into());
    }
    let native = ImageExtent::new(640, 360)?;
    let output = ImageExtent::new(960, 640)?;
    let camera = Camera::look_at(
        native,
        [4.0, 6.0, -4.0],
        [0.0, 0.0, -1.0],
        [0.0, 0.0, -1.0],
        0.8,
        0.1,
        100.0,
    )?;
    let mut scene = Scene::new()?;
    let mesh = scene.add_mesh(MeshData::new(
        vec![
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
            [-1.0, -1.0, -2.0],
            [1.0, -1.0, -2.0],
            [1.0, 1.0, -2.0],
            [-1.0, 1.0, -2.0],
        ],
        vec![
            0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1, 1, 5, 6, 1, 6, 2, 2, 6, 7, 2, 7,
            3, 3, 7, 4, 3, 4, 0,
        ],
        std::iter::once(0..36).collect(),
    )?)?;
    let material = scene.add_material(PbrMaterial {
        base_color: [0.8, 0.2, 0.05, 1.0],
        roughness: 0.3,
        metallic: 0.2,
        double_sided: true,
        ..Default::default()
    })?;
    scene.add_instance(mesh, vec![material], ModelMatrix::IDENTITY)?;
    scene.set_lighting(Lighting::new(
        vec![Light::Directional {
            direction: [0.3, -0.5, 1.0],
            irradiance: [3.0; 3],
            angular_radius: 0.0,
            shadow_mode: None,
        }],
        None,
    )?)?;
    let mut plate = Vec::new();
    for y in 0..native.height() {
        for x in 0..native.width() {
            let check = if (x / 40 + y / 40).is_multiple_of(2) {
                100
            } else {
                140
            };
            plate.extend_from_slice(&[check, check, check, 255]);
        }
    }
    let plate = upload(&gpu, native, &plate)?;
    let overlay = upload(&gpu, ImageExtent::new(1, 1)?, &[0, 180, 255, 160])?;
    let layers = [
        ImageLayer::new(&plate, ImageStage::DisplayBackground),
        ImageLayer {
            image: &overlay,
            stage: ImageStage::DisplayForeground,
            rect: PixelRect {
                x: 24.0,
                y: 16.0,
                width: 220.0,
                height: 28.0,
            },
            opacity: 1.0,
        },
    ];
    let target = OffscreenTarget::new(&gpu, output)?;
    let mut reference: Option<Vec<u8>> = None;
    for exposure in [1.0, 4.0] {
        let mut config = CompositionOutput::new(output);
        config.pbr.exposure = exposure;
        config.pbr.tone_map = ToneMap::Reinhard;
        let mut renderer = FrameCompositor::new(&gpu, OffscreenTarget::FORMAT, config)?;
        renderer.set_sample_count(&gpu, 4)?;
        renderer.prepare(&gpu, &scene.snapshot(), &camera, &layers)?;
        let mut pixels = Vec::new();
        target.render_rgba(
            &gpu,
            |encoder, view| {
                renderer
                    .encode(encoder, view.texture())
                    .expect("validated composition target")
            },
            &mut pixels,
        )?;
        if let Some(previous) = &reference {
            for (x, y) in [(10, 320), (100, 24)] {
                let index = (y * output.width() as usize + x) * 4;
                if pixels[index..index + 4] != previous[index..index + 4] {
                    return Err("display plate/overlay changed with scene exposure".into());
                }
            }
            if pixels == *previous {
                return Err("scene exposure did not affect virtual geometry".into());
            }
        }
        write_png(
            &directory.join(format!("composition-exposure-{exposure:.0}.png")),
            output,
            &pixels,
        )?;
        reference = Some(pixels);
    }
    eprintln!(
        "Composition smoke passed: fitted native camera, 4x MSAA, invariant display plate/overlay, changed CG exposure."
    );
    Ok(())
}
