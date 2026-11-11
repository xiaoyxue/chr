//! Private local diagnostic fixture, not a public control protocol or live preview.
use chr_renderer::{
    Gpu, MeshRenderer, OffscreenTarget, PbrSettings,
    camera::Camera,
    coordinates::{ImageExtent, Ti},
    glb::import_glb,
    scene::{Light, Lighting, MeshData, ModelMatrix, PbrMaterial, Scene},
};
use serde::Deserialize;
use std::{error::Error, fs, io::BufWriter, path::Path};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Deserialize)]
struct CameraFixture {
    width: u32,
    height: u32,
    ti: [[f64; 3]; 3],
    center: [f64; 3],
    pitch: [f64; 2],
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
    env_logger::init();
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        return Err("usage: pbr_milestone <private-camera.json> <output-directory> <local.glb> [local.glb...]".into());
    }
    let fixture: CameraFixture = serde_json::from_reader(fs::File::open(&args[0])?)?;
    let extent = ImageExtent::new(fixture.width, fixture.height)?;
    let camera = Camera::from_ti(extent, Ti::new(fixture.ti)?, fixture.center, 0.1, 10000.0)?;
    let gpu =
        pollster::block_on(Gpu::request(&Gpu::instance(), None)).map_err(|e| e.to_string())?;
    let adapter = gpu.adapter.get_info();
    if adapter.device_type == wgpu::DeviceType::Cpu {
        return Err("PBR milestone requires hardware".into());
    }
    eprintln!("PBR adapter: {} ({:?})", adapter.name, adapter.device_type);
    let [length, width] = fixture.pitch;
    let mut scene = Scene::new()?;
    let plane = scene.add_mesh(MeshData::new(
        vec![
            [-0.5, -0.5, 0.0],
            [0.5, -0.5, 0.0],
            [0.5, 0.5, 0.0],
            [-0.5, 0.5, 0.0],
        ],
        vec![0, 2, 1, 0, 3, 2],
        std::iter::once(0..6).collect(),
    )?)?;
    let green = scene.add_material(PbrMaterial {
        base_color: [0.035, 0.19, 0.045, 1.0],
        roughness: 0.9,
        ..Default::default()
    })?;
    let alternate = scene.add_material(PbrMaterial {
        base_color: [0.045, 0.24, 0.06, 1.0],
        roughness: 0.9,
        ..Default::default()
    })?;
    for i in 0..14 {
        let x = -length / 2.0 + (i as f64 + 0.5) * length / 14.0;
        scene.add_instance(
            plane,
            vec![if i % 2 == 0 { green } else { alternate }],
            ModelMatrix::new([
                [length / 14.0, 0.0, 0.0, x],
                [0.0, width, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ])?,
        )?;
    }
    let white = scene.add_material(PbrMaterial {
        base_color: [0.8, 0.85, 0.75, 1.0],
        roughness: 0.85,
        ..Default::default()
    })?;
    let lines = [
        (length, 0.16, 0.0, -width / 2.0),
        (length, 0.16, 0.0, width / 2.0),
        (0.16, width, -length / 2.0, 0.0),
        (0.16, width, length / 2.0, 0.0),
        (0.16, width, 0.0, 0.0),
    ];
    for (sx, sy, x, y) in lines {
        scene.add_instance(
            plane,
            vec![white],
            ModelMatrix::new([
                [sx, 0.0, 0.0, x],
                [0.0, sy, 0.0, y],
                [0.0, 0.0, 1.0, -0.025],
                [0.0, 0.0, 0.0, 1.0],
            ])?,
        )?;
    }
    let models = &args[2..];
    for (i, path) in models.iter().enumerate() {
        let asset = import_glb(&fs::read(path)?)?;
        for warning in &asset.warnings {
            eprintln!("{warning}");
        }
        let bounds = asset.mesh.bounds();
        let lo = bounds.min();
        let hi = bounds.max();
        let height = hi[2] - lo[2];
        if height <= 0.0 {
            return Err("model has no vertical extent".into());
        }
        let u = if models.len() == 1 {
            0.5
        } else {
            0.32 + 0.36 * i as f64 / (models.len() - 1) as f64
        };
        let ray = camera.ray([
            f64::from(extent.width()) * u,
            f64::from(extent.height()) * 0.77,
        ])?;
        let t = -ray.origin[2] / ray.direction[2];
        if !t.is_finite() || t <= 0.0 {
            return Err(
                "selected image position does not intersect the ground in front of the camera"
                    .into(),
            );
        }
        let ground = [
            ray.origin[0] + t * ray.direction[0],
            ray.origin[1] + t * ray.direction[1],
        ];
        let pixel0 = camera.project([ground[0], ground[1], 0.0])?.0;
        let pixel1 = camera.project([ground[0], ground[1], -1.0])?.0;
        let pixels_per_metre = (pixel1[1] - pixel0[1]).abs();
        let desired_height =
            (f64::from(extent.height()) * 0.32 / pixels_per_metre).clamp(1.0, 12.0);
        let scale = desired_height / height;
        let model = ModelMatrix::new([
            [scale, 0.0, 0.0, ground[0] - scale * (lo[0] + hi[0]) / 2.0],
            [0.0, scale, 0.0, ground[1] - scale * (lo[1] + hi[1]) / 2.0],
            [0.0, 0.0, scale, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ])?
        .grounded(bounds)?;
        let mesh = scene.add_mesh(asset.mesh)?;
        let materials = asset
            .materials
            .into_iter()
            .map(|m| scene.add_material_with_maps(m.factors, m.maps))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let instance = scene.add_instance(mesh, materials, model)?;
        if scene.snapshot().instance(instance)?.bounds().max()[2].abs() > 1e-8 {
            return Err("model grounding failed".into());
        }
    }
    let directory = Path::new(&args[1]);
    fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    let target = OffscreenTarget::new(&gpu, extent)?;
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    for mode in ["sun", "points", "spots"] {
        let mut lights = vec![Light::Directional {
            direction: [0.2, -0.4, 1.0],
            irradiance: [2.5, 2.35, 2.1],
        }];
        if mode != "sun" {
            for x in [-length / 2.0, length / 2.0] {
                for y in [-width / 2.0, width / 2.0] {
                    let position = [x, y, -35.0];
                    let intensity = [4000.0, 4200.0, 5000.0];
                    lights.push(if mode == "points" {
                        Light::Point {
                            position,
                            intensity,
                        }
                    } else {
                        Light::Spot {
                            position,
                            direction: [-x, -y, 35.0],
                            intensity,
                            inner_angle: 0.4,
                            outer_angle: 0.9,
                        }
                    });
                }
            }
        }
        scene.set_lighting(Lighting::new(lights, None)?)?;
        renderer.prepare_pbr(&gpu, &scene.snapshot(), &camera, PbrSettings::default())?;
        let mut pixels = Vec::new();
        target.render_rgba(
            &gpu,
            |encoder, view| renderer.encode(encoder, view),
            &mut pixels,
        )?;
        write_png(&directory.join(format!("{mode}.png")), extent, &pixels)?;
        eprintln!("Rendered {mode} diagnostic (no shadows or IBL).");
    }
    Ok(())
}
