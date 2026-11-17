use std::{io::Cursor, sync::Arc};

use chr_renderer::{
    exr::{ExrError, import_exr},
    scene::{Lighting, Scene, SceneError, SkyLight},
};
use exr::{
    meta::attribute::{Chromaticities, EnvironmentMap as Projection},
    prelude::*,
};

type Fixture = Image<Layer<AnyChannels<FlatSamples>>>;

fn fixture(alpha: bool, half: bool) -> Fixture {
    let channels = ["R", "G", "B", "A"]
        .into_iter()
        .take(if alpha { 4 } else { 3 })
        .map(|name| {
            let values = (0..8)
                .map(|index| match name {
                    "R" => 1.0003 + index as f32,
                    "G" => 0.25,
                    "B" => 128.5,
                    _ => 1.0,
                })
                .collect::<Vec<_>>();
            let samples = if half {
                FlatSamples::F16(values.into_iter().map(f16::from_f32).collect())
            } else {
                FlatSamples::F32(values)
            };
            AnyChannel::new(name, samples)
        })
        .collect();
    Image::from_layer(Layer::new(
        (4, 2),
        LayerAttributes::default(),
        Encoding {
            compression: Compression::Uncompressed,
            blocks: Blocks::ScanLines,
            line_order: LineOrder::Increasing,
        },
        AnyChannels::sort(channels),
    ))
}

fn encode(image: &Fixture) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image.write().to_buffered(&mut bytes).unwrap();
    bytes.into_inner()
}

fn srgb() -> Chromaticities {
    Chromaticities {
        red: Vec2(0.64, 0.33),
        green: Vec2(0.30, 0.60),
        blue: Vec2(0.15, 0.06),
        white: Vec2(0.3127, 0.3290),
    }
}

#[test]
fn rgb_and_opaque_rgba_preserve_half_and_float_hdr_in_raster_order() {
    for alpha in [false, true] {
        for half in [false, true] {
            let mut image = fixture(alpha, half);
            image.attributes.chromaticities = Some(srgb());
            image.layer_data.attributes.environment_map = Some(Projection::LatitudeLongitude);
            let decoded = import_exr(&encode(&image)).unwrap();
            assert_eq!(
                (decoded.extent().width(), decoded.extent().height()),
                (4, 2)
            );
            for (index, actual) in decoded.pixels().iter().enumerate() {
                let r = 1.0003 + index as f32;
                let expected_r = if half { f16::from_f32(r).to_f32() } else { r };
                assert_eq!(*actual, [expected_r, 0.25, 128.5]);
            }
        }
    }
}

#[test]
fn mixed_precision_and_lossless_scanline_or_tiled_encodings() {
    for compression in [
        Compression::Uncompressed,
        Compression::RLE,
        Compression::ZIP1,
        Compression::ZIP16,
        Compression::PIZ,
    ] {
        for blocks in [Blocks::ScanLines, Blocks::Tiles(Vec2(2, 2))] {
            let mut image = fixture(false, false);
            image.layer_data.encoding.compression = compression;
            image.layer_data.encoding.blocks = blocks;
            image.layer_data.channel_data.list[0].sample_data =
                FlatSamples::F16(vec![f16::from_f32(128.5); 8]);
            let decoded = import_exr(&encode(&image)).unwrap();
            assert_eq!(decoded.pixels()[0], [1.0003, 0.25, 128.5]);
            assert_eq!(decoded.pixels()[7], [8.0003, 0.25, 128.5]);
        }
    }
}

#[test]
fn clamps_negative_radiance_to_zero() {
    for half in [false, true] {
        let mut image = fixture(false, half);
        let values = [-0.019, -1.0, 0.0, 2.0, -0.5, 1.0, 3.0, -0.0];
        image.layer_data.channel_data.list[2].sample_data = if half {
            FlatSamples::F16(values.into_iter().map(f16::from_f32).collect())
        } else {
            FlatSamples::F32(values.to_vec())
        };
        let decoded = import_exr(&encode(&image)).unwrap();
        let red: Vec<f32> = decoded.pixels().iter().map(|p| p[0]).collect();
        assert_eq!(red, [0.0, 0.0, 0.0, 2.0, 0.0, 1.0, 3.0, 0.0]);
    }
}

