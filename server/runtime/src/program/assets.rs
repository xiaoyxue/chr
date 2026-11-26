//! Reads and imports the assets of a render description: a GLB, an image quad, flag
//! artwork or an environment map, from a file or a ZIP package entry, verified by
//! SHA-256.

use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use chr_common::description::{Asset, AssetKind, AssetSource, BlendAs};
use chr_renderer::{
    coordinates::ImageExtent,
    exr::import_exr,
    glb::{GlbAsset, ImportOptions, ImportedMaterial, import_glb_with},
    scene::{
        EnvironmentMap, Filter, MaterialMaps, MeshAttributes, MeshData, PbrMaterial, TextureData,
        TextureSampler, Wrap,
    },
};
use sha2::{Digest, Sha256};

use super::Error;

/// Largest asset the server reads, file or package entry.
pub const MAX_ASSET_BYTES: u64 = 256 * 1024 * 1024;

/// Where asset paths may point.
pub enum Paths<'a> {
    /// A trusted startup file: relative paths resolve against its directory.
    Startup { base: &'a Path },
    /// A runtime request: absolute paths inside one of these canonical roots only.
    Roots(&'a [PathBuf]),
}

/// An imported asset: drawable geometry, or an environment that lighting refers to.
pub enum Imported {
    Model(GlbAsset),
    Environment(EnvironmentMap),
}

/// Reads, verifies and imports `asset`.
pub fn load(asset: &Asset, paths: &Paths) -> Result<Imported, Error> {
    import(asset, &read_verified(asset, paths)?)
}

/// The source bytes of `asset`, after checking their SHA-256.
pub fn read_verified(asset: &Asset, paths: &Paths) -> Result<Vec<u8>, Error> {
    let bytes = read(&asset.source, paths)?;
    let digest = hex(&Sha256::digest(&bytes));
    if !digest.eq_ignore_ascii_case(&asset.sha256) {
        return Err(format!(
            "asset {} has SHA-256 {digest}, the description expects {}",
            asset.id.as_str(),
            asset.sha256
        )
        .into());
    }
    Ok(bytes)
}

/// Imports verified source bytes as `asset.kind` describes them.
pub fn import(asset: &Asset, bytes: &[u8]) -> Result<Imported, Error> {
    // A checkout whose Git LFS files were not fetched holds a small text pointer instead,
    // such as the asset/ibl panoramas before `git lfs pull`.
    if bytes.starts_with(b"version https://git-lfs.github.com/spec/") {
        return Err(format!(
            "asset {} is a Git LFS pointer, not its file: run `git lfs pull` where it is checked out",
            asset.id.as_str()
        )
        .into());
    }
    if asset.blend_as.is_some() && asset.kind != AssetKind::Glb {
        return Err(format!(
            "asset {}: blend_as applies to glb assets only",
            asset.id.as_str()
        )
        .into());
    }
    Ok(match asset.kind {
        AssetKind::Glb => {
            let glb = import_glb_with(
                bytes,
                ImportOptions {
                    blend_as_mask: asset.blend_as == Some(BlendAs::Mask),
                },
            )?;
            for warning in &glb.warnings {
                log::warn!("asset {}: {warning}", asset.id.as_str());
            }
            Imported::Model(glb)
        }
        AssetKind::ImageQuad => Imported::Model(image_quad(bytes)?),
        AssetKind::Flag => Imported::Model(flag(bytes)?),
        AssetKind::Environment => Imported::Environment(environment(asset, bytes)?),
    })
}

/// A latitude-longitude OpenEXR panorama; other aspects would be stretched over the sphere.
fn environment(asset: &Asset, bytes: &[u8]) -> Result<EnvironmentMap, Error> {
    let map = import_exr(bytes)?;
    let extent = map.extent();
    if extent.width() != 2 * extent.height() {
        return Err(format!(
            "asset {}: an environment must be a 2:1 panorama, not {}x{}",
            asset.id.as_str(),
            extent.width(),
            extent.height()
        )
        .into());
    }
    Ok(map)
}

/// What makes two registrations of one asset ID the same: its kind, bytes and
/// import options.
pub fn identity(asset: &Asset) -> String {
    let digest = format!("{:?}:{}", asset.kind, asset.sha256.to_ascii_lowercase());
    match asset.blend_as {
        Some(BlendAs::Mask) => format!("{digest}+blend-as-mask"),
        None => digest,
    }
}

fn read(source: &AssetSource, paths: &Paths) -> Result<Vec<u8>, Error> {
    match source {
        AssetSource::File(file) => {
            let path = resolve(&file.path, paths)?;
            read_limited(File::open(&path)?, &path.display().to_string())
        }
        AssetSource::Package(package) => {
            let path = resolve(&package.package, paths)?;
            let mut archive = zip::ZipArchive::new(File::open(&path)?)?;
            let entry = archive.by_name(&package.entry)?;
            if entry.size() > MAX_ASSET_BYTES {
                return Err(format!("{} exceeds {MAX_ASSET_BYTES} bytes", package.entry).into());
            }
            read_limited(entry, &package.entry)
        }
    }
}

fn read_limited(reader: impl Read, name: &str) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    reader.take(MAX_ASSET_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ASSET_BYTES {
        return Err(format!("{name} exceeds {MAX_ASSET_BYTES} bytes").into());
    }
    Ok(bytes)
}

fn resolve(path: &Path, paths: &Paths) -> Result<PathBuf, Error> {
    match paths {
        Paths::Startup { base } => Ok(base.join(path)),
        Paths::Roots(roots) => {
            if !path.is_absolute() {
                return Err(format!("asset path {} is not absolute", path.display()).into());
            }
            let real = path.canonicalize()?;
            if !roots.iter().any(|root| real.starts_with(root)) {
                return Err(format!(
                    "asset path {} is outside the server's --asset-root folders",
                    path.display()
                )
                .into());
            }
            Ok(real)
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Opacity of an image quad instance without its own (#163), so the pitch shows
/// through ads.
pub const DEFAULT_IMAGE_OPACITY: f32 = 0.5;

/// The material of an image quad: an unlit, double-sided ground decal blended by the
/// image's alpha times `opacity`.
pub fn image_quad_material(opacity: f32) -> PbrMaterial {
    PbrMaterial {
        base_color: [1.0, 1.0, 1.0, opacity],
        unlit: true,
        double_sided: true,
        decal: true,
        ..Default::default()
    }
}

/// The unit image quad of `AssetKind::ImageQuad`: UV (0, 0) is the image's top-left
/// texel at (-0.5, -0.5), so the image reads upright in a top view with +Y down. Its
/// one part has [`image_quad_material`] at [`DEFAULT_IMAGE_OPACITY`]; an instance's
/// `opacity` overrides it.
fn image_quad(bytes: &[u8]) -> Result<GlbAsset, Error> {
    let texture = image_texture(bytes)?;
    let mesh = MeshData::new(
        vec![
            [-0.5, -0.5, 0.],
            [0.5, -0.5, 0.],
            [0.5, 0.5, 0.],
            [-0.5, 0.5, 0.],
        ],
        vec![0, 2, 1, 0, 3, 2],
        std::iter::once(0..6).collect(),
    )?
    .with_attributes(MeshAttributes {
        normals: vec![[0., 0., -1.]; 4],
        tex_coords: vec![[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
        ..Default::default()
    })?;
    Ok(GlbAsset {
        mesh,
        materials: vec![ImportedMaterial {
            factors: image_quad_material(DEFAULT_IMAGE_OPACITY),
            maps: MaterialMaps {
                base_color: Some(Arc::new(texture)),
                ..Default::default()
            },
        }],
        warnings: Vec::new(),
    })
}

/// Flag artwork on the shared flag grid: UV (0, 0) is the image's top-left texel at
/// the flag-local origin. Instances supply the size and motion; texels with alpha
/// below 0.5 are cut out, and both sides show the image.
fn flag(bytes: &[u8]) -> Result<GlbAsset, Error> {
    Ok(GlbAsset {
        mesh: MeshData::flag_grid(),
        materials: vec![ImportedMaterial {
            factors: PbrMaterial {
                roughness: 0.8,
                alpha_cutoff: Some(0.5),
                double_sided: true,
                ..Default::default()
            },
            maps: MaterialMaps {
                base_color: Some(Arc::new(image_texture(bytes)?)),
                ..Default::default()
            },
        }],
        warnings: Vec::new(),
    })
}

fn image_texture(bytes: &[u8]) -> Result<TextureData, Error> {
    let image = image::load_from_memory(bytes)?.into_rgba8();
    let extent = ImageExtent::new(image.width(), image.height())?;
    Ok(
        TextureData::new(extent, image.into_raw())?.with_sampler(TextureSampler {
            wrap_u: Wrap::Clamp,
            wrap_v: Wrap::Clamp,
            mag: Filter::Linear,
            min: Filter::Linear,
            mip: Some(Filter::Linear),
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use chr_common::description::{FilePath, PackageEntry};

    use super::*;

    fn png() -> Vec<u8> {
        let mut png = Cursor::new(Vec::new());
        image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 0])
            .unwrap()
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        png.into_inner()
    }

    fn asset(source: AssetSource, bytes: &[u8]) -> Asset {
        Asset {
            id: "ad".to_owned().try_into().unwrap(),
            kind: AssetKind::ImageQuad,
            source,
            sha256: hex(&Sha256::digest(bytes)),
            blend_as: None,
        }
    }

    #[test]
    fn image_quads_load_from_files_and_packages_and_verify_their_digest() {
        let dir = std::env::temp_dir().join(format!("chr-assets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = png();
        std::fs::write(dir.join("ad.png"), &png).unwrap();
        let mut zip = zip::ZipWriter::new(File::create(dir.join("match.zip")).unwrap());
        zip.start_file("assets/ad.png", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&png).unwrap();
        zip.finish().unwrap();

        let file = asset(
            AssetSource::File(FilePath {
                path: "ad.png".into(),
            }),
            &png,
        );
        let Imported::Model(quad) = load(&file, &Paths::Startup { base: &dir }).unwrap() else {
            panic!("an image quad is a model");
        };
        assert_eq!(quad.mesh.parts().len(), quad.materials.len());
        let maps = &quad.materials[0].maps;
        assert_eq!(
            maps.base_color.as_ref().unwrap().pixels()[..4],
            [255, 0, 0, 255]
        );
        assert!(quad.materials[0].factors.unlit && quad.materials[0].factors.decal);
        let mut masked = file.clone();
        masked.blend_as = Some(BlendAs::Mask);
        assert!(
            load(&masked, &Paths::Startup { base: &dir }).is_err(),
            "glb only"
        );

        let packaged = asset(
            AssetSource::Package(PackageEntry {
                package: dir.join("match.zip"),
                entry: "assets/ad.png".into(),
            }),
            &png,
        );
        let roots = [dir.canonicalize().unwrap()];
        assert!(load(&packaged, &Paths::Roots(&roots)).is_ok());

        let mut tampered = packaged.clone();
        tampered.sha256 = "0".repeat(64);
        let error = load(&tampered, &Paths::Roots(&roots))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("SHA-256"), "{error}");
        assert!(
            load(&file, &Paths::Roots(&roots)).is_err(),
            "relative at runtime"
        );
        let outside = [std::env::temp_dir().join("chr-no-such-root")];
        assert!(load(&packaged, &Paths::Roots(&outside)).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn environments_import_exr_panoramas_of_two_to_one_only() {
        let dir = std::env::temp_dir().join(format!("chr-environment-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let environment = |name: &str, width: usize, height: usize| {
            let path = dir.join(name);
            exr::prelude::write_rgb_file(&path, width, height, |x, _| (x as f32, 0.5, 2.0))
                .unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let mut described = asset(AssetSource::File(FilePath { path }), &bytes);
            described.kind = AssetKind::Environment;
            load(&described, &Paths::Startup { base: &dir })
        };
        let Imported::Environment(map) = environment("sky.exr", 4, 2).unwrap() else {
            panic!("an EXR is an environment");
        };
        assert_eq!((map.extent().width(), map.extent().height()), (4, 2));
        assert_eq!(map.pixels()[3], [3.0, 0.5, 2.0]);
        let error = environment("square.exr", 2, 2).err().unwrap().to_string();
        assert!(error.contains("2:1"), "{error}");
        let pointer =
            b"version https://git-lfs.github.com/spec/v1\noid sha256:e1d9\nsize 95419853\n";
        let mut described = asset(
            AssetSource::File(FilePath {
                path: "sky.exr".into(),
            }),
            pointer,
        );
        described.kind = AssetKind::Environment;
        let error = import(&described, pointer).err().unwrap().to_string();
        assert!(error.contains("git lfs pull"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
