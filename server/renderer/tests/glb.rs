use chr_renderer::{
    glb::{GlbError, ImportOptions, import_glb, import_glb_with},
    scene::{MapSlot, ModelMatrix, Scene, UvTransform},
};
use std::{io::Cursor, sync::Arc};

fn fixture() -> (String, Vec<u8>) {
    let mut bin = Vec::new();
    for value in [
        0.0_f32, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0,
        1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 2.0,
    ] {
        bin.extend(value.to_le_bytes());
    }
    for value in [0_u16, 1, 2] {
        bin.extend(value.to_le_bytes());
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let image_offset = bin.len();
    let mut png = Cursor::new(Vec::new());
    image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 255])
        .unwrap()
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let png = png.into_inner();
    bin.extend(&png);
    let json=r#"{
      "asset":{"version":"2.0"},"scene":0,
      "scenes":[{"nodes":[0,2]}],
      "nodes":[{"translation":[1,0,0],"children":[1]},
               {"mesh":0,"translation":[0,2,0]},
               {"mesh":1,"translation":[3,0,0],"scale":[-1,1,1]}],
      "buffers":[{"byteLength":$LEN}],
      "bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36},
                     {"buffer":0,"byteOffset":36,"byteLength":36},
                     {"buffer":0,"byteOffset":72,"byteLength":24},
                     {"buffer":0,"byteOffset":96,"byteLength":12},
                     {"buffer":0,"byteOffset":108,"byteLength":6},
                     {"buffer":0,"byteOffset":$IMAGE_OFFSET,"byteLength":$IMAGE_LEN}],
      "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[2,1,0]},
                   {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},
                   {"bufferView":2,"componentType":5126,"count":3,"type":"VEC2"},
                   {"bufferView":3,"componentType":5126,"count":3,"type":"SCALAR","min":[0],"max":[2]},
                   {"bufferView":4,"componentType":5123,"count":3,"type":"SCALAR"}],
      "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2},"indices":4,"material":0,"mode":4}]},
                {"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2},"indices":4,"material":1,"mode":4}]}],
      "materials":[{"pbrMetallicRoughness":{"baseColorTexture":{"index":0}},"normalTexture":{"index":0,"scale":0.5}},
                   {"pbrMetallicRoughness":{"baseColorTexture":{"index":0},"metallicFactor":0.25}}],
      "images":[{"bufferView":5,"mimeType":"image/png"}],
      "textures":[{"source":0}],
      "animations":[{"samplers":[{"input":3,"output":0}],
                     "channels":[{"sampler":0,"target":{"node":1,"path":"translation"}}]}]
    }"#.replace("$LEN",&bin.len().to_string())
        .replace("$IMAGE_OFFSET",&image_offset.to_string())
        .replace("$IMAGE_LEN",&png.len().to_string());
    (json, bin)
}

fn pack(json: &str, data: &[u8]) -> Vec<u8> {
    let mut json = json.as_bytes().to_vec();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut data = data.to_vec();
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let mut result = Vec::from(*b"glTF");
    result.extend(2_u32.to_le_bytes());
    result.extend((28_u32 + json.len() as u32 + data.len() as u32).to_le_bytes());
    result.extend((json.len() as u32).to_le_bytes());
    result.extend(*b"JSON");
    result.extend(json);
    result.extend((data.len() as u32).to_le_bytes());
    result.extend(*b"BIN\0");
    result.extend(data);
    result
}

#[test]
fn default_nodes_material_parts_and_images_are_preserved() {
    let (json, bin) = fixture();
    let asset = import_glb(&pack(&json, &bin)).unwrap();
    assert_eq!(asset.mesh.positions()[0], [1.0, 0.0, -2.0]);
    assert_eq!(asset.mesh.positions()[3], [3.0, 0.0, 0.0]);
    assert_eq!(asset.mesh.indices(), &[0, 1, 2, 3, 5, 4]);
    assert_eq!(asset.mesh.parts(), &[0..3, 3..6]);
    assert_eq!(asset.mesh.attributes().normals[0], [0.0, 1.0, 0.0]);
    assert_eq!(asset.materials[0].factors.metallic, 1.0);
    assert_eq!(asset.materials[1].factors.metallic, 0.25);
    assert_eq!(asset.materials[0].factors.normal_scale, 0.5);
    assert_eq!(asset.warnings.len(), 1);
    let a = asset.materials[0].maps.base_color.as_ref().unwrap();
    let b = asset.materials[1].maps.base_color.as_ref().unwrap();
    assert!(Arc::ptr_eq(a, b));
    assert_eq!(a.pixels(), &[255, 0, 0, 255, 0, 255, 0, 255]);
    let mut scene = Scene::new().unwrap();
    let mesh = scene.add_mesh(asset.mesh).unwrap();
    let mats = asset
        .materials
        .into_iter()
        .map(|m| scene.add_material_with_maps(m.factors, m.maps).unwrap())
        .collect();
    let id = scene
        .add_instance(mesh, mats, ModelMatrix::IDENTITY)
        .unwrap();
    assert_eq!(
        scene.snapshot().instance(id).unwrap().bounds().max(),
        [3.0, 0.0, 0.0]
    );
}

