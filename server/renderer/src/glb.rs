//! Static, self-contained GLB import. No filesystem access or animation evaluation.
//! `server/docs/gltf-support.md` lists the glTF features and extensions it reads.
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};
use thiserror::Error;

use crate::{
    coordinates::ImageExtent,
    scene::{
        Filter, MapSlot, MaterialLayers, MaterialMaps, MeshAttributes, MeshData, ModelMatrix,
        PbrMaterial, SceneError, TextureData, TextureSampler, UvTransform, Wrap,
    },
};

/// glTF extensions the importer and renderer implement. Any other extension is
/// ignored with a warning, or rejects the file when `extensionsRequired` lists it.
pub const IMPLEMENTED_EXTENSIONS: &[&str] = &[
    "EXT_texture_webp",
    "KHR_materials_anisotropy",
    "KHR_materials_clearcoat",
    "KHR_materials_emissive_strength",
    "KHR_materials_ior",
    "KHR_materials_iridescence",
    "KHR_materials_pbrSpecularGlossiness",
    "KHR_materials_sheen",
    "KHR_materials_specular",
    "KHR_materials_unlit",
    "KHR_materials_variants",
    "KHR_mesh_quantization",
    "KHR_texture_transform",
];

#[derive(Debug, Error)]
pub enum GlbError {
    #[error("invalid GLB: {0}")]
    Parse(#[from] gltf::Error),
    #[error("invalid imported geometry: {0}")]
    Geometry(#[from] SceneError),
    #[error("image decoding failed: {0}")]
    Image(#[from] image::ImageError),
    #[error("unsupported GLB feature: {0}")]
    Unsupported(String),
    #[error("GLB is missing valid embedded geometry or buffer data")]
    MissingData,
    #[error("GLB node graph is not a tree")]
    NodeGraph,
    #[error("GLB accessor or image references data outside its buffer")]
    InvalidData,
    #[error("invalid glTF material property: {0}")]
    InvalidProperty(String),
}

#[derive(Clone, Debug)]
pub struct ImportedMaterial {
    pub factors: PbrMaterial,
    pub maps: MaterialMaps,
}

#[derive(Clone, Debug)]
pub struct GlbAsset {
    pub mesh: MeshData,
    /// One binding per mesh part, in node/primitive order.
    pub materials: Vec<ImportedMaterial>,
    pub warnings: Vec<String>,
}

/// Explicit importer choices for features the renderer does not implement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// Import `alphaMode: BLEND` materials as alpha masks with a 0.5 cutoff instead
    /// of rejecting them. Suits decals whose transparency comes from their texture;
    /// translucent surfaces such as glass are cut out. Each such material is listed
    /// in the asset's warnings.
    pub blend_as_mask: bool,
}

/// glTF +Y-up is rotated to chr -Z-up: (x, y, z) -> (x, z, -y).
/// Default node transforms are baked once; callers apply only the instance model afterward.
/// Rejects alpha blending; see [`import_glb_with`].
pub fn import_glb(bytes: &[u8]) -> Result<GlbAsset, GlbError> {
    import_glb_with(bytes, ImportOptions::default())
}

/// [`import_glb`] with explicit [`ImportOptions`].
pub fn import_glb_with(bytes: &[u8], options: ImportOptions) -> Result<GlbAsset, GlbError> {
    if bytes.get(..4) != Some(b"glTF") {
        return Err(GlbError::MissingData);
    }
    let file = gltf::Gltf::from_slice_without_validation(bytes)?;
    validate(&file)?;
    // The gltf crate drops extension properties it does not model; read them here.
    let raw: Value = serde_json::from_slice(&gltf::binary::Glb::from_slice(bytes)?.json)
        .map_err(|error| GlbError::Parse(gltf::Error::Deserialize(error)))?;
    let blob = file.blob.as_deref().ok_or(GlbError::MissingData)?;
    for buffer in file.buffers() {
        if !matches!(buffer.source(), gltf::buffer::Source::Bin) {
            return Err(GlbError::Unsupported("external buffers".into()));
        }
        if buffer.length() > blob.len() {
            return Err(GlbError::InvalidData);
        }
    }
    let mut warnings = Vec::new();
    for extension in file.extensions_used() {
        if !IMPLEMENTED_EXTENSIONS.contains(&extension) {
            warnings.push(format!(
                "Extension {extension} is not implemented; the asset renders without it."
            ));
        }
    }
    let basis = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let mut primitives = Vec::new();
    let mut masked: Vec<String> = Vec::new();
    if let Some(scene) = file.default_scene().or_else(|| file.scenes().next()) {
        let mut stack: Vec<_> = scene.nodes().map(|node| (node, basis)).collect();
        stack.reverse();
        let mut visited = vec![false; file.nodes().len()];
        while let Some((node, parent)) = stack.pop() {
            if std::mem::replace(&mut visited[node.index()], true) {
                return Err(GlbError::NodeGraph);
            }
            if node.skin().is_some() {
                return Err(GlbError::Unsupported("skinning".into()));
            }
            let columns = node.transform().matrix();
            let local = std::array::from_fn(|r| std::array::from_fn(|c| f64::from(columns[c][r])));
            let world = multiply(parent, local);
            if let Some(mesh) = node.mesh() {
                primitives.extend(mesh.primitives().map(|p| (p, world)));
            }
            let start = stack.len();
            stack.extend(node.children().map(|child| (child, world)));
            stack[start..].reverse();
        }
    } else {
        primitives.extend(
            file.meshes()
                .flat_map(|m| m.primitives())
                .map(|p| (p, basis)),
        );
    }
    let mut positions = Vec::new();
    let mut indices = Vec::new();
    let mut attributes = MeshAttributes::default();
    let mut parts = Vec::new();
    let mut materials = Vec::new();
    let mut loader = Loader {
        file: &file,
        raw: &raw,
        blob,
        images: HashMap::new(),
        textures: HashMap::new(),
    };
    for (primitive, world) in primitives {
        if primitive.mode() != gltf::mesh::Mode::Triangles {
            return Err(GlbError::Unsupported("non-triangle primitives".into()));
        }
        if primitive.morph_targets().next().is_some() {
            return Err(GlbError::Unsupported("morph targets".into()));
        }
        let model = ModelMatrix::new(world)?;
        let reader = primitive.reader(|_| Some(blob));
        let attribute = |semantic| primitive.get(&semantic);
        let source: Vec<[f32; 3]> = read_vectors(
            attribute(gltf::Semantic::Positions).ok_or(GlbError::MissingData)?,
            blob,
        )?;
        let source_indices: Vec<u32> = match reader.read_indices() {
            Some(values) => values.into_u32().collect(),
            None => (0..u32::try_from(source.len()).map_err(|_| GlbError::InvalidData)?).collect(),
        };
        if source_indices.is_empty()
            || !source_indices.len().is_multiple_of(3)
            || source_indices.iter().any(|&i| i as usize >= source.len())
        {
            return Err(GlbError::InvalidData);
        }
        let normals: Option<Vec<[f32; 3]>> = attribute(gltf::Semantic::Normals)
            .map(|a| read_vectors(a, blob))
            .transpose()?;
        let uvs: Option<Vec<[f32; 2]>> = attribute(gltf::Semantic::TexCoords(0))
            .map(|a| read_vectors(a, blob))
            .transpose()?;
        let tangents: Option<Vec<[f32; 4]>> = attribute(gltf::Semantic::Tangents)
            .map(|a| read_vectors(a, blob))
            .transpose()?;
        let colors: Option<Vec<_>> = reader.read_colors(0).map(|v| v.into_rgba_f32().collect());
        for count in [
            normals.as_ref().map(Vec::len),
            uvs.as_ref().map(Vec::len),
            tangents.as_ref().map(Vec::len),
            colors.as_ref().map(Vec::len),
        ]
        .into_iter()
        .flatten()
        {
            if count != source.len() {
                return Err(GlbError::InvalidData);
            }
        }
        let material_source = primitive.material();
        if material_source.alpha_mode() == gltf::material::AlphaMode::Blend {
            if !options.blend_as_mask {
                return Err(GlbError::Unsupported("alpha blending".into()));
            }
            let name = material_source.name().unwrap_or("unnamed").to_owned();
            if !masked.contains(&name) {
                masked.push(name);
            }
        }
        let material = loader.material(material_source)?;
        if uvs.is_none()
            && MapSlot::ALL
                .into_iter()
                .any(|slot| material.maps.get(slot).is_some())
        {
            return Err(GlbError::Unsupported(
                "textured primitive without TEXCOORD_0".into(),
            ));
        }
        // Normal maps, clearcoat normal maps and anisotropy all need a tangent frame.
        let needs_tangents = material.maps.normal.is_some()
            || material.maps.clearcoat_normal.is_some()
            || material.maps.anisotropy.is_some()
            || material.factors.layers.anisotropy_strength > 0.0;
        let tangents = match (tangents, &uvs) {
            (None, Some(uvs)) if needs_tangents => Some(derive_tangents(
                &source,
                normals.as_deref(),
                uvs,
                &source_indices,
            )),
            (tangents, _) => tangents,
        };
        let expanded = normals.is_none();
        let order: Vec<u32> = if expanded {
            source_indices.clone()
        } else {
            (0..u32::try_from(source.len()).map_err(|_| GlbError::InvalidData)?).collect()
        };
        let base = u32::try_from(positions.len()).map_err(|_| GlbError::InvalidData)?;
        let mut flat = Vec::new();
        if expanded {
            for face in source_indices.as_chunks::<3>().0 {
                let [a, b, c] = [
                    source[face[0] as usize],
                    source[face[1] as usize],
                    source[face[2] as usize],
                ];
                let u = std::array::from_fn::<_, 3, _>(|i| f64::from(b[i]) - f64::from(a[i]));
                let v = std::array::from_fn::<_, 3, _>(|i| f64::from(c[i]) - f64::from(a[i]));
                let n = [
                    u[1] * v[2] - u[2] * v[1],
                    u[2] * v[0] - u[0] * v[2],
                    u[0] * v[1] - u[1] * v[0],
                ];
                flat.extend([model.transform_normal(n)?.map(|x| x as f32); 3]);
            }
        }
        for (offset, &index) in order.iter().enumerate() {
            let i = index as usize;
            positions.push(
                model
                    .transform_point(source[i].map(f64::from))?
                    .map(|v| v as f32),
            );
            attributes.normals.push(match &normals {
                Some(n) => model
                    .transform_normal(n[i].map(f64::from))?
                    .map(|v| v as f32),
                None => flat[offset],
            });
            attributes
                .tex_coords
                .push(uvs.as_ref().map_or([0.0; 2], |v| v[i]));
            attributes
                .colors
                .push(colors.as_ref().map_or([1.0; 4], |v| v[i]));
            // A zero tangent (no usable UV gradient) defers to the shader's derivative frame.
            let tangent = if let Some(t) = tangents.as_ref().filter(|t| t[i][..3] != [0.0; 3]) {
                let xyz =
                    std::array::from_fn(|r| (0..3).map(|c| world[r][c] * f64::from(t[i][c])).sum());
                let n = ModelMatrix::IDENTITY.transform_normal(xyz)?;
                [
                    n[0] as f32,
                    n[1] as f32,
                    n[2] as f32,
                    t[i][3] * if model.is_mirrored() { -1.0 } else { 1.0 },
                ]
            } else {
                [0.0, 0.0, 0.0, 1.0]
            };
            attributes.tangents.push(tangent);
        }
        let start = u32::try_from(indices.len()).map_err(|_| GlbError::InvalidData)?;
        let local = if expanded {
            (0..u32::try_from(order.len()).map_err(|_| GlbError::InvalidData)?).collect()
        } else {
            source_indices
        };
        for face in local.as_chunks::<3>().0 {
            let face = if model.is_mirrored() {
                [face[0], face[2], face[1]]
            } else {
                [face[0], face[1], face[2]]
            };
            for index in face {
                indices.push(base.checked_add(index).ok_or(GlbError::InvalidData)?);
            }
        }
        parts.push(start..u32::try_from(indices.len()).map_err(|_| GlbError::InvalidData)?);
        materials.push(material);
    }
    let mesh = MeshData::new(positions, indices, parts)?.with_attributes(attributes)?;
    if file.animations().next().is_some() {
        warnings.push("Animation clips ignored; authored default node transforms are used.".into());
    }
    warnings.extend(masked.into_iter().map(|name| {
        format!("Material {name:?}: alpha blending imported as an alpha mask (cutoff 0.5).")
    }));
    Ok(GlbAsset {
        mesh,
        materials,
        warnings,
    })
}

/// The gltf crate's validation, except that it rejects every required extension it does
/// not model itself; chr checks those against [`IMPLEMENTED_EXTENSIONS`] instead.
fn validate(file: &gltf::Gltf) -> Result<(), GlbError> {
    use gltf::json::validation::Validate;
    let root = file.as_json();
    let mut errors = Vec::new();
    root.validate(root, gltf::json::Path::new, &mut |path, error| {
        let path = path();
        if !path.as_str().starts_with("extensionsRequired") {
            errors.push((path, error));
        }
    });
    if !errors.is_empty() {
        return Err(gltf::Error::Validation(errors).into());
    }
    if let Some(extension) = file
        .extensions_required()
        .find(|extension| !IMPLEMENTED_EXTENSIONS.contains(extension))
    {
        return Err(GlbError::Unsupported(format!(
            "required extension {extension}"
        )));
    }
    Ok(())
}

/// Reads a vertex attribute of any component type as floats: normalized integers
/// (`KHR_mesh_quantization`) map to `[0, 1]` or `[-1, 1]`, others keep their values.
fn read_vectors<const N: usize>(
    accessor: gltf::Accessor<'_>,
    blob: &[u8],
) -> Result<Vec<[f32; N]>, GlbError>
where
    [f32; N]: gltf::accessor::Item,
    [i8; N]: gltf::accessor::Item,
    [u8; N]: gltf::accessor::Item,
    [i16; N]: gltf::accessor::Item,
    [u16; N]: gltf::accessor::Item,
    [u32; N]: gltf::accessor::Item,
{
    use gltf::accessor::{DataType, Dimensions};
    fn collect<T: Copy, const N: usize>(
        accessor: gltf::Accessor<'_>,
        blob: &[u8],
        convert: impl Fn(T) -> f32,
    ) -> Option<Vec<[f32; N]>>
    where
        [T; N]: gltf::accessor::Item,
    {
        let values = gltf::accessor::Iter::<[T; N]>::new(accessor, |_| Some(blob))?;
        Some(values.map(|v| v.map(&convert)).collect())
    }
    let dimensions = match N {
        2 => Dimensions::Vec2,
        3 => Dimensions::Vec3,
        _ => Dimensions::Vec4,
    };
    if accessor.dimensions() != dimensions {
        return Err(GlbError::InvalidData);
    }
    let normalized = accessor.normalized();
    let scale = |value: f32, max: f32| {
        if normalized {
            (value / max).max(-1.0)
        } else {
            value
        }
    };
    match accessor.data_type() {
        DataType::F32 => collect::<f32, N>(accessor, blob, |v| v),
        DataType::I8 => collect::<i8, N>(accessor, blob, |v| scale(f32::from(v), 127.0)),
        DataType::U8 => collect::<u8, N>(accessor, blob, |v| scale(f32::from(v), 255.0)),
        DataType::I16 => collect::<i16, N>(accessor, blob, |v| scale(f32::from(v), 32767.0)),
        DataType::U16 => collect::<u16, N>(accessor, blob, |v| scale(f32::from(v), 65535.0)),
        DataType::U32 => collect::<u32, N>(accessor, blob, |v| v as f32),
    }
    .ok_or(GlbError::InvalidData)
}

/// Reads materials from the typed document plus the raw JSON of the extensions the
/// gltf crate does not model, sharing decoded images and samplers between them.
struct Loader<'a> {
    file: &'a gltf::Gltf,
    raw: &'a Value,
    blob: &'a [u8],
    images: HashMap<usize, TextureData>,
    textures: HashMap<usize, Arc<TextureData>>,
}

impl Loader<'_> {
    fn material(&mut self, source: gltf::Material<'_>) -> Result<ImportedMaterial, GlbError> {
        let raw = source
            .index()
            .map_or(&Value::Null, |index| &self.raw["materials"][index]);
        let extension = |name: &str| raw.get("extensions").and_then(|e| e.get(name));
        let pbr = source.pbr_metallic_roughness();
        let alpha_cutoff = match source.alpha_mode() {
            gltf::material::AlphaMode::Opaque => None,
            gltf::material::AlphaMode::Mask => Some(source.alpha_cutoff().unwrap_or(0.5)),
            // Only reached with ImportOptions::blend_as_mask; see import_glb_with.
            gltf::material::AlphaMode::Blend => Some(0.5),
        };
        let mut factors = PbrMaterial {
            base_color: pbr.base_color_factor(),
            metallic: pbr.metallic_factor(),
            roughness: pbr.roughness_factor(),
            emissive: source
                .emissive_factor()
                .map(|v| v * source.emissive_strength().unwrap_or(1.0)),
            alpha_cutoff,
            double_sided: source.double_sided(),
            normal_scale: source.normal_texture().map_or(1.0, |v| v.scale()),
            occlusion_strength: source.occlusion_texture().map_or(1.0, |v| v.strength()),
            unlit: source.unlit(),
            // glTF has no shadow-catcher, decal or holdout concept; callers opt in explicitly.
            shadow_catcher: false,
            decal: false,
            holdout: false,
            layers: MaterialLayers::default(),
        };
        let mut maps = MaterialMaps::default();
        let core = &raw["pbrMetallicRoughness"];
        self.map(&mut maps, MapSlot::BaseColor, &core["baseColorTexture"])?;
        self.map(
            &mut maps,
            MapSlot::MetallicRoughness,
            &core["metallicRoughnessTexture"],
        )?;
        self.map(&mut maps, MapSlot::Normal, &raw["normalTexture"])?;
        self.map(&mut maps, MapSlot::Emissive, &raw["emissiveTexture"])?;
        self.map(&mut maps, MapSlot::Occlusion, &raw["occlusionTexture"])?;
        let layers = &mut factors.layers;
        if let Some(e) = extension("KHR_materials_ior") {
            layers.ior = number(e, "ior", 1.5)?;
        }
        if let Some(e) = extension("KHR_materials_specular") {
            layers.specular = number(e, "specularFactor", 1.0)?;
            layers.specular_color = numbers(e, "specularColorFactor", [1.0; 3])?;
            self.map(&mut maps, MapSlot::Specular, &e["specularTexture"])?;
            self.map(
                &mut maps,
                MapSlot::SpecularColor,
                &e["specularColorTexture"],
            )?;
        }
        if let Some(e) = extension("KHR_materials_clearcoat") {
            layers.clearcoat = number(e, "clearcoatFactor", 0.0)?;
            layers.clearcoat_roughness = number(e, "clearcoatRoughnessFactor", 0.0)?;
            layers.clearcoat_normal_scale = number(&e["clearcoatNormalTexture"], "scale", 1.0)?;
            self.map(&mut maps, MapSlot::Clearcoat, &e["clearcoatTexture"])?;
            self.map(
                &mut maps,
                MapSlot::ClearcoatRoughness,
                &e["clearcoatRoughnessTexture"],
            )?;
            self.map(
                &mut maps,
                MapSlot::ClearcoatNormal,
                &e["clearcoatNormalTexture"],
            )?;
        }
        if let Some(e) = extension("KHR_materials_sheen") {
            layers.sheen_color = numbers(e, "sheenColorFactor", [0.0; 3])?;
            layers.sheen_roughness = number(e, "sheenRoughnessFactor", 0.0)?;
            self.map(&mut maps, MapSlot::SheenColor, &e["sheenColorTexture"])?;
            self.map(
                &mut maps,
                MapSlot::SheenRoughness,
                &e["sheenRoughnessTexture"],
            )?;
        }
        if let Some(e) = extension("KHR_materials_anisotropy") {
            layers.anisotropy_strength = number(e, "anisotropyStrength", 0.0)?;
            layers.anisotropy_rotation = number(e, "anisotropyRotation", 0.0)?;
            self.map(&mut maps, MapSlot::Anisotropy, &e["anisotropyTexture"])?;
        }
        if let Some(e) = extension("KHR_materials_iridescence") {
            layers.iridescence = number(e, "iridescenceFactor", 0.0)?;
            layers.iridescence_ior = number(e, "iridescenceIor", 1.3)?;
            layers.iridescence_thickness = [
                number(e, "iridescenceThicknessMinimum", 100.0)?,
                number(e, "iridescenceThicknessMaximum", 400.0)?,
            ];
            self.map(&mut maps, MapSlot::Iridescence, &e["iridescenceTexture"])?;
            self.map(
                &mut maps,
                MapSlot::IridescenceThickness,
                &e["iridescenceThicknessTexture"],
            )?;
        }
        if let Some(e) = extension("KHR_materials_pbrSpecularGlossiness") {
            self.specular_glossiness(e, &mut factors, &mut maps)?;
        }
        factors.validate()?;
        Ok(ImportedMaterial { factors, maps })
    }

