use std::{fs, io::BufWriter, path::Path};

use chr_core::{Gpu, coordinates::ImageExtent, render_triangle_rgba};

use crate::Result;

pub fn write_triangle_png(gpu: &Gpu) -> Result<()> {
    let extent = ImageExtent::new(960, 640)?;
    let rgba = render_triangle_rgba(gpu, extent)?;
    let path = Path::new("output").join("triangle.png");
    fs::create_dir_all("output")?;
    let file = BufWriter::new(fs::File::create(&path)?);
    let mut encoder = png::Encoder::new(file, extent.width(), extent.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgba)?;
    writer.finish()?;
    log::info!("saved {}", path.display());
    Ok(())
}
