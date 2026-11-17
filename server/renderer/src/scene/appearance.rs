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
    /// Invisible receiver for compositing over captured footage: in shaded PBR views it
    /// draws no colour and darkens whatever lies behind it (the clear colour or, through
    /// composition, a background image) by its luminance ratio of shadowed to unshadowed
    /// light. It still occludes and casts shadows like the ground it stands in for.
    pub shadow_catcher: bool,
    /// Ground decal, such as a painted or projected advertisement: in shaded PBR views
    /// it is drawn unlit and blended over the opaque scene with premultiplied
    /// alpha (base colour alpha times texture alpha), after all opaque geometry and
    /// before shadow catchers, in instance order. It writes no depth and casts no
    /// shadow, so shadow catchers drawn over it darken it like the ground beneath.
    pub decal: bool,
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
            shadow_catcher: false,
            decal: false,
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
            || (self.decal && self.shadow_catcher)
        {
            return Err(SceneError::InvalidMaterial);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum ShadowMode {
    #[default]
    Off = 0,
    Hard = 1,
    Pcf = 2,
    Pcss = 3,
}

/// CPU light inputs for the shared PBR path.
/// Positions are world metres; directions describe light travel and need not be unit length.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Light {
    Directional {
        direction: [f64; 3],
        /// Linear RGB W/m^2 on a plane perpendicular to the light.
        irradiance: [f32; 3],
        /// Angular source radius in radians for PCSS; zero is an ideal directional source.
        angular_radius: f32,
        /// None inherits the frame default; Some(Off) disables only this light's shadows.
        shadow_mode: Option<ShadowMode>,
    },
    Point {
        position: [f64; 3],
        /// Linear RGB W/sr.
        intensity: [f32; 3],
        /// Shadow-source radius in metres; does not change punctual BRDF evaluation.
        source_radius: f32,
        /// None inherits the frame default; Some(Off) disables only this light's shadows.
        shadow_mode: Option<ShadowMode>,
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
        /// None inherits the frame default; Some(Off) disables only this light's shadows.
        shadow_mode: Option<ShadowMode>,
    },
    /// Experimental uniform, isotropic finite emitter shaded with the owner-approved
    /// `ltc-plausible-v1` approximation profile (see `server/docs/area-light-evaluation.md`).
    /// Shadows use one centre cubemap with PCSS sized by `AreaShape::equivalent_radius`:
    /// an owner-approved approximation whose penumbra ignores the emitter's outline.
    Area {
        /// Emitter centre in world metres.
        position: [f64; 3],
        /// Front-face direction of planar emitters; ignored by spheres. Need not be unit length.
        normal: [f64; 3],
        /// Rectangle width axis, projected perpendicular to `normal`; must not be parallel to it.
        tangent: [f64; 3],
        shape: AreaShape,
        /// Linear RGB radiance in W/(m^2 sr), uniform over the emitting surface.
        radiance: [f32; 3],
        /// Planar emitters also emit from their back face. Spheres emit outward only.
        two_sided: bool,
        /// None inherits the frame default; Some(Off) disables only this light's shadows.
        /// Casters inside a sphere emitter are ignored, so an emissive mesh representing it
        /// casts no shadow; a planar emitter's mesh must lie in or behind its plane.
        shadow_mode: Option<ShadowMode>,
    },
}

/// Experimental finite emitter geometry in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AreaShape {
    /// Half extents along `tangent` and `normal x tangent`.
    Rectangle {
        half_width: f32,
        half_height: f32,
    },
    Disk {
        radius: f32,
    },
    /// Outward-emitting sphere.
    Sphere {
        radius: f32,
    },
}

impl AreaShape {
    /// Emitting surface area in square metres.
    pub fn area(self) -> f64 {
        match self {
            Self::Rectangle {
                half_width,
                half_height,
            } => 4.0 * f64::from(half_width) * f64::from(half_height),
            Self::Disk { radius } => std::f64::consts::PI * f64::from(radius).powi(2),
            Self::Sphere { radius } => 4.0 * std::f64::consts::PI * f64::from(radius).powi(2),
        }
    }