    /// Converts a specular-glossiness material as `KHR_materials_specular` recommends:
    /// a dielectric whose specular colour is the reflectance at every angle (`ior = 0`).
    /// The diffuse colour becomes the base colour; roughness is `1 - glossiness`, baked
    /// into a metallic-roughness texture when glossiness is textured.
    fn specular_glossiness(
        &mut self,
        e: &Value,
        factors: &mut PbrMaterial,
        maps: &mut MaterialMaps,
    ) -> Result<(), GlbError> {
        factors.base_color = numbers(e, "diffuseFactor", [1.0; 4])?;
        factors.metallic = 0.0;
        let glossiness = number(e, "glossinessFactor", 1.0)?;
        factors.roughness = 1.0 - glossiness;
        factors.layers.ior = 0.0;
        factors.layers.specular_color = numbers(e, "specularFactor", [1.0; 3])?;
        *maps.get_mut(MapSlot::BaseColor) = None;
        *maps.get_mut(MapSlot::MetallicRoughness) = None;
        self.map(maps, MapSlot::BaseColor, &e["diffuseTexture"])?;
        self.map(
            maps,
            MapSlot::SpecularColor,
            &e["specularGlossinessTexture"],
        )?;
        if let Some(texture) = maps.specular_color.clone() {
            let pixels = texture
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|texel| {
                    let rough = 1.0 - glossiness * f32::from(texel[3]) / 255.0;
                    [0, (rough.clamp(0.0, 1.0) * 255.0).round() as u8, 0, 255]
                })
                .collect();
            let rough = TextureData::new(texture.extent(), pixels)?.with_sampler(texture.sampler());
            maps.metallic_roughness = Some(Arc::new(rough));
            factors.roughness = 1.0;
            maps.transforms[MapSlot::MetallicRoughness as usize] =
                maps.transform(MapSlot::SpecularColor);
        }
        Ok(())
    }

    /// Loads one `textureInfo` into `slot`, with its `KHR_texture_transform`.
    fn map(
        &mut self,
        maps: &mut MaterialMaps,
        slot: MapSlot,
        info: &Value,
    ) -> Result<(), GlbError> {
        if info.is_null() {
            return Ok(());
        }
        let index = info
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| property("textureInfo.index"))? as usize;
        let mut tex_coord = info.get("texCoord").map_or(Some(0), Value::as_u64);
        let mut transform = UvTransform::default();
        if let Some(t) = info
            .get("extensions")
            .and_then(|e| e.get("KHR_texture_transform"))
        {
            let [offset_x, offset_y] = numbers(t, "offset", [0.0; 2])?;
            transform = UvTransform {
                offset: [offset_x, offset_y],
                rotation: number(t, "rotation", 0.0)?,
                scale: numbers(t, "scale", [1.0; 2])?,
            };
            if let Some(value) = t.get("texCoord") {
                tex_coord = value.as_u64();
            }
        }
        match tex_coord {
            Some(0) => {}
            Some(_) => {
                return Err(GlbError::Unsupported(
                    "texture coordinate set other than zero".into(),
                ));
            }
            None => return Err(property("textureInfo.texCoord")),
        }
        if !transform
            .offset
            .into_iter()
            .chain(transform.scale)
            .chain([transform.rotation])
            .all(f32::is_finite)
        {
            return Err(property("KHR_texture_transform"));
        }
        *maps.get_mut(slot) = Some(self.texture(index)?);
        maps.transforms[slot as usize] = transform;
        Ok(())
    }

    fn texture(&mut self, index: usize) -> Result<Arc<TextureData>, GlbError> {
        if let Some(value) = self.textures.get(&index) {
            return Ok(Arc::clone(value));
        }
        let texture = self
            .file
            .textures()
            .nth(index)
            .ok_or(GlbError::InvalidData)?;
        // EXT_texture_webp names its own image; the core source is then only a fallback.
        let webp = self.raw["textures"][index]["extensions"]["EXT_texture_webp"]["source"].as_u64();
        let image = match webp {
            Some(image) => self
                .file
                .images()
                .nth(image as usize)
                .ok_or(GlbError::InvalidData)?,
            None => texture.source().ok_or_else(|| {
                GlbError::Unsupported("texture without a PNG, JPEG or WebP image".into())
            })?,
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = self.images.entry(image.index()) {
            let gltf::image::Source::View { view, .. } = image.source() else {
                return Err(GlbError::Unsupported("external images".into()));
            };
            let end = view
                .offset()
                .checked_add(view.length())
                .ok_or(GlbError::InvalidData)?;
            let data = self
                .blob
                .get(view.offset()..end)
                .ok_or(GlbError::InvalidData)?;
            let decoded = image::load_from_memory(data)?.to_rgba8();
            let extent = ImageExtent::new(decoded.width(), decoded.height())
                .map_err(|_| GlbError::InvalidData)?;
            entry.insert(TextureData::new(extent, decoded.into_raw())?);
        }
        use gltf::texture::{MagFilter, MinFilter, WrappingMode};
        let sampler = texture.sampler();
        let wrap = |v| match v {
            WrappingMode::ClampToEdge => Wrap::Clamp,
            WrappingMode::MirroredRepeat => Wrap::Mirror,
            WrappingMode::Repeat => Wrap::Repeat,
        };
        let (min, mip) = match sampler.min_filter() {
            Some(MinFilter::Nearest) => (Filter::Nearest, None),
            Some(MinFilter::Linear) | None => (Filter::Linear, None),
            Some(MinFilter::NearestMipmapNearest) => (Filter::Nearest, Some(Filter::Nearest)),
            Some(MinFilter::LinearMipmapNearest) => (Filter::Linear, Some(Filter::Nearest)),
            Some(MinFilter::NearestMipmapLinear) => (Filter::Nearest, Some(Filter::Linear)),
            Some(MinFilter::LinearMipmapLinear) => (Filter::Linear, Some(Filter::Linear)),
        };
        let data = self.images[&image.index()]
            .clone()
            .with_sampler(TextureSampler {
                wrap_u: wrap(sampler.wrap_s()),
                wrap_v: wrap(sampler.wrap_t()),
                min,
                mip,
                mag: if sampler.mag_filter() == Some(MagFilter::Nearest) {
                    Filter::Nearest
                } else {
                    Filter::Linear
                },
            });
        let data = Arc::new(data);
        self.textures.insert(index, Arc::clone(&data));
        Ok(data)
    }
}

