use std::{
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
};

use chr_core::{Gpu, coordinates::ImageExtent, render_triangle_rgba};

use crate::Result;

fn output_directory(working_directory: &Path) -> PathBuf {
    let root = working_directory
        .ancestors()
        .find(|path| path.join("flake.nix").is_file() && path.join("render/Cargo.toml").is_file())
        .unwrap_or(working_directory);
    root.join("output/render")
}

pub fn write_triangle_png(gpu: &Gpu) -> Result<()> {
    let extent = ImageExtent::new(960, 640)?;
    let rgba = render_triangle_rgba(gpu, extent)?;
    let directory = output_directory(&std::env::current_dir()?);
    let path = directory.join("triangle.png");
    fs::create_dir_all(&directory)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_location_inside_and_outside_a_checkout() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "chr-output-test-{}-{timestamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("temporary directory");
        assert_eq!(output_directory(&root), root.join("output/render"));
        let package = root.join("render/native/chr-app");
        fs::create_dir_all(&package).expect("package directory");
        fs::write(root.join("flake.nix"), "").expect("flake marker");
        fs::write(root.join("render/Cargo.toml"), "").expect("workspace marker");
        let expected = root.join("output/render");
        assert_eq!(output_directory(&root), expected);
        assert_eq!(output_directory(&root.join("render")), expected);
        assert_eq!(output_directory(&package), expected);
        fs::remove_dir_all(&root).expect("remove temporary directory");
    }
}