#[test]
fn accepts_rounded_srgb_tags_and_harmless_editor_attributes() {
    let mut image = fixture(false, false);
    // As float32 tags of real files store sRGB/D65.
    image.attributes.chromaticities = Some(Chromaticities {
        red: Vec2(0.640_009_2, 0.330_006_9),
        green: Vec2(0.300_007_6, 0.600_015_3),
        blue: Vec2(0.150_011_4, 0.059_998_5),
        white: Vec2(0.312_703_1, 0.328_999_8),
    });
    let other = &mut image.layer_data.attributes.other;
    other.insert(
        Text::from("PhotoshopLayerInfo"),
        AttributeValue::TextVector(vec![Text::from("{\"opacity\": \"255\"}")]),
    );
    other.insert(Text::from("Orientation"), AttributeValue::I32(1));
    assert!(import_exr(&encode(&image)).is_ok());
    image
        .layer_data
        .attributes
        .other
        .insert(Text::from("Orientation"), AttributeValue::I32(3));
    assert!(
        import_exr(&encode(&image)).is_err(),
        "a rotated image would turn the lighting"
    );
}

#[test]
fn rejects_nonfinite_radiance_using_scene_validation() {
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for half in [false, true] {
            let mut image = fixture(false, half);
            image.layer_data.channel_data.list[0].sample_data = if half {
                FlatSamples::F16(vec![f16::from_f32(value); 8])
            } else {
                FlatSamples::F32(vec![value; 8])
            };
            assert!(matches!(
                import_exr(&encode(&image)),
                Err(ExrError::Radiance(SceneError::InvalidEnvironment))
            ));
        }
    }
}

#[test]
fn rejects_transparent_and_invalid_alpha_without_unpremultiplication() {
    for value in [0.0, 0.5, -1.0, 2.0, f32::NAN, f32::INFINITY] {
        let mut image = fixture(true, false);
        image.layer_data.channel_data.list[0].sample_data = FlatSamples::F32(vec![value; 8]);
        assert!(matches!(
            import_exr(&encode(&image)),
            Err(ExrError::Unsupported(_))
        ));
    }
}

#[test]
fn rejects_wrong_channels_and_integer_samples() {
    for names in [
        vec!["Y"],
        vec!["R", "G"],
        vec!["R", "G", "B", "Z"],
        vec!["beauty.R", "beauty.G", "beauty.B"],
    ] {
        let mut image = fixture(false, false);
        image.layer_data.channel_data = AnyChannels::sort(
            names
                .into_iter()
                .map(|name| AnyChannel::new(name, FlatSamples::F32(vec![1.0; 8])))
                .collect(),
        );
        assert!(matches!(
            import_exr(&encode(&image)),
            Err(ExrError::Unsupported(_))
        ));
    }
    let mut image = fixture(false, false);
    image.layer_data.channel_data.list[0].sample_data = FlatSamples::U32(vec![1; 8]);
    assert!(matches!(
        import_exr(&encode(&image)),
        Err(ExrError::Unsupported(_))
    ));
}

#[test]
fn rejects_unsupported_or_invalid_colour_metadata() {
    for chromaticities in [
        Chromaticities {
            red: Vec2(0.68, 0.32),
            ..srgb()
        },
        Chromaticities {
            white: Vec2(0.32168, 0.33767),
            ..srgb()
        },
        Chromaticities {
            green: Vec2(f32::NAN, 0.6),
            ..srgb()
        },
    ] {
        let mut image = fixture(false, false);
        image.attributes.chromaticities = Some(chromaticities);
        assert!(import_exr(&encode(&image)).is_err());
    }
    let mut images = vec![fixture(false, false); 6];
    images[0].layer_data.attributes.white_luminance = Some(100.0);
    images[1].layer_data.attributes.adopted_neutral = Some(Vec2(0.3127, 0.329));
    images[2].layer_data.attributes.rendering_transform_name = Some(Text::from("transform"));
    images[3]
        .layer_data
        .attributes
        .look_modification_transform_name = Some(Text::from("look"));
    for (index, name) in ["acesImageContainerFlag", "colorSpace"]
        .into_iter()
        .enumerate()
    {
        images[index + 4]
            .layer_data
            .attributes
            .other
            .insert(Text::from(name), AttributeValue::Text(Text::from("ACES")));
    }
    for image in images {
        assert!(import_exr(&encode(&image)).is_err());
    }
    let mut image = fixture(false, false);
    image.attributes.chromaticities = Some(srgb());
    let mut bytes = encode(&image);
    let signature = b"chromaticities\0chromaticities\0";
    let start = bytes
        .windows(signature.len())
        .position(|window| window == signature)
        .unwrap();
    bytes[start + b"chromaticities\0".len()] = b'x';
    assert!(import_exr(&bytes).is_err());
}

