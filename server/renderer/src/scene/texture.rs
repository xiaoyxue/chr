use std::sync::Arc;

use super::SceneError;
use crate::coordinates::ImageExtent;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Wrap {
    #[default]
    Repeat,
    Clamp,
    Mirror,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    Nearest,
    #[default]
    Linear,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TextureSampler {
    pub wrap_u: Wrap,
    pub wrap_v: Wrap,
    pub mag: Filter,
    pub min: Filter,
    pub mip: Option<Filter>,
}

/// Immutable decoded RGBA8 texels; colour-space interpretation belongs to the map slot.
/// Cloning shares pixels, including when a source image has different samplers.
#[derive(Clone, Debug)]
pub struct TextureData {
    extent: ImageExtent,
    pixels: Arc<[u8]>,
    sampler: TextureSampler,
}

impl TextureData {
    pub fn new(extent: ImageExtent, pixels: Vec<u8>) -> Result<Self, SceneError> {
        let expected = u64::from(extent.width()) * u64::from(extent.height());
        if !pixels.len().is_multiple_of(4) || pixels.len() as u64 / 4 != expected {
            return Err(SceneError::InvalidTexture);
        }
        Ok(Self {
            extent,
            pixels: pixels.into(),
            sampler: TextureSampler::default(),
        })
    }
    pub fn with_sampler(mut self, sampler: TextureSampler) -> Self {
        self.sampler = sampler;
        self
    }
    pub fn extent(&self) -> ImageExtent {
        self.extent
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub fn sampler(&self) -> TextureSampler {
        self.sampler
    }
}

/// The texture slots of a material, in the order shaders bind them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MapSlot {
    BaseColor,
    MetallicRoughness,
    Normal,
    Emissive,
    Occlusion,
    Clearcoat,
    ClearcoatRoughness,
    ClearcoatNormal,
    SheenColor,
    SheenRoughness,
    Specular,
    SpecularColor,
    Iridescence,
    IridescenceThickness,
    Anisotropy,
}

impl MapSlot {
    pub const COUNT: usize = 15;
    pub const ALL: [Self; Self::COUNT] = [
        Self::BaseColor,
        Self::MetallicRoughness,
        Self::Normal,
        Self::Emissive,
        Self::Occlusion,
        Self::Clearcoat,
        Self::ClearcoatRoughness,
        Self::ClearcoatNormal,
        Self::SheenColor,
        Self::SheenRoughness,
        Self::Specular,
        Self::SpecularColor,
        Self::Iridescence,
        Self::IridescenceThickness,
        Self::Anisotropy,
    ];
}

/// `KHR_texture_transform` of one texture reference, applied to its texture
/// coordinates as `offset + rotation * (scale * uv)`. A positive rotation turns the
/// coordinates counter-clockwise (the image clockwise), as in the extension's examples:
/// `(u, v)` becomes `(cos * u + sin * v, cos * v - sin * u)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UvTransform {
    pub offset: [f32; 2],
    /// Radians.
    pub rotation: f32,
    pub scale: [f32; 2],
}

impl Default for UvTransform {
    fn default() -> Self {
        Self {
            offset: [0.0; 2],
            rotation: 0.0,
            scale: [1.0; 2],
        }
    }
}

impl UvTransform {
    /// Row-major affine map: `uv' = [rows[0] . (u, v, 1), rows[1] . (u, v, 1)]`.
    pub fn rows(self) -> [[f32; 3]; 2] {
        let (sin, cos) = self.rotation.sin_cos();
        let [sx, sy] = self.scale;
        [
            [cos * sx, sin * sy, self.offset[0]],
            [-sin * sx, cos * sy, self.offset[1]],
        ]
    }
}

