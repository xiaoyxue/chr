//! Static, self-contained GLB import. No filesystem access or animation evaluation.
use std::{collections::HashMap, sync::Arc};
use thiserror::Error;

use crate::{
    coordinates::ImageExtent,
    scene::{
        Filter, MaterialMaps, MeshAttributes, MeshData, ModelMatrix, PbrMaterial, SceneError,
        TextureData, TextureSampler, Wrap,
    },
};

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

/// glTF +Y-up is rotated to chr -Z-up: (x, y, z) -> (x, z, -y).
/// Default node transforms are baked once; callers apply only the instance model afterward.
pub fn import_glb(bytes: &[u8]) -> Result<GlbAsset, GlbError> {
    if bytes.get(..4) != Some(b"glTF") {
        return Err(GlbError::MissingData);
    }
    let file = gltf::Gltf::from_slice(bytes)?;
    let blob = file.blob.as_deref().ok_or(GlbError::MissingData)?;
    for buffer in file.buffers() {
        if !matches!(buffer.source(), gltf::buffer::Source::Bin) {
            return Err(GlbError::Unsupported("external buffers".into()));
        }
        if buffer.length() > blob.len() {
            return Err(GlbError::InvalidData);
        }
    }
    for extension in file.extensions_used() {
        if !["KHR_materials_unlit", "KHR_materials_emissive_strength"].contains(&extension) {
            return Err(GlbError::Unsupported(extension.into()));
        }
    }
    let basis = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let mut primitives = Vec::new();
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
    let mut images = HashMap::new();
    let mut textures = HashMap::new();
    for (primitive, world) in primitives {
        if primitive.mode() != gltf::mesh::Mode::Triangles {
            return Err(GlbError::Unsupported("non-triangle primitives".into()));
        }
        if primitive.morph_targets().next().is_some() {
            return Err(GlbError::Unsupported("morph targets".into()));
        }
        let model = ModelMatrix::new(world)?;
        let reader = primitive.reader(|_| Some(blob));
        let source: Vec<_> = reader
            .read_positions()
            .ok_or(GlbError::MissingData)?
            .collect();
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
        let normals: Option<Vec<_>> = reader.read_normals().map(Iterator::collect);
        let uvs: Option<Vec<_>> = reader.read_tex_coords(0).map(|v| v.into_f32().collect());
        let tangents: Option<Vec<_>> = reader.read_tangents().map(Iterator::collect);
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
        let material = read_material(primitive.material(), blob, &mut images, &mut textures)?;
        if uvs.is_none()
            && [
                &material.maps.base_color,
                &material.maps.normal,
                &material.maps.metallic_roughness,
                &material.maps.emissive,
                &material.maps.occlusion,
            ]
            .into_iter()
            .any(Option::is_some)
        {
            return Err(GlbError::Unsupported(
                "textured primitive without TEXCOORD_0".into(),
            ));
        }
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
            let tangent = if let Some(t) = &tangents {
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
    let warnings = if file.animations().next().is_some() {
        vec!["Animation clips ignored; authored default node transforms are used.".into()]
    } else {
        Vec::new()
    };
    Ok(GlbAsset {
        mesh,
        materials,
        warnings,
    })
}

fn read_material(
    source: gltf::Material<'_>,
    blob: &[u8],
    images: &mut HashMap<usize, TextureData>,
    textures: &mut HashMap<usize, Arc<TextureData>>,
) -> Result<ImportedMaterial, GlbError> {
    let pbr = source.pbr_metallic_roughness();
    let alpha_cutoff = match source.alpha_mode() {
        gltf::material::AlphaMode::Opaque => None,
        gltf::material::AlphaMode::Mask => Some(source.alpha_cutoff().unwrap_or(0.5)),
        gltf::material::AlphaMode::Blend => {
            return Err(GlbError::Unsupported("alpha blending".into()));
        }
    };
    let factors = PbrMaterial {
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
    };
    factors.validate()?;
    let maps = MaterialMaps {
        base_color: load_map(
            pbr.base_color_texture()
                .map(|v| (v.texture(), v.tex_coord())),
            blob,
            images,
            textures,
        )?,
        metallic_roughness: load_map(
            pbr.metallic_roughness_texture()
                .map(|v| (v.texture(), v.tex_coord())),
            blob,
            images,
            textures,
        )?,
        normal: load_map(
            source
                .normal_texture()
                .map(|v| (v.texture(), v.tex_coord())),
            blob,
            images,
            textures,
        )?,
        emissive: load_map(
            source
                .emissive_texture()
                .map(|v| (v.texture(), v.tex_coord())),
            blob,
            images,
            textures,
        )?,
        occlusion: load_map(
            source
                .occlusion_texture()
                .map(|v| (v.texture(), v.tex_coord())),
            blob,
            images,
            textures,
        )?,
    };
    Ok(ImportedMaterial { factors, maps })
}

fn load_map(
    source: Option<(gltf::Texture<'_>, u32)>,
    blob: &[u8],
    images: &mut HashMap<usize, TextureData>,
    textures: &mut HashMap<usize, Arc<TextureData>>,
) -> Result<Option<Arc<TextureData>>, GlbError> {
    let Some((texture, uv)) = source else {
        return Ok(None);
    };
    if uv != 0 {
        return Err(GlbError::Unsupported(
            "texture coordinate set other than zero".into(),
        ));
    }
    if let Some(value) = textures.get(&texture.index()) {
        return Ok(Some(Arc::clone(value)));
    }
    let image = texture.source();
    if let std::collections::hash_map::Entry::Vacant(entry) = images.entry(image.index()) {
        let gltf::image::Source::View { view, .. } = image.source() else {
            return Err(GlbError::Unsupported("external images".into()));
        };
        let end = view
            .offset()
            .checked_add(view.length())
            .ok_or(GlbError::InvalidData)?;
        let data = blob.get(view.offset()..end).ok_or(GlbError::InvalidData)?;
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
    let data = images[&image.index()].clone().with_sampler(TextureSampler {
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
    textures.insert(texture.index(), Arc::clone(&data));
    Ok(Some(data))
}

fn multiply(a: [[f64; 4]; 4], b: [[f64; 4]; 4]) -> [[f64; 4]; 4] {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..4).map(|i| a[r][i] * b[i][c]).sum()))
}