    /// Radiance giving total emitted power `flux` (linear RGB watts), so size changes can
    /// keep power fixed instead of radiance: `flux = pi * area * radiance` per emitting side.
    pub fn radiance_from_flux(self, flux: [f32; 3], two_sided: bool) -> [f32; 3] {
        let sides = if two_sided && !matches!(self, Self::Sphere { .. }) {
            2.0
        } else {
            1.0
        };
        flux.map(|f| (f64::from(f) / (std::f64::consts::PI * self.area() * sides)) as f32)
    }

    /// PCSS source radius in metres: the radius of a disk with the same area for
    /// rectangles, otherwise the shape radius.
    pub fn equivalent_radius(self) -> f32 {
        match self {
            Self::Rectangle {
                half_width,
                half_height,
            } => (4.0 * half_width * half_height / std::f32::consts::PI).sqrt(),
            Self::Disk { radius } | Self::Sphere { radius } => radius,
        }
    }

    fn valid(self) -> bool {
        let positive = |v: f32| v.is_finite() && v > 0.0;
        match self {
            Self::Rectangle {
                half_width,
                half_height,
            } => positive(half_width) && positive(half_height),
            Self::Disk { radius } | Self::Sphere { radius } => positive(radius),
        }
    }
}

impl Light {
    pub(crate) fn effective_shadow_mode(self, default: ShadowMode) -> ShadowMode {
        match self {
            Self::Directional { shadow_mode, .. }
            | Self::Point { shadow_mode, .. }
            | Self::Spot { shadow_mode, .. }
            | Self::Area { shadow_mode, .. } => shadow_mode.unwrap_or(default),
        }
    }

    fn validate(self) -> Result<(), SceneError> {
        let valid = match self {
            Self::Directional {
                direction,
                irradiance,
                angular_radius,
                ..
            } => {
                valid_direction(direction)
                    && nonnegative(irradiance)
                    && (0.0..std::f32::consts::FRAC_PI_2).contains(&angular_radius)
            }
            Self::Point {
                position,
                intensity,
                source_radius,
                ..
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
                ..
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
            Self::Area {
                position,
                normal,
                tangent,
                shape,
                radiance,
                two_sided,
                ..
            } => {
                position.into_iter().all(f64::is_finite)
                    && valid_direction(normal)
                    && valid_direction(tangent)
                    && !parallel(normal, tangent)
                    && shape.valid()
                    && nonnegative(radiance)
                    && !(two_sided && matches!(shape, AreaShape::Sphere { .. }))
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
/// +X maps to UV (0.5, 0.5), +Y to (0.75, 0.5), and upward -Z to V=0.
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

/// No implicit ambient fill or fixed key/fill/rim rig; a constant fill is explicit.
#[derive(Clone, Debug, Default)]
pub struct Lighting {
    lights: Arc<[Light]>,
    sky: Option<SkyLight>,
    constant: [f32; 3],
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
            constant: [0.0; 3],
        })
    }

    /// Adds uniform, unoccluded radiance from every direction (linear RGB
    /// W/(m^2 sr)) to the sky's: a constant fill shaded like a uniform environment.
    pub fn with_constant(mut self, radiance: [f32; 3]) -> Result<Self, SceneError> {
        if !nonnegative(radiance) {
            return Err(SceneError::InvalidLighting);
        }
        self.constant = radiance;
        Ok(self)
    }

    pub fn constant(&self) -> [f32; 3] {
        self.constant
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

// Rejects nearly parallel axes whose perpendicular tangent would be ill-conditioned.
// Callers pass finite, nonzero directions, so the scaled lengths are finite.
fn parallel(a: [f64; 3], b: [f64; 3]) -> bool {
    let scaled = |v: [f64; 3]| {
        let scale = v.into_iter().map(f64::abs).fold(0.0, f64::max);
        v.map(|x| x / scale)
    };
    let (a, b) = (scaled(a), scaled(b));
    let cross = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let length = |v: [f64; 3]| v.into_iter().map(|x| x * x).sum::<f64>().sqrt();
    length(cross) <= 1e-6 * length(a) * length(b)
}

fn valid_direction(direction: [f64; 3]) -> bool {
    direction.into_iter().all(f64::is_finite) && direction != [0.0; 3]
}