#[test]
fn rejects_unsupported_projection_windows_views_and_lossy_compression() {
    let mut images = vec![fixture(false, false); 7];
    images[0].layer_data.attributes.environment_map = Some(Projection::Cube);
    images[1].attributes.pixel_aspect = 2.0;
    images[2].layer_data.attributes.layer_position = Vec2(1, 0);
    images[3].attributes.display_window.size = Vec2(8, 4);
    images[4].layer_data.attributes.multi_view_names = Some(vec![Text::from("left")]);
    images[5].layer_data.attributes.view_name = Some(Text::from("left"));
    images[6].layer_data.encoding.compression = Compression::PXR24;
    for image in images {
        assert!(matches!(
            import_exr(&encode(&image)),
            Err(ExrError::Unsupported(_))
        ));
    }
}

#[test]
fn rejects_multipart_even_when_the_first_part_is_valid_rgb() {
    let mut first = fixture(false, false);
    first.layer_data.attributes.layer_name = Some(Text::from("first"));
    let mut second = first.layer_data.clone();
    second.attributes.layer_name = Some(Text::from("second"));
    let image = Image {
        attributes: first.attributes,
        layer_data: vec![first.layer_data, second]
            .into_iter()
            .collect::<Layers<_>>(),
    };
    let mut bytes = Cursor::new(Vec::new());
    image.write().to_buffered(&mut bytes).unwrap();
    assert!(matches!(
        import_exr(bytes.get_ref()),
        Err(ExrError::Unsupported(_))
    ));
}

fn attribute_offset(bytes: &[u8], signature: &[u8]) -> usize {
    bytes
        .windows(signature.len())
        .position(|window| window == signature)
        .unwrap()
        + signature.len()
        + 4 // Skip the attribute payload byte count.
}

#[test]
fn rejects_deep_multilevel_and_subsampled_headers_before_decoding_pixels() {
    let mut deep = encode(&fixture(false, false));
    deep[5] |= 8; // Version flag for non-image/deep data.
    assert!(import_exr(&deep).is_err());

    let mut image = fixture(false, false);
    image.layer_data.encoding.blocks = Blocks::Tiles(Vec2(2, 2));
    let mut tiled = encode(&image);
    let tiles = attribute_offset(&tiled, b"tiles\0tiledesc\0");
    tiled[tiles + 8] = 1; // Mipmap level mode.
    assert!(import_exr(&tiled).is_err());

    let mut subsampled = encode(&fixture(false, false));
    let channels = attribute_offset(&subsampled, b"channels\0chlist\0");
    // First channel: name "B\\0", sample type, pLinear/reserved, xSampling.
    subsampled[channels + 10..channels + 14].copy_from_slice(&2_i32.to_le_bytes());
    assert!(import_exr(&subsampled).is_err());
}

#[test]
fn rejects_truncated_corrupt_and_non_exr_data() {
    let bytes = encode(&fixture(false, false));
    for length in [0, 4, 8, bytes.len() / 2, bytes.len() - 1] {
        assert!(import_exr(&bytes[..length]).is_err());
    }
    assert!(import_exr(b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n").is_err());
    assert!(import_exr(b"\x89PNG\r\n\x1a\n").is_err());
    let mut corrupt = bytes;
    let compression = attribute_offset(&corrupt, b"compression\0compression\0");
    corrupt[compression] = 255;
    assert!(import_exr(&corrupt).is_err());
}

#[test]
fn replacement_keeps_old_scene_snapshot_and_failed_import_is_not_published() {
    let mut scene = Scene::new().unwrap();
    let lighting = |environment| {
        Lighting::new(
            vec![],
            Some(SkyLight {
                environment: Arc::new(environment),
                gain: 1.0,
                rotation: 0.0,
                visible: false,
            }),
        )
        .unwrap()
    };
    scene
        .set_lighting(lighting(
            import_exr(&encode(&fixture(false, false))).unwrap(),
        ))
        .unwrap();
    let before = scene.snapshot();
    let mut replacement = fixture(false, false);
    replacement.layer_data.channel_data.list[2].sample_data = FlatSamples::F32(vec![32.0; 8]);
    scene
        .set_lighting(lighting(import_exr(&encode(&replacement)).unwrap()))
        .unwrap();
    assert_eq!(
        before.lighting().sky().unwrap().environment.pixels()[0][0],
        1.0003
    );
    assert_eq!(
        scene
            .snapshot()
            .lighting()
            .sky()
            .unwrap()
            .environment
            .pixels()[0][0],
        32.0
    );
    assert!(import_exr(b"invalid replacement").is_err());
    assert_eq!(
        scene
            .snapshot()
            .lighting()
            .sky()
            .unwrap()
            .environment
            .pixels()[0][0],
        32.0
    );
}