#[derive(Clone, Debug, Default)]
pub struct MaterialMaps {
    /// sRGB RGB, linear alpha.
    pub base_color: Option<Arc<TextureData>>,
    /// Linear G=roughness, B=metallic.
    pub metallic_roughness: Option<Arc<TextureData>>,
    /// Linear tangent-space normal.
    pub normal: Option<Arc<TextureData>>,
    /// sRGB RGB.
    pub emissive: Option<Arc<TextureData>>,
    /// Linear R=occlusion.
    pub occlusion: Option<Arc<TextureData>>,
    /// Linear R=clearcoat intensity.
    pub clearcoat: Option<Arc<TextureData>>,
    /// Linear G=clearcoat roughness.
    pub clearcoat_roughness: Option<Arc<TextureData>>,
    /// Linear tangent-space normal of the clearcoat; without it the coat is not
    /// normal-mapped, even when the base is.
    pub clearcoat_normal: Option<Arc<TextureData>>,
    /// sRGB RGB=sheen colour.
    pub sheen_color: Option<Arc<TextureData>>,
    /// Linear A=sheen roughness.
    pub sheen_roughness: Option<Arc<TextureData>>,
    /// Linear A=specular strength.
    pub specular: Option<Arc<TextureData>>,
    /// sRGB RGB=specular colour.
    pub specular_color: Option<Arc<TextureData>>,
    /// Linear R=iridescence intensity.
    pub iridescence: Option<Arc<TextureData>>,
    /// Linear G=thin-film thickness between the two thickness factors.
    pub iridescence_thickness: Option<Arc<TextureData>>,
    /// Linear RG=anisotropy direction in tangent space (`2 * v - 1`), B=strength.
    pub anisotropy: Option<Arc<TextureData>>,
    /// Texture-coordinate transform of each slot, indexed by [`MapSlot`] order.
    pub transforms: [UvTransform; MapSlot::COUNT],
}

impl MaterialMaps {
    pub fn get(&self, slot: MapSlot) -> &Option<Arc<TextureData>> {
        match slot {
            MapSlot::BaseColor => &self.base_color,
            MapSlot::MetallicRoughness => &self.metallic_roughness,
            MapSlot::Normal => &self.normal,
            MapSlot::Emissive => &self.emissive,
            MapSlot::Occlusion => &self.occlusion,
            MapSlot::Clearcoat => &self.clearcoat,
            MapSlot::ClearcoatRoughness => &self.clearcoat_roughness,
            MapSlot::ClearcoatNormal => &self.clearcoat_normal,
            MapSlot::SheenColor => &self.sheen_color,
            MapSlot::SheenRoughness => &self.sheen_roughness,
            MapSlot::Specular => &self.specular,
            MapSlot::SpecularColor => &self.specular_color,
            MapSlot::Iridescence => &self.iridescence,
            MapSlot::IridescenceThickness => &self.iridescence_thickness,
            MapSlot::Anisotropy => &self.anisotropy,
        }
    }

    pub fn get_mut(&mut self, slot: MapSlot) -> &mut Option<Arc<TextureData>> {
        match slot {
            MapSlot::BaseColor => &mut self.base_color,
            MapSlot::MetallicRoughness => &mut self.metallic_roughness,
            MapSlot::Normal => &mut self.normal,
            MapSlot::Emissive => &mut self.emissive,
            MapSlot::Occlusion => &mut self.occlusion,
            MapSlot::Clearcoat => &mut self.clearcoat,
            MapSlot::ClearcoatRoughness => &mut self.clearcoat_roughness,
            MapSlot::ClearcoatNormal => &mut self.clearcoat_normal,
            MapSlot::SheenColor => &mut self.sheen_color,
            MapSlot::SheenRoughness => &mut self.sheen_roughness,
            MapSlot::Specular => &mut self.specular,
            MapSlot::SpecularColor => &mut self.specular_color,
            MapSlot::Iridescence => &mut self.iridescence,
            MapSlot::IridescenceThickness => &mut self.iridescence_thickness,
            MapSlot::Anisotropy => &mut self.anisotropy,
        }
    }

    pub fn transform(&self, slot: MapSlot) -> UvTransform {
        self.transforms[slot as usize]
    }
}
