use std::{
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
};

use chr_renderer::{Gpu, coordinates::ImageExtent, render_triangle_rgba};

use crate::Result;

fn output_directory(working_directory: &Path) -> PathBuf {
    let root = working_directory
        .ancestors()
        .find(|path| path.join("flake.nix").is_file() && path.join("Cargo.toml").is_file())
        .unwrap_or(working_directory);
    root.join("output/renderer")
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
        assert_eq!(output_directory(&root), root.join("output/renderer"));
        let package = root.join("server/viewer");
        fs::create_dir_all(&package).expect("package directory");
        fs::write(root.join("flake.nix"), "").expect("flake marker");
        fs::write(root.join("Cargo.toml"), "").expect("workspace marker");
        let expected = root.join("output/renderer");
        assert_eq!(output_directory(&root), expected);
        assert_eq!(output_directory(&root.join("server")), expected);
        assert_eq!(output_directory(&package), expected);
        fs::remove_dir_all(&root).expect("remove temporary directory");
    }
}
