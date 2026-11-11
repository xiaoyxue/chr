use chr_renderer::{
    glb::{GlbError, import_glb},
    scene::{ModelMatrix, Scene},
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
