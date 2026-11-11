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
}
