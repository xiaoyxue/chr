use std::sync::Arc;

use crate::coordinates::ImageExtent;

use super::SceneError;

/// Scene-linear material factors. Texture bindings and GPU BRDF evaluation are separate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PbrMaterial {
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    pub alpha_cutoff: Option<f32>,
    pub double_sided: bool,
    pub normal_scale: f32,
    pub occlusion_strength: f32,
    pub unlit: bool,
}

impl Default for PbrMaterial {
    fn default() -> Self {
        Self {
            base_color: [1.0; 4],
            metallic: 0.0,
            roughness: 0.5,
            emissive: [0.0; 3],
            alpha_cutoff: None,
            double_sided: false,
            normal_scale: 1.0,
            occlusion_strength: 1.0,
            unlit: false,
        }
    }
}

impl PbrMaterial {
    pub(crate) fn validate(self) -> Result<(), SceneError> {
        if !self
            .base_color
            .into_iter()
            .chain([self.metallic, self.roughness, self.occlusion_strength])
            .chain(self.alpha_cutoff)
            .all(|v| (0.0..=1.0).contains(&v))
            || !nonnegative(self.emissive)
            || !self.normal_scale.is_finite()
        {
            return Err(SceneError::InvalidMaterial);
        }
        Ok(())
    }
}

/// CPU light inputs, not a promise that the current triangle shader evaluates them.
/// Positions are world metres; directions describe light travel and need not be unit length.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Light {
    Directional {
        direction: [f64; 3],
        /// Linear RGB W/m^2 on a plane perpendicular to the light.
        irradiance: [f32; 3],
        /// Angular source radius in radians for PCSS; zero is an ideal directional source.
        angular_radius: f32,
    },
    Point {
        position: [f64; 3],
        /// Linear RGB W/sr.
        intensity: [f32; 3],
        /// Shadow-source radius in metres; does not change punctual BRDF evaluation.
        source_radius: f32,
    },
    Spot {
        position: [f64; 3],
        direction: [f64; 3],
        /// Linear RGB W/sr before cone attenuation.
        intensity: [f32; 3],
        /// Half angles in radians: 0 <= inner < outer < pi/2.
        inner_angle: f64,
        outer_angle: f64,
        /// Shadow-source radius in metres; does not change punctual BRDF evaluation.
        source_radius: f32,
    },
}

impl Light {
    fn validate(self) -> Result<(), SceneError> {
        let valid = match self {
            Self::Directional {
                direction,
                irradiance,
                angular_radius,
            } => {
                valid_direction(direction)
                    && nonnegative(irradiance)
                    && (0.0..std::f32::consts::FRAC_PI_2).contains(&angular_radius)
            }
            Self::Point {
                position,
                intensity,
                source_radius,
            } => {
                position.into_iter().all(f64::is_finite)
                    && nonnegative(intensity)
                    && source_radius.is_finite()
                    && source_radius >= 0.0
            }
            Self::Spot {
                position,
                direction,
                intensity,
                inner_angle,
                outer_angle,
                source_radius,
            } => {
                position.into_iter().all(f64::is_finite)
                    && valid_direction(direction)
                    && nonnegative(intensity)
                    && 0.0 <= inner_angle
                    && inner_angle < outer_angle
                    && outer_angle < std::f64::consts::FRAC_PI_2
                    && source_radius.is_finite()
                    && source_radius >= 0.0
            }
        };
        if valid {
            Ok(())
        } else {
            Err(SceneError::InvalidLighting)
        }
    }
}

/// Decoded, scene-linear RGB radiance for an equirectangular environment.
/// OpenEXR decoding and filesystem I/O do not belong in retained scene storage.
#[derive(Clone, Debug)]
pub struct EnvironmentMap {
    extent: ImageExtent,
    pixels: Arc<[[f32; 3]]>,
}

impl EnvironmentMap {
    pub fn new(extent: ImageExtent, pixels: Vec<[f32; 3]>) -> Result<Self, SceneError> {
        let expected = u64::from(extent.width()) * u64::from(extent.height());
        if pixels.len() as u64 != expected || !pixels.iter().copied().all(nonnegative) {
            return Err(SceneError::InvalidEnvironment);
        }
        Ok(Self {
            extent,
            pixels: pixels.into(),
        })
    }

    pub fn extent(&self) -> ImageExtent {
        self.extent
    }

    pub fn pixels(&self) -> &[[f32; 3]] {
        &self.pixels
    }
}

/// One orientation/gain for environment lighting and its optional backdrop.
#[derive(Clone, Debug)]
pub struct SkyLight {
    pub environment: Arc<EnvironmentMap>,
    pub gain: f32,
    /// World yaw about the upward (-Z) axis, in radians.
    pub rotation: f64,
    pub visible: bool,
}

/// No implicit ambient fill or fixed key/fill/rim rig.
#[derive(Clone, Debug, Default)]
pub struct Lighting {
    lights: Arc<[Light]>,
    sky: Option<SkyLight>,
}

impl Lighting {
    pub fn new(lights: Vec<Light>, sky: Option<SkyLight>) -> Result<Self, SceneError> {
        for light in &lights {
            light.validate()?;
        }
        if sky
            .as_ref()
            .is_some_and(|sky| !sky.gain.is_finite() || sky.gain < 0.0 || !sky.rotation.is_finite())
        {
            return Err(SceneError::InvalidLighting);
        }
        Ok(Self {
            lights: lights.into(),
            sky,
        })
    }

    pub fn lights(&self) -> &[Light] {
        &self.lights
    }

    pub fn sky(&self) -> Option<&SkyLight> {
        self.sky.as_ref()
    }
}

fn nonnegative(values: [f32; 3]) -> bool {
    values.into_iter().all(|v| v.is_finite() && v >= 0.0)
}

fn valid_direction(direction: [f64; 3]) -> bool {
    direction.into_iter().all(f64::is_finite) && direction != [0.0; 3]
}
