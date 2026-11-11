//! CPU-only OpenEXR environment import; callers own byte loading and scheduling.

use std::io::Cursor;

use ::exr::{
    meta::{
        BlockDescription,
        attribute::{Chromaticities, EnvironmentMap as Projection, LevelMode, SampleType},
        header::Header,
    },
    prelude::{Compression, ReadChannels, ReadLayers, RgbaChannels, Vec2, read},
};
use thiserror::Error;

use crate::{
    coordinates::ImageExtent,
    scene::{EnvironmentMap, SceneError},
};

#[derive(Debug, Error)]
pub enum ExrError {
    #[error("invalid OpenEXR: {0}")]
    Decode(#[from] ::exr::error::Error),
    #[error("unsupported OpenEXR environment: {0}")]
    Unsupported(&'static str),
    #[error("invalid OpenEXR radiance: {0}")]
    Radiance(#[from] SceneError),
}

/// Decode one complete equirectangular image, outside the render hot path.
///
/// Supports single-part flat RGB or opaque RGBA, with independently HALF/FLOAT
/// channels, scan lines or single-level tiles, and lossless NONE/RLE/ZIP/ZIPS/PIZ
/// compression. HALF is widened exactly to f32; FLOAT is retained unchanged.
/// Pixels remain in file raster order (top row first), without rotation, gain,
/// exposure, tone mapping, resampling or colour conversion.
///
/// The working space is scene-linear sRGB/Rec.709 with D65 white. Missing
/// chromaticities explicitly means this space; tagged input must match it.
/// Other colour transforms, luminance scaling and custom attributes are rejected.
/// Missing `envmap` explicitly means latitude-longitude; cube maps, cropped or
/// shifted windows, non-square pixels and multiview data are not supported.
/// Alpha, when present, must be exactly one: associated transparent RGB cannot
/// be interpreted as complete environment radiance without a compositing policy.
pub fn import_exr(bytes: &[u8]) -> Result<EnvironmentMap, ExrError> {
    let chunks = ::exr::block::read(Cursor::new(bytes), true)?;
    let metadata = chunks.meta_data();
    if metadata.requirements.has_deep_data
        || metadata.requirements.has_multiple_layers
        || metadata.headers.len() != 1
    {
        return Err(ExrError::Unsupported("deep or multipart data"));
    }
    let header = &metadata.headers[0];
    validate_header(header)?;
    let width = u32::try_from(header.layer_size.width())
        .map_err(|_| ExrError::Unsupported("image dimensions"))?;
    let height = u32::try_from(header.layer_size.height())
        .map_err(|_| ExrError::Unsupported("image dimensions"))?;
    let extent =
        ImageExtent::new(width, height).map_err(|_| ExrError::Unsupported("empty image"))?;
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .rgba_channels(
            |size, _: &RgbaChannels| (vec![[f32::NAN; 3]; size.area()], true),
            move |(pixels, opaque), position, (r, g, b, a): (f32, f32, f32, f32)| {
                pixels[position.y() * width as usize + position.x()] = [r, g, b];
                *opaque &= a == 1.0;
            },
        )
        .first_valid_layer()
        .all_attributes()
        .pedantic()
        .from_chunks(chunks)?;
    let (pixels, opaque) = image.layer_data.channel_data.pixels;
    if !opaque {
        return Err(ExrError::Unsupported("non-opaque alpha"));
    }
    Ok(EnvironmentMap::new(extent, pixels)?)
}

fn validate_header(header: &Header) -> Result<(), ExrError> {
    if header.deep
        || matches!(header.blocks, BlockDescription::Tiles(tile) if tile.level_mode != LevelMode::Singular)
    {
        return Err(ExrError::Unsupported("deep data or mip/rip levels"));
    }
    if !matches!(
        header.compression,
        Compression::Uncompressed
            | Compression::RLE
            | Compression::ZIP1
            | Compression::ZIP16
            | Compression::PIZ
    ) {
        return Err(ExrError::Unsupported(
            "compression (requires NONE/RLE/ZIPS/ZIP/PIZ)",
        ));
    }
    let channels = &header.channels.list;
    let rgb = ["B", "G", "R"];
    let rgba = ["A", "B", "G", "R"];
    let names = if channels.len() == 3 {
        &rgb[..]
    } else {
        &rgba[..]
    };
    if channels.len() != names.len()
        || channels.iter().zip(names).any(|(channel, name)| {
            channel.name != **name
                || !matches!(channel.sample_type, SampleType::F16 | SampleType::F32)
                || channel.sampling != Vec2(1, 1)
        })
    {
        return Err(ExrError::Unsupported(
            "requires unsubsampled R/G/B or R/G/B/A HALF/FLOAT channels",
        ));
    }
    let shared = &header.shared_attributes;
    let own = &header.own_attributes;
    if shared.display_window.position != Vec2(0, 0)
        || own.layer_position != Vec2(0, 0)
        || shared.display_window.size != header.layer_size
        || shared.pixel_aspect != 1.0
        || own.environment_map == Some(Projection::Cube)
        || own.multi_view_names.is_some()
        || own.view_name.is_some()
    {
        return Err(ExrError::Unsupported(
            "requires a complete, square-pixel latitude-longitude image",
        ));
    }
    if shared
        .chromaticities
        .is_some_and(|value| !is_linear_srgb(value))
        || own.white_luminance.is_some()
        || own.adopted_neutral.is_some()
        || own.rendering_transform_name.is_some()
        || own.look_modification_transform_name.is_some()
        || !shared.other.is_empty()
        || !own.other.is_empty()
    {
        return Err(ExrError::Unsupported(
            "colour metadata (requires linear sRGB/D65 without transforms or custom attributes)",
        ));
    }
    Ok(())
}

fn is_linear_srgb(value: Chromaticities) -> bool {
    [
        (value.red, Vec2(0.64, 0.33)),
        (value.green, Vec2(0.30, 0.60)),
        (value.blue, Vec2(0.15, 0.06)),
        (value.white, Vec2(0.3127, 0.3290)),
    ]
    .into_iter()
    .all(|(actual, expected)| {
        (actual.x() - expected.x()).abs() <= 1e-5 && (actual.y() - expected.y()).abs() <= 1e-5
    })
}