fn property(name: &str) -> GlbError {
    GlbError::InvalidProperty(name.into())
}

/// An optional numeric property; present values must be numbers.
fn number(object: &Value, key: &str, default: f32) -> Result<f32, GlbError> {
    match object.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_f64()
            .map(|v| v as f32)
            .ok_or_else(|| property(key)),
    }
}

/// An optional array of `N` numbers.
fn numbers<const N: usize>(
    object: &Value,
    key: &str,
    default: [f32; N],
) -> Result<[f32; N], GlbError> {
    let Some(value) = object.get(key) else {
        return Ok(default);
    };
    let values = value
        .as_array()
        .filter(|values| values.len() == N)
        .ok_or_else(|| property(key))?;
    let mut result = default;
    for (target, value) in result.iter_mut().zip(values) {
        *target = value.as_f64().ok_or_else(|| property(key))? as f32;
    }
    Ok(result)
}

fn multiply(a: [[f64; 4]; 4], b: [[f64; 4]; 4]) -> [[f64; 4]; 4] {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..4).map(|i| a[r][i] * b[i][c]).sum()))
}

/// Per-vertex tangents for normal-mapped primitives without `TANGENT`, in the
/// primitive's own space. `w` follows glTF: the bitangent `cross(n, t) * w` points
/// towards decreasing V, the texture's up, where normal maps put +Y.
fn derive_tangents(
    positions: &[[f32; 3]],
    normals: Option<&[[f32; 3]]>,
    uvs: &[[f32; 2]],
    indices: &[u32],
) -> Vec<[f32; 4]> {
    let mut sums = vec![[[0.0f64; 3]; 3]; positions.len()];
    for face in indices.as_chunks::<3>().0 {
        let [a, b, c] = face.map(|i| i as usize);
        let p = |i: usize| positions[i].map(f64::from);
        let uv = |i: usize| uvs[i].map(f64::from);
        let e1: [f64; 3] = std::array::from_fn(|k| p(b)[k] - p(a)[k]);
        let e2: [f64; 3] = std::array::from_fn(|k| p(c)[k] - p(a)[k]);
        let d1: [f64; 2] = std::array::from_fn(|k| uv(b)[k] - uv(a)[k]);
        let d2: [f64; 2] = std::array::from_fn(|k| uv(c)[k] - uv(a)[k]);
        let det = d1[0] * d2[1] - d1[1] * d2[0];
        let face_normal = cross(e1, e2);
        for i in [a, b, c] {
            sums[i][2] = std::array::from_fn(|k| sums[i][2][k] + face_normal[k]);
        }
        if det.abs() <= 1e-20 {
            continue;
        }
        let dpdu: [f64; 3] = std::array::from_fn(|k| (e1[k] * d2[1] - e2[k] * d1[1]) / det);
        let dpdv: [f64; 3] = std::array::from_fn(|k| (e2[k] * d1[0] - e1[k] * d2[0]) / det);
        for i in [a, b, c] {
            sums[i][0] = std::array::from_fn(|k| sums[i][0][k] + dpdu[k]);
            sums[i][1] = std::array::from_fn(|k| sums[i][1][k] + dpdv[k]);
        }
    }
    sums.iter()
        .enumerate()
        .map(|(i, [dpdu, dpdv, face_normal])| {
            let n = normals.map_or(*face_normal, |n| n[i].map(f64::from));
            let n = scaled(n, 1.0 / dot(n, n).sqrt());
            let t: [f64; 3] = std::array::from_fn(|k| dpdu[k] - n[k] * dot(n, *dpdu));
            let length = dot(t, t).sqrt();
            if !length.is_finite() || length <= 1e-12 {
                return [0.0, 0.0, 0.0, 1.0];
            }
            let t = scaled(t, 1.0 / length);
            let w = if dot(cross(n, t), *dpdv) > 0.0 {
                -1.0
            } else {
                1.0
            };
            [t[0] as f32, t[1] as f32, t[2] as f32, w]
        })
        .collect()
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter().zip(b).map(|(x, y)| x * y).sum()
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn scaled(a: [f64; 3], s: f64) -> [f64; 3] {
    a.map(|x| x * s)
}