#[test]
fn omitted_normals_are_flat_and_unsupported_inputs_fail() {
    let (json, bin) = fixture();
    let flat = import_glb(&pack(&json.replace("\"NORMAL\":1,", ""), &bin)).unwrap();
    assert_eq!(flat.mesh.attributes().normals[0], [0.0, 1.0, 0.0]);
    for json in [
        json.replace("\"mode\":4", "\"mode\":1"),
        json.replace("\"mesh\":0,", "\"mesh\":0,\"skin\":0,")
            .replace("\"scene\":0", "\"skins\":[{\"joints\":[2]}],\"scene\":0"),
        json.replace(
            "\"buffers\":[{",
            "\"buffers\":[{\"uri\":\"not-loaded.bin\",",
        ),
        json.replace(
            "\"material\":0",
            "\"material\":0,\"targets\":[{\"POSITION\":0}]",
        ),
    ] {
        assert!(matches!(
            import_glb(&pack(&json, &bin)),
            Err(GlbError::Unsupported(_))
        ));
    }
    assert!(import_glb(b"invalid").is_err());
}

#[test]
fn blended_materials_import_as_masks_only_on_request() {
    let (json, bin) = fixture();
    let blended = pack(
        &json.replace(
            "\"materials\":[{",
            "\"materials\":[{\"name\":\"badge\",\"alphaMode\":\"BLEND\",",
        ),
        &bin,
    );
    assert!(matches!(
        import_glb(&blended),
        Err(GlbError::Unsupported(feature)) if feature == "alpha blending"
    ));
    let masked = import_glb_with(
        &blended,
        ImportOptions {
            blend_as_mask: true,
        },
    )
    .unwrap();
    assert_eq!(masked.materials[0].factors.alpha_cutoff, Some(0.5));
    assert_eq!(masked.materials[1].factors.alpha_cutoff, None);
    assert!(
        masked
            .warnings
            .iter()
            .any(|warning| warning.contains("\"badge\"") && warning.contains("alpha mask")),
        "{:?}",
        masked.warnings
    );
}

#[test]
fn material_map_changes_preserve_older_views() {
    let (json, bin) = fixture();
    let asset = import_glb(&pack(&json, &bin)).unwrap();
    let mut scene = Scene::new().unwrap();
    let m = &asset.materials[0];
    let id = scene
        .add_material_with_maps(m.factors, m.maps.clone())
        .unwrap();
    let old = scene.snapshot();
    scene.set_material_maps(id, Default::default()).unwrap();
    assert!(old.material_maps(id).unwrap().base_color.is_some());
    assert!(
        scene
            .snapshot()
            .material_maps(id)
            .unwrap()
            .base_color
            .is_none()
    );
}

