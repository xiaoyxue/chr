//! Synthetic M1 camera/mesh/depth image, not a PBR lighting demonstration.
use chr_renderer::{
    Gpu, MeshRenderer, OffscreenTarget,
    camera::Camera,
    coordinates::ImageExtent,
    scene::{MaterialId, MeshData, MeshId, ModelMatrix, PbrMaterial, Scene},
};
use std::{error::Error, fs, io::BufWriter};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

fn material(scene: &mut Scene, color: [f32; 3]) -> Result<MaterialId> {
    Ok(scene.add_material(PbrMaterial {
        base_color: [color[0], color[1], color[2], 1.0],
        ..Default::default()
    })?)
}

fn place(
    scene: &mut Scene,
    mesh: MeshId,
    materials: Vec<MaterialId>,
    size: [f64; 3],
    position: [f64; 3],
    yaw: f64,
) -> Result<()> {
    let (s, c) = yaw.sin_cos();
    let model = ModelMatrix::new([
        [c * size[0], -s * size[1], 0.0, position[0]],
        [s * size[0], c * size[1], 0.0, position[1]],
        [0.0, 0.0, size[2], position[2]],
        [0.0, 0.0, 0.0, 1.0],
    ])?;
    scene.add_instance(mesh, materials, model)?;
    Ok(())
}

fn main() -> Result<()> {
    env_logger::init();
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))
        .map_err(|error| error.to_string())?;
    let info = gpu.adapter.get_info();
    eprintln!("M1 adapter: {} ({:?})", info.name, info.device_type);
    if info.device_type == wgpu::DeviceType::Cpu {
        return Err("M1 requires a hardware GPU".into());
    }
    let mut scene = Scene::new()?;
    let quad = scene.add_mesh(MeshData::new(
        vec![
            [-0.5, -0.5, 0.0],
            [0.5, -0.5, 0.0],
            [0.5, 0.5, 0.0],
            [-0.5, 0.5, 0.0],
        ],
        vec![0, 1, 2, 0, 2, 3],
        std::iter::once(0..6).collect(),
    )?)?;
    let greens = [
        material(&mut scene, [0.035, 0.23, 0.075])?,
        material(&mut scene, [0.045, 0.29, 0.095])?,
    ];
    let white = material(&mut scene, [0.85, 0.9, 0.8])?;
    for i in 0..14 {
        place(
            &mut scene,
            quad,
            vec![greens[i % 2]],
            [7.5, 68.0, 1.0],
            [-52.5 + (i as f64 + 0.5) * 7.5, 0.0, 0.0],
            0.0,
        )?;
    }
    for y in [-34.0, 34.0] {
        place(
            &mut scene,
            quad,
            vec![white],
            [105.0, 0.2, 1.0],
            [0.0, y, -0.03],
            0.0,
        )?;
    }
    for x in [-52.5, 0.0, 52.5] {
        place(
            &mut scene,
            quad,
            vec![white],
            [0.2, 68.0, 1.0],
            [x, 0.0, -0.03],
            0.0,
        )?;
    }
    for sign in [-1.0, 1.0] {
        place(
            &mut scene,
            quad,
            vec![white],
            [0.2, 40.32, 1.0],
            [sign * 36.0, 0.0, -0.03],
            0.0,
        )?;
        for y in [-20.16, 20.16] {
            place(
                &mut scene,
                quad,
                vec![white],
                [16.5, 0.2, 1.0],
                [sign * 44.25, y, -0.03],
                0.0,
            )?;
        }
    }
    let mut ring = Vec::new();
    let mut indices = Vec::new();
    for i in 0..64_u32 {
        for radius in [9.05, 9.25] {
            let a = f64::from(i) * std::f64::consts::TAU / 64.0;
            ring.push([(radius * a.cos()) as f32, (radius * a.sin()) as f32, -0.03]);
        }
        let a = 2 * i;
        let b = 2 * ((i + 1) % 64);
        indices.extend([a, b, a + 1, a + 1, b, b + 1]);
    }
    let count = indices.len() as u32;
    let ring = scene.add_mesh(MeshData::new(
        ring,
        indices,
        std::iter::once(0..count).collect(),
    )?)?;
    scene.add_instance(ring, vec![white], ModelMatrix::IDENTITY)?;
    let cube = scene.add_mesh(MeshData::new(
        vec![
            [-0.5, -0.5, -0.5],
            [0.5, -0.5, -0.5],
            [0.5, 0.5, -0.5],
            [-0.5, 0.5, -0.5],
            [-0.5, -0.5, 0.5],
            [0.5, -0.5, 0.5],
            [0.5, 0.5, 0.5],
            [-0.5, 0.5, 0.5],
        ],
        vec![
            0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1, 1, 5, 6, 1, 6, 2, 2, 6, 7, 2, 7,
            3, 3, 7, 4, 3, 4, 0,
        ],
        (0..6).map(|i| i * 6..i * 6 + 6).collect(),
    )?)?;
    for (x, height, color) in [
        (-28.0, 8.0, [0.7, 0.08, 0.045]),
        (0.0, 14.0, [0.035, 0.24, 0.8]),
        (28.0, 20.0, [0.85, 0.48, 0.04]),
    ] {
        let mut mats = Vec::new();
        for gain in [1.0, 0.45, 0.6, 0.8, 0.7, 0.55] {
            mats.push(material(&mut scene, color.map(|v| v * gain))?);
        }
        place(
            &mut scene,
            cube,
            mats,
            [9.0, 9.0, height],
            [x, 0.0, -height / 2.0],
            0.3,
        )?;
    }
    let extent = ImageExtent::new(1280, 800)?;
    let camera = Camera::look_at(
        extent,
        [90.0, -105.0, -100.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, -1.0],
        0.8,
        0.1,
        1000.0,
    )?;
    let target = OffscreenTarget::new(&gpu, extent)?;
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    renderer.prepare(&gpu, &scene.snapshot(), &camera)?;
    let mut pixels = Vec::new();
    target.render_rgba(
        &gpu,
        |encoder, view| renderer.encode(encoder, view),
        &mut pixels,
    )?;
    let path = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "output/renderer/milestone-m1.png".into()),
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut encoder = png::Encoder::new(
        BufWriter::new(fs::File::create(&path)?),
        extent.width(),
        extent.height(),
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&pixels)?;
    writer.finish()?;
    eprintln!("M1 image: {}", path.display());
    Ok(())
}