#[test]
fn normal_mapped_primitives_without_tangents_get_gltf_handed_tangents() {
    let (json, bin) = fixture();
    let tangents = |uvs: [f32; 6]| {
        let mut bin = bin.clone();
        for (i, value) in uvs.into_iter().enumerate() {
            bin[72 + 4 * i..76 + 4 * i].copy_from_slice(&value.to_le_bytes());
        }
        import_glb(&pack(&json, &bin))
            .unwrap()
            .mesh
            .attributes()
            .tangents[..4]
            .to_vec()
    };
    let close = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6);
    // U along glTF +X; V growing up (+Y) runs against the texture's up, so w = -1.
    let upward = tangents([0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
    assert!(close(upward[0], [1.0, 0.0, 0.0, -1.0]), "{upward:?}");
    // The second primitive has no normal map and keeps the shader's derivative frame.
    assert_eq!(upward[3], [0.0, 0.0, 0.0, 1.0]);
    let downward = tangents([0.0, 1.0, 1.0, 1.0, 0.0, 0.0]);
    assert!(close(downward[0], [1.0, 0.0, 0.0, 1.0]), "{downward:?}");
    // Without a UV gradient the tangent stays zero for the shader to derive.
    assert_eq!(tangents([0.5; 6])[0], [0.0, 0.0, 0.0, 1.0]);
}

const MATERIAL_ZERO: &str = r#""materials":[{"pbrMetallicRoughness":{"baseColorTexture":{"index":0}},"normalTexture":{"index":0,"scale":0.5}},"#;

/// The fixture with material 0 replaced and `extensions` declared on the asset.
fn with_material(material: &str, extensions: &str) -> Vec<u8> {
    let (json, bin) = fixture();
    assert!(json.contains(MATERIAL_ZERO));
    let json = json
        .replace(MATERIAL_ZERO, &format!("\"materials\":[{material},"))
        .replace(
            "\"asset\":{\"version\":\"2.0\"}",
            &format!("\"asset\":{{\"version\":\"2.0\"}},{extensions}"),
        );
    pack(&json, &bin)
}

#[test]
fn material_extensions_import_factors_maps_and_texture_transforms() {
    let glb = with_material(
        r#"{"pbrMetallicRoughness":{"baseColorTexture":{"index":0,"extensions":{"KHR_texture_transform":
              {"offset":[0.5,0.25],"rotation":1.5707964,"scale":[2,3]}}}},
            "extensions":{
              "KHR_materials_ior":{"ior":1.8},
              "KHR_materials_specular":{"specularFactor":0.8,"specularColorFactor":[1.2,1,0.5],
                                        "specularTexture":{"index":0},"specularColorTexture":{"index":0}},
              "KHR_materials_clearcoat":{"clearcoatFactor":0.5,"clearcoatRoughnessFactor":0.2,
                                         "clearcoatTexture":{"index":0},"clearcoatRoughnessTexture":{"index":0},
                                         "clearcoatNormalTexture":{"index":0,"scale":0.7}},
              "KHR_materials_sheen":{"sheenColorFactor":[0.2,0.3,0.4],"sheenRoughnessFactor":0.6,
                                     "sheenColorTexture":{"index":0},"sheenRoughnessTexture":{"index":0}},
              "KHR_materials_anisotropy":{"anisotropyStrength":0.4,"anisotropyRotation":1.0,
                                          "anisotropyTexture":{"index":0}},
              "KHR_materials_iridescence":{"iridescenceFactor":0.9,"iridescenceIor":1.4,
                                           "iridescenceThicknessMinimum":200,"iridescenceThicknessMaximum":600,
                                           "iridescenceTexture":{"index":0},"iridescenceThicknessTexture":{"index":0}}}}"#,
        r#""extensionsUsed":["KHR_texture_transform","KHR_materials_ior","KHR_materials_specular",
            "KHR_materials_clearcoat","KHR_materials_sheen","KHR_materials_anisotropy","KHR_materials_iridescence"]"#,
    );
    let asset = import_glb(&glb).unwrap();
    // Only the animation warning: every extension used is implemented.
    assert_eq!(asset.warnings.len(), 1, "{:?}", asset.warnings);
    let material = &asset.materials[0];
    let layers = material.factors.layers;
    assert_eq!(layers.ior, 1.8);
    assert_eq!(
        (layers.specular, layers.specular_color),
        (0.8, [1.2, 1.0, 0.5])
    );
    assert_eq!(
        (
            layers.clearcoat,
            layers.clearcoat_roughness,
            layers.clearcoat_normal_scale
        ),
        (0.5, 0.2, 0.7)
    );
    assert_eq!(
        (layers.sheen_color, layers.sheen_roughness),
        ([0.2, 0.3, 0.4], 0.6)
    );
    assert_eq!(
        (layers.anisotropy_strength, layers.anisotropy_rotation),
        (0.4, 1.0)
    );
    assert_eq!(
        (
            layers.iridescence,
            layers.iridescence_ior,
            layers.iridescence_thickness
        ),
        (0.9, 1.4, [200.0, 600.0])
    );
    // The core normal map is gone with the replaced material; every extension slot is bound.
    assert!(material.maps.normal.is_none());
    for slot in MapSlot::ALL {
        let expected = !matches!(
            slot,
            MapSlot::MetallicRoughness | MapSlot::Normal | MapSlot::Emissive | MapSlot::Occlusion
        );
        assert_eq!(material.maps.get(slot).is_some(), expected, "{slot:?}");
    }
    // offset + rotation * scale: a quarter turn maps (u, v) to (3 v, -2 u) before the offset.
    let rows = material.maps.transform(MapSlot::BaseColor).rows();
    let apply = |u: f32, v: f32| rows.map(|r| r[0] * u + r[1] * v + r[2]);
    let close = |a: [f32; 2], b: [f32; 2]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5);
    assert!(close(apply(0.0, 0.0), [0.5, 0.25]));
    assert!(
        close(apply(1.0, 0.0), [0.5, -1.75]),
        "{:?}",
        apply(1.0, 0.0)
    );
    assert!(close(apply(0.0, 1.0), [3.5, 0.25]), "{:?}", apply(0.0, 1.0));
    assert_eq!(
        material.maps.transform(MapSlot::Clearcoat),
        UvTransform::default()
    );
    // Anisotropy needs a tangent frame: tangents are derived without a core normal map.
    assert_ne!(asset.mesh.attributes().tangents[0][..3], [0.0; 3]);
    assert_eq!(asset.mesh.attributes().tangents[3], [0.0, 0.0, 0.0, 1.0]);
}

#[test]
fn unimplemented_extensions_warn_unless_required() {
    let optional = with_material(
        r#"{"extensions":{"KHR_materials_transmission":{"transmissionFactor":1}}}"#,
        r#""extensionsUsed":["KHR_materials_transmission","KHR_mesh_quantization"],"extensionsRequired":["KHR_mesh_quantization"]"#,
    );
    let asset = import_glb(&optional).unwrap();
    assert!(
        asset
            .warnings
            .iter()
            .any(|w| w.contains("KHR_materials_transmission") && w.contains("not implemented")),
        "{:?}",
        asset.warnings
    );
    let required = with_material(
        "{}",
        r#""extensionsUsed":["KHR_materials_transmission"],"extensionsRequired":["KHR_materials_transmission"]"#,
    );
    assert!(matches!(
        import_glb(&required),
        Err(GlbError::Unsupported(feature)) if feature.contains("KHR_materials_transmission")
    ));
    let variants = with_material(
        "{}",
        r#""extensionsUsed":["KHR_materials_variants"],"extensions":{"KHR_materials_variants":{"variants":[{"name":"alt"}]}}"#,
    );
    assert_eq!(import_glb(&variants).unwrap().warnings.len(), 1);
}

#[test]
fn specular_glossiness_becomes_specular_colour_with_ior_zero() {
    let glb = with_material(
        r#"{"extensions":{"KHR_materials_pbrSpecularGlossiness":{"diffuseFactor":[0.5,0.6,0.7,1],
            "diffuseTexture":{"index":0},"specularFactor":[0.5,0.4,0.3],"glossinessFactor":0.8,
            "specularGlossinessTexture":{"index":0}}}}"#,
        r#""extensionsUsed":["KHR_materials_pbrSpecularGlossiness"]"#,
    );
    let material = &import_glb(&glb).unwrap().materials[0];
    let factors = material.factors;
    assert_eq!(factors.base_color, [0.5, 0.6, 0.7, 1.0]);
    assert_eq!((factors.metallic, factors.roughness), (0.0, 1.0));
    assert_eq!(factors.layers.ior, 0.0);
    assert_eq!(factors.layers.specular_color, [0.5, 0.4, 0.3]);
    assert!(material.maps.base_color.is_some() && material.maps.specular_color.is_some());
    // Opaque texels have glossiness 1: roughness 1 - 0.8 in G, metallic 0 in B.
    let rough = material.maps.metallic_roughness.as_ref().unwrap();
    assert_eq!(rough.pixels(), &[0, 51, 0, 255, 0, 51, 0, 255]);
}

#[test]
fn invalid_extension_values_are_rejected() {
    for (material, check) in [
        (
            r#"{"extensions":{"KHR_materials_ior":{"ior":0.5}}}"#,
            (|e: &GlbError| matches!(e, GlbError::Geometry(_))) as fn(&GlbError) -> bool,
        ),
        (
            r#"{"extensions":{"KHR_materials_specular":{"specularFactor":"high"}}}"#,
            |e| matches!(e, GlbError::InvalidProperty(_)),
        ),
        (
            r#"{"extensions":{"KHR_materials_sheen":{"sheenColorFactor":[1,1]}}}"#,
            |e| matches!(e, GlbError::InvalidProperty(_)),
        ),
        (
            r#"{"pbrMetallicRoughness":{"baseColorTexture":{"index":0,"extensions":{"KHR_texture_transform":{"texCoord":1}}}}}"#,
            |e| matches!(e, GlbError::Unsupported(_)),
        ),
        (
            r#"{"extensions":{"KHR_materials_clearcoat":{"clearcoatFactor":1,"clearcoatTexture":{"index":7}}}}"#,
            |e| matches!(e, GlbError::InvalidData),
        ),
    ] {
        let error = import_glb(&with_material(material, r#""extensionsUsed":[]"#)).unwrap_err();
        assert!(check(&error), "{material}: {error}");
    }
}

/// One triangle whose attributes use `KHR_mesh_quantization` types: unnormalized
/// `u16` positions, normalized `i8` normals and normalized `u16` texture coordinates.
#[test]
fn quantized_attributes_are_dequantized() {
    let mut bin = Vec::new();
    for p in [[0_u16, 0, 0], [4, 0, 0], [0, 2, 0]] {
        for c in p {
            bin.extend(c.to_le_bytes());
        }
        bin.extend([0, 0]);
    }
    for _ in 0..3 {
        bin.extend([0_i8, 127, 0, 0].map(|v| v as u8));
    }
    for uv in [[0_u16, 0], [65535, 0], [0, 32768]] {
        for c in uv {
            bin.extend(c.to_le_bytes());
        }
    }
    let json = r#"{"asset":{"version":"2.0"},"extensionsUsed":["KHR_mesh_quantization"],
      "extensionsRequired":["KHR_mesh_quantization"],
      "buffers":[{"byteLength":48}],
      "bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":24,"byteStride":8},
                     {"buffer":0,"byteOffset":24,"byteLength":12,"byteStride":4},
                     {"buffer":0,"byteOffset":36,"byteLength":12,"byteStride":4}],
      "accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"VEC3","min":[0,0,0],"max":[4,2,0]},
                   {"bufferView":1,"componentType":5120,"normalized":true,"count":3,"type":"VEC3"},
                   {"bufferView":2,"componentType":5123,"normalized":true,"count":3,"type":"VEC2"}],
      "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2}}]}],
      "nodes":[{"mesh":0}],"scenes":[{"nodes":[0]}],"scene":0}"#;
    let asset = import_glb(&pack(json, &bin)).unwrap();
    assert!(asset.warnings.is_empty(), "{:?}", asset.warnings);
    // glTF (x, y, z) becomes chr (x, z, -y).
    assert_eq!(
        asset.mesh.positions(),
        &[[0.0, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 0.0, -2.0]]
    );
    assert_eq!(asset.mesh.attributes().normals[0], [0.0, 0.0, -1.0]);
    let uvs = &asset.mesh.attributes().tex_coords;
    assert_eq!(uvs[1], [1.0, 0.0]);
    assert!((uvs[2][1] - 0.5).abs() < 1e-4, "{uvs:?}");
}

#[test]
fn webp_textures_decode() {
    let mut webp = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
        .encode(
            &[10, 20, 30, 255, 200, 100, 50, 128],
            2,
            1,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    let (json, mut bin) = fixture();
    let offset = bin.len();
    bin.extend(&webp);
    let mut root: serde_json::Value = serde_json::from_str(&json).unwrap();
    root["buffers"][0]["byteLength"] = bin.len().into();
    root["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"buffer": 0, "byteOffset": offset, "byteLength": webp.len()}));
    root["images"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"bufferView": 6, "mimeType": "image/webp"}));
    // A WebP-only texture: no core source, as the extension allows when required.
    root["textures"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"extensions": {"EXT_texture_webp": {"source": 1}}}));
    root["materials"][1]["pbrMetallicRoughness"]["baseColorTexture"]["index"] = 1.into();
    root["extensionsUsed"] = serde_json::json!(["EXT_texture_webp"]);
    root["extensionsRequired"] = serde_json::json!(["EXT_texture_webp"]);
    let asset = import_glb(&pack(&root.to_string(), &bin)).unwrap();
    let texture = asset.materials[1].maps.base_color.as_ref().unwrap();
    assert_eq!(texture.pixels(), &[10, 20, 30, 255, 200, 100, 50, 128]);
}
