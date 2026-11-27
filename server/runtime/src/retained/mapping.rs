use std::{collections::HashMap, sync::Arc};

use chr_common::{jsonrpc::ErrorObject, scene_frame as w};
use chr_renderer::{
    CascadeSettings, EnvironmentSettings, PbrSettings, PcssSettings, RenderSettings, ShadowMode,
    ShadowSettings, ToneMap,
    camera::{Camera, Intrinsics},
    composition::{AlphaMode, CompositionOutput, ImageFit, ImageStage, Transfer},
    coordinates::{ImageExtent, Ti},
    distortion::FrameWarp,
    glb::GlbAsset,
    scene::{self, EnvironmentMap, ModelMatrix, Scene, SceneView},
};
use w::Validate;

use super::{incompatible, motion::Motion, reject};

/// An animated instance: its neutral model and its motion.
pub(super) type Animated = (scene::InstanceId, ModelMatrix, Motion);

/// A flag that appears over `duration` seconds: its instance and its fully shown
/// flag. A cued flag waits for a cue of its scene instance `cue`; the others
/// start with the scene.
#[derive(Clone, Debug)]
pub(super) struct Revealing {
    pub id: scene::InstanceId,
    pub flag: scene::Flag,
    pub duration: f64,
    pub cue: Option<w::Id>,
}

impl Revealing {
    /// The flag `seconds` after its scene started, with the cue start times of its
    /// scene's instances.
    pub fn at(&self, seconds: f64, cues: &HashMap<w::Id, f64>) -> scene::Flag {
        let start = match &self.cue {
            None => Some(0.0),
            Some(instance) => cues.get(instance).copied(),
        };
        let revealed = start.map_or(0.0, |start| {
            ((seconds - start) / self.duration).clamp(0.0, 1.0)
        });
        scene::Flag {
            revealed: revealed as f32,
            ..self.flag
        }
    }
}

/// A bound scene's view with its camera and the instances that change per frame.
pub(super) type Prepared = (SceneView, Option<Camera>, Vec<Animated>, Vec<Revealing>);
use chr_tracker::pose::rows3;

/// Immutable native binding for a declared calibration context. No calibration is inferred.
#[derive(Clone, Debug)]
pub struct CameraContext {
    pub world: w::Version,
    pub extent: w::Extent,
    pub camera_center: w::Vec3,
}

/// Startup registration; afterwards only models may be added, through the owner
/// (`SceneAdapter::register_model`), never removed or replaced. Models are imported
/// GLB assets (already basis-converted) or trusted synthetic meshes constructed
/// directly in native -Z-up coordinates.
#[derive(Default)]
pub struct Registrations {
    models: HashMap<w::Id, GlbAsset>,
    environments: HashMap<w::Id, Arc<EnvironmentMap>>,
    worlds: Vec<w::Version>,
    contexts: HashMap<w::Id, CameraContext>,
}

impl Registrations {
    pub fn register_world(&mut self, world: w::Version) -> Result<(), ErrorObject> {
        if self.worlds.contains(&world) {
            return Err(reject(
                w::RejectionKind::Conflict,
                "world already registered",
            ));
        }
        self.worlds.push(world);
        Ok(())
    }

    pub fn register_model(&mut self, id: w::Id, asset: GlbAsset) -> Result<(), ErrorObject> {
        self.available_asset(&id)?;
        if asset.materials.len() != asset.mesh.parts().len() {
            return Err(incompatible("model requires one material per part"));
        }
        let mut check = Scene::new().map_err(incompatible)?;
        for material in &asset.materials {
            check
                .add_material_with_maps(material.factors, material.maps.clone())
                .map_err(incompatible)?;
        }
        self.models.insert(id, asset);
        Ok(())
    }

    pub fn register_environment(
        &mut self,
        id: w::Id,
        environment: EnvironmentMap,
    ) -> Result<(), ErrorObject> {
        self.available_asset(&id)?;
        self.environments.insert(id, Arc::new(environment));
        Ok(())
    }

    fn available_asset(&self, id: &w::Id) -> Result<(), ErrorObject> {
        if self.models.contains_key(id) || self.environments.contains_key(id) {
            return Err(reject(
                w::RejectionKind::Conflict,
                "asset already registered",
            ));
        }
        Ok(())
    }

    pub fn register_context(
        &mut self,
        id: w::Id,
        context: CameraContext,
    ) -> Result<(), ErrorObject> {
        self.world(&context.world)?;
        extent(&context.extent)?;
        if context.camera_center[2].get() == 0.0 {
            return Err(incompatible("camera context is on the ground plane"));
        }
        if self.contexts.contains_key(&id) {
            return Err(reject(
                w::RejectionKind::Conflict,
                "context already registered",
            ));
        }
        self.contexts.insert(id, context);
        Ok(())
    }

    fn world(&self, world: &w::Version) -> Result<(), ErrorObject> {
        if !self.worlds.contains(world) {
            return Err(reject(
                w::RejectionKind::NotFound,
                "world version is not registered",
            ));
        }
        Ok(())
    }

    pub(super) fn prepare(
        &self,
        config: &w::Configuration,
        state: &w::SceneState,
    ) -> Result<Prepared, ErrorObject> {
        state.validate().map_err(incompatible)?;
        self.world(&state.world)?;
        if state.configuration_id != config.configuration_id {
            return Err(incompatible(
                "scene configuration differs from installed configuration",
            ));
        }
        if state.instances.len() > config.limits.instances_per_scene as usize
            || state.lighting.lights.len() > config.limits.lights_per_scene as usize
        {
            return Err(reject(
                w::RejectionKind::Capacity,
                "scene exceeds configured limits",
            ));
        }
        let camera = match &state.camera {
            w::CameraMode::Manual { camera } => Some(pinhole(config, camera)?),
            w::CameraMode::Tracked { .. } => None,
        };
        let mut scene = Scene::new().map_err(incompatible)?;
        let mut meshes = HashMap::new();
        let mut animated = Vec::new();
        let mut revealing = Vec::new();
        for instance in &state.instances {
            let asset = self.models.get(&instance.asset_ref).ok_or_else(|| {
                reject(w::RejectionKind::NotFound, "model asset is not registered")
            })?;
            if asset.mesh.is_flag_grid() != instance.flag.is_some() {
                return Err(incompatible(
                    "flag assets need flag parameters, which other assets do not take",
                ));
            }
            let model = ModelMatrix::new(instance.model.map(|r| r.map(w::Finite::get)))
                .map_err(incompatible)?;
            for n in model
                .rows()
                .iter()
                .flatten()
                .chain(model.normal_matrix().iter().flatten())
            {
                float(*n)?;
            }
            let mesh = match meshes.get(&instance.asset_ref) {
                Some(id) => *id,
                None => {
                    let id = scene.add_mesh(asset.mesh.clone()).map_err(incompatible)?;
                    meshes.insert(instance.asset_ref.clone(), id);
                    id
                }
            };
            let mut materials = asset.materials.clone();
            if instance
                .flag
                .as_ref()
                .is_some_and(|flag| flag.material == w::FlagMaterial::Cloth)
            {
                materials.iter_mut().for_each(|m| cloth(&mut m.factors));
            }
            for replacement in &instance.material_overrides {
                let entry = materials
                    .get_mut(replacement.part as usize)
                    .ok_or_else(|| incompatible("material override part is absent"))?;
                let (decal, layers) = (entry.factors.decal, entry.factors.layers);
                entry.factors = factors(&replacement.factors)?;
                entry.factors.decal = decal;
                entry.factors.layers = layers;
            }
            let materials = materials
                .into_iter()
                .map(|m| {
                    scene
                        .add_material_with_maps(m.factors, m.maps)
                        .map_err(incompatible)
                })
                .collect::<Result<_, _>>()?;
            let id = scene
                .add_instance(mesh, materials, model)
                .map_err(incompatible)?;
            scene
                .set_visible(id, instance.visible)
                .map_err(incompatible)?;
            if let Some(flag) = &instance.flag {
                let named = |e: ErrorObject| {
                    incompatible(format!(
                        "instance {}: {}",
                        instance.instance_id.as_str(),
                        e.message
                    ))
                };
                let native = native_flag(flag).map_err(named)?;
                let shown = match &flag.reveal {
                    None => native,
                    Some(reveal) => {
                        let duration = reveal.duration_s.get();
                        if duration <= 0.0 {
                            return Err(named(incompatible("reveal duration_s must be above 0")));
                        }
                        let native = scene::Flag {
                            reveal_direction: reveal_direction(reveal.direction),
                            ..native
                        };
                        revealing.push(Revealing {
                            id,
                            flag: native,
                            duration,
                            cue: (reveal.start == w::RevealStart::Cue)
                                .then(|| instance.instance_id.clone()),
                        });
                        scene::Flag {
                            revealed: 0.0,
                            ..native
                        }
                    }
                };
                scene.set_flag(id, Some(shown)).map_err(incompatible)?;
            }
            if let Some(motion) = &instance.motion {
                let motion = Motion::new(motion).map_err(|e| {
                    incompatible(format!("instance {}: {e}", instance.instance_id.as_str()))
                })?;
                animated.push((id, model, motion));
            }
        }
        let lights = state
            .lighting
            .lights
            .iter()
            .map(light)
            .collect::<Result<Vec<_>, _>>()?;
        let sky = state
            .lighting
            .environment
            .as_ref()
            .map(|sky| {
                let environment = self.environments.get(&sky.asset_ref).ok_or_else(|| {
                    reject(
                        w::RejectionKind::NotFound,
                        "environment asset is not registered",
                    )
                })?;
                let e = environment.extent();
                let resolution = u128::from(config.environment.resolution);
                let bytes = (u128::from(e.width()) * u128::from(e.height())
                    + resolution * (resolution / 2) * 6
                    + 64 * 64)
                    * 16;
                if bytes > u128::from(config.environment.max_bytes.get()) {
                    return Err(reject(
                        w::RejectionKind::Capacity,
                        "environment memory budget exceeded",
                    ));
                }
                nonnegative(sky.gain)?;
                Ok(scene::SkyLight {
                    environment: Arc::clone(environment),
                    gain: f(sky.gain)?,
                    rotation: sky.rotation.get(),
                    visible: sky.visible,
                    max_radiance: sky
                        .max_radiance
                        .map(|cap| {
                            if cap.get() <= 0.0 {
                                return Err(incompatible(
                                    "environment max_radiance must be positive",
                                ));
                            }
                            f(cap)
                        })
                        .transpose()?,
                })
            })
            .transpose()?;
        let mut lighting = scene::Lighting::new(lights, sky).map_err(incompatible)?;
        if let Some(constant) = state.lighting.constant {
            for v in constant {
                nonnegative(v)?;
            }
            lighting = lighting
                .with_constant(v3(constant)?)
                .map_err(incompatible)?;
        }
        scene.set_lighting(lighting).map_err(incompatible)?;
        shadow_budget(config, state)?;
        Ok((scene.snapshot(), camera, animated, revealing))
    }

    pub(super) fn frame_camera(
        &self,
        config: &w::Configuration,
        state: &w::SceneState,
        manual: Option<&Camera>,
        geometry: &w::FrameGeometry,
    ) -> Result<Camera, ErrorObject> {
        match (&state.camera, geometry) {
            (w::CameraMode::Manual { .. }, w::FrameGeometry::SceneCamera {}) => manual
                .cloned()
                .ok_or_else(|| incompatible("manual camera was not prepared")),
            (
                w::CameraMode::Tracked { near_m, far_m },
                w::FrameGeometry::TiC {
                    world,
                    context_id,
                    extent,
                    ti,
                    camera_center,
                    lens,
                    frame_warp,
                },
            ) => {
                if let Some(lens) = lens {
                    crate::calibration::lens::radial(lens).map_err(incompatible)?;
                }
                if let Some(rows) = frame_warp {
                    if lens.is_none() {
                        return Err(reject(
                            w::RejectionKind::UnsupportedProfile,
                            "frame_warp requires a lens",
                        ));
                    }
                    FrameWarp::new(rows3(rows)).map_err(incompatible)?;
                }
                if world != &state.world {
                    return Err(incompatible("frame world differs from bound scene"));
                }
                if let Some(id) = context_id {
                    let context = self.contexts.get(id).ok_or_else(|| {
                        reject(
                            w::RejectionKind::NotFound,
                            "camera context is not registered",
                        )
                    })?;
                    if context.world != *world
                        || context.extent != *extent
                        || context.camera_center != *camera_center
                    {
                        return Err(incompatible("frame differs from immutable camera context"));
                    }
                }
                pinhole(
                    config,
                    &w::Pinhole::TiC {
                        extent: *extent,
                        near_m: *near_m,
                        far_m: *far_m,
                        ti: *ti,
                        camera_center: *camera_center,
                    },
                )
            }
            _ => Err(incompatible(
                "frame geometry does not match scene camera mode",
            )),
        }
    }
}

/// Fully mapped CPU settings. Device support must additionally be checked by the installed port.
#[derive(Clone, Copy, Debug)]
pub struct NativeSettings {
    pub output: CompositionOutput,
    pub raster: RenderSettings,
    pub input_stage: ImageStage,
}

impl NativeSettings {
    pub(super) fn new(config: &w::Configuration, mode: w::ShadowMode) -> Result<Self, ErrorObject> {
        config.validate().map_err(incompatible)?;
        let s = &config.shadows;
        nonnegative(config.exposure)?;
        nonnegative(s.filter_radius)?;
        nonnegative(s.normal_bias)?;
        unit(s.depth_bias)?;
        let pbr = PbrSettings {
            exposure: f(config.exposure)?,
            tone_map: match config.tone_map {
                w::ToneMap::Aces => ToneMap::Aces,
                w::ToneMap::Reinhard => ToneMap::Reinhard,
            },
            shadows: ShadowSettings {
                mode: shadow(mode),
                resolution: s.resolution,
                filter_radius: f(s.filter_radius)?,
                pcf_samples: s.pcf_samples,
                depth_bias: f(s.depth_bias)?,
                normal_bias: f(s.normal_bias)?,
                max_bytes: s.max_bytes.get(),
                max_samples_per_fragment: s.max_samples_per_fragment.get(),
                pcss: PcssSettings {
                    blocker_samples: s.pcss.blocker_samples,
                    filter_samples: s.pcss.filter_samples,
                    max_search_radius: f(s.pcss.max_search_radius)?,
                    max_filter_radius: f(s.pcss.max_filter_radius)?,
                },
                cascades: CascadeSettings {
                    count: s.cascades.count,
                    max_distance: s.cascades.max_distance.get(),
                    split_lambda: s.cascades.split_lambda.get(),
                    blend_fraction: s.cascades.blend_fraction.get(),
                },
            },
            environment: EnvironmentSettings {
                resolution: config.environment.resolution,
                samples: config.environment.samples,
                max_bytes: config.environment.max_bytes.get(),
            },
        };
        if pbr.shadows.pcss.max_search_radius <= 0.0 || pbr.shadows.pcss.max_filter_radius <= 0.0 {
            return Err(incompatible("PCSS radius underflows native float"));
        }
        let native = extent(&config.native_extent)?;
        let output = extent(&config.output_extent)?;
        let pixels = u128::from(native.width()) * u128::from(native.height())
            + u128::from(output.width()) * u128::from(output.height());
        if pixels * 8 > u128::from(config.max_intermediate_bytes.get()) {
            return Err(reject(
                w::RejectionKind::Capacity,
                "composition canvases exceed memory budget",
            ));
        }
        Ok(Self {
            output: CompositionOutput {
                extent: output,
                fit: match config.fit {
                    w::Fit::Contain => ImageFit::Contain,
                    w::Fit::Cover => ImageFit::Cover,
                    w::Fit::Stretch => ImageFit::Stretch,
                },
                transfer: match config.output_transfer {
                    w::OutputTransfer::Srgb => Transfer::Srgb,
                    w::OutputTransfer::Bt709 => Transfer::Bt709,
                },
                alpha: AlphaMode::Straight,
                clear_color: [0.0, 0.0, 0.0, 1.0],
                pbr,
                max_intermediate_bytes: config.max_intermediate_bytes.get(),
            },
            raster: RenderSettings {
                sample_count: config.sample_count,
                ..RenderSettings::default()
            },
            input_stage: match config.input_stage {
                w::InputStage::SceneBackground => ImageStage::SceneBackground,
                w::InputStage::DisplayBackground => ImageStage::DisplayBackground,
            },
        })
    }
}

fn extent(e: &w::Extent) -> Result<ImageExtent, ErrorObject> {
    ImageExtent::new(e.width, e.height).map_err(incompatible)
}

fn pinhole(config: &w::Configuration, input: &w::Pinhole) -> Result<Camera, ErrorObject> {
    let camera = match input {
        w::Pinhole::IntrinsicsPose {
            extent: e,
            near_m,
            far_m,
            intrinsics: k,
            camera_to_world,
        } => {
            if e != &config.native_extent {
                return Err(incompatible("camera extent differs from configuration"));
            }
            Camera::from_pose(
                extent(e)?,
                Intrinsics {
                    fx: k.fx.get(),
                    fy: k.fy.get(),
                    skew: k.skew.get(),
                    cx: k.cx.get(),
                    cy: k.cy.get(),
                },
                camera_to_world.map(|r| r.map(w::Finite::get)),
                near_m.get(),
                far_m.get(),
            )
        }
        w::Pinhole::TiC {
            extent: e,
            near_m,
            far_m,
            ti,
            camera_center,
        } => {
            if e != &config.native_extent {
                return Err(incompatible("camera extent differs from configuration"));
            }
            let ti = Ti::new(rows3(ti)).map_err(incompatible)?;
            Camera::from_ti(
                extent(e)?,
                ti,
                camera_center.map(w::Finite::get),
                near_m.get(),
                far_m.get(),
            )
        }
    }
    .map_err(incompatible)?;
    camera.clip_from_world().map_err(incompatible)?;
    camera
        .center()
        .into_iter()
        .try_for_each(|v| float(v).map(|_| ()))?;
    Ok(camera)
}

fn float(n: f64) -> Result<f32, ErrorObject> {
    let mapped = n as f32;
    if !mapped.is_finite() {
        return Err(incompatible("value exceeds native GPU float range"));
    }
    Ok(mapped)
}
fn f(n: w::Finite) -> Result<f32, ErrorObject> {
    float(n.get())
}
fn native_flag(flag: &w::Flag) -> Result<scene::Flag, ErrorObject> {
    Ok(scene::Flag {
        width: f(flag.width_m)?,
        height: f(flag.height_m)?,
        amplitude: f(flag.amplitude_m)?,
        frequency: f(flag.frequency)?,
        speed: f(flag.speed)?,
        shading: match flag.material {
            w::FlagMaterial::Simple => scene::FlagShading::Simple,
            w::FlagMaterial::Pbr | w::FlagMaterial::Cloth => scene::FlagShading::Pbr,
        },
        artwork_turns: match flag.artwork_rotation.unwrap_or(0) {
            0 => 0,
            90 => 1,
            180 => 2,
            270 => 3,
            _ => {
                return Err(incompatible(
                    "flag artwork_rotation must be 0, 90, 180 or 270",
                ));
            }
        },
        revealed: 1.0,
        reveal_direction: scene::RevealDirection::CenterHorizontal,
    })
}
fn reveal_direction(direction: w::RevealDirection) -> scene::RevealDirection {
    match direction {
        w::RevealDirection::LeftToRight => scene::RevealDirection::LeftToRight,
        w::RevealDirection::RightToLeft => scene::RevealDirection::RightToLeft,
        w::RevealDirection::TopToBottom => scene::RevealDirection::TopToBottom,
        w::RevealDirection::BottomToTop => scene::RevealDirection::BottomToTop,
        w::RevealDirection::CenterHorizontal => scene::RevealDirection::CenterHorizontal,
        w::RevealDirection::CenterVertical => scene::RevealDirection::CenterVertical,
    }
}
/// A flag's `cloth` look: PBR fabric, matte with a soft sheen at grazing angles
/// (`KHR_materials_sheen`) and a weak dielectric highlight.
fn cloth(factors: &mut scene::PbrMaterial) {
    factors.roughness = 0.9;
    factors.layers.specular = 0.3;
    factors.layers.sheen_color = [0.45; 3];
    factors.layers.sheen_roughness = 0.5;
}
fn v3(v: w::Vec3) -> Result<[f32; 3], ErrorObject> {
    Ok([f(v[0])?, f(v[1])?, f(v[2])?])
}
fn factors(v: &w::PbrFactors) -> Result<scene::PbrMaterial, ErrorObject> {
    for value in v
        .base_color
        .into_iter()
        .chain([v.metallic, v.roughness, v.occlusion_strength])
        .chain(v.alpha_cutoff)
    {
        unit(value)?;
    }
    for value in v.emissive {
        nonnegative(value)?;
    }
    Ok(scene::PbrMaterial {
        base_color: [
            f(v.base_color[0])?,
            f(v.base_color[1])?,
            f(v.base_color[2])?,
            f(v.base_color[3])?,
        ],
        metallic: f(v.metallic)?,
        roughness: f(v.roughness)?,
        emissive: v3(v.emissive)?,
        alpha_cutoff: v.alpha_cutoff.map(f).transpose()?,
        double_sided: v.double_sided,
        normal_scale: f(v.normal_scale)?,
        occlusion_strength: f(v.occlusion_strength)?,
        unlit: v.unlit,
        // The shared wire material has no shadow-catcher field yet; adding one is a
        // protocol change for the owning contracts.
        shadow_catcher: false,
        // Decals are an asset's role, kept by the caller across factor overrides.
        decal: false,
        // Holdouts are grounds the runtime builds, never wire materials.
        holdout: false,
        // glTF extension layers belong to the asset, kept by the caller like decals.
        layers: scene::MaterialLayers::default(),
    })
}
fn shadow(mode: w::ShadowMode) -> ShadowMode {
    match mode {
        w::ShadowMode::Off => ShadowMode::Off,
        w::ShadowMode::Hard => ShadowMode::Hard,
        w::ShadowMode::Pcf => ShadowMode::Pcf,
        w::ShadowMode::Pcss => ShadowMode::Pcss,
    }
}
fn light(input: &w::Light) -> Result<scene::Light, ErrorObject> {
    match input {
        w::Light::Directional {
            direction,
            irradiance,
            angular_radius,
            ..
        } => {
            direction_length(*direction)?;
            for v in irradiance {
                nonnegative(*v)?;
            }
            if !(0.0..std::f64::consts::FRAC_PI_2).contains(&angular_radius.get()) {
                return Err(incompatible(
                    "directional angular radius is outside its physical range",
                ));
            }
        }
        w::Light::Point {
            intensity,
            source_radius,
            ..
        } => {
            for v in intensity {
                nonnegative(*v)?;
            }
            nonnegative(*source_radius)?;
        }
        w::Light::Spot {
            direction,
            intensity,
            source_radius,
            ..
        } => {
            direction_length(*direction)?;
            for v in intensity {
                nonnegative(*v)?;
            }
            nonnegative(*source_radius)?;
        }
    }
    Ok(match input {
        w::Light::Directional {
            direction,
            irradiance,
            angular_radius,
            shadow_mode,
            ..
        } => scene::Light::Directional {
            direction: direction.map(w::Finite::get),
            irradiance: v3(*irradiance)?,
            angular_radius: f(*angular_radius)?,
            shadow_mode: shadow_mode.map(shadow),
        },
        w::Light::Point {
            position,
            intensity,
            source_radius,
            shadow_mode,
            ..
        } => {
            v3(*position)?;
            scene::Light::Point {
                position: position.map(w::Finite::get),
                intensity: v3(*intensity)?,
                source_radius: f(*source_radius)?,
                shadow_mode: shadow_mode.map(shadow),
            }
        }
        w::Light::Spot {
            position,
            direction,
            intensity,
            source_radius,
            inner_angle,
            outer_angle,
            shadow_mode,
            ..
        } => {
            v3(*position)?;
            scene::Light::Spot {
                position: position.map(w::Finite::get),
                direction: direction.map(w::Finite::get),
                intensity: v3(*intensity)?,
                source_radius: f(*source_radius)?,
                inner_angle: inner_angle.get(),
                outer_angle: outer_angle.get(),
                shadow_mode: shadow_mode.map(shadow),
            }
        }
    })
}

fn unit(value: w::Finite) -> Result<(), ErrorObject> {
    if !(0.0..=1.0).contains(&value.get()) {
        return Err(incompatible("factor must be in [0, 1]"));
    }
    Ok(())
}

fn nonnegative(value: w::Finite) -> Result<(), ErrorObject> {
    if value.get() < 0.0 {
        return Err(incompatible("physical value must be nonnegative"));
    }
    Ok(())
}

fn direction_length(direction: w::Vec3) -> Result<(), ErrorObject> {
    let length_squared = direction.iter().map(|v| v.get() * v.get()).sum::<f64>();
    if !length_squared.is_finite() || length_squared <= 0.0 {
        return Err(incompatible("light direction cannot be normalized"));
    }
    Ok(())
}

fn shadow_budget(config: &w::Configuration, state: &w::SceneState) -> Result<(), ErrorObject> {
    let mut layers = 0_u128;
    let mut samples = 0_u128;
    for light in &state.lighting.lights {
        let (mode, count, blend) = match light {
            w::Light::Directional { shadow_mode, .. } => (
                *shadow_mode,
                config.shadows.cascades.count,
                config.shadows.cascades.count > 1
                    && config.shadows.cascades.blend_fraction.get() > 0.0,
            ),
            w::Light::Point { shadow_mode, .. } => (*shadow_mode, 6, false),
            w::Light::Spot { shadow_mode, .. } => (*shadow_mode, 1, false),
        };
        let cost = match mode.unwrap_or(state.shadow_mode) {
            w::ShadowMode::Off => continue,
            w::ShadowMode::Hard => 1,
            w::ShadowMode::Pcf => config.shadows.pcf_samples,
            w::ShadowMode::Pcss => {
                config.shadows.pcss.blocker_samples + config.shadows.pcss.filter_samples
            }
        };
        layers += u128::from(count);
        samples += u128::from(cost) * if blend { 2 } else { 1 };
    }
    let resolution = u128::from(config.shadows.resolution);
    if resolution * resolution * 4 * layers > u128::from(config.shadows.max_bytes.get())
        || samples > u128::from(config.shadows.max_samples_per_fragment.get())
    {
        return Err(reject(
            w::RejectionKind::Capacity,
            "shadow memory or sample budget exceeded",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveals_start_with_the_scene_or_with_their_latest_cue() {
        let flag = scene::Flag {
            width: 4.0,
            height: 2.0,
            amplitude: 0.3,
            frequency: 1.35,
            speed: 1.0,
            shading: scene::FlagShading::Simple,
            artwork_turns: 1,
            revealed: 1.0,
            reveal_direction: scene::RevealDirection::BottomToTop,
        };
        let id = |name: &str| serde_json::from_value::<w::Id>(serde_json::json!(name)).unwrap();
        let mut scene = Scene::new().unwrap();
        let grid = scene.add_mesh(scene::MeshData::flag_grid()).unwrap();
        let material = scene.add_material(scene::PbrMaterial::default()).unwrap();
        let instance = scene
            .add_instance(grid, vec![material], ModelMatrix::IDENTITY)
            .unwrap();
        let revealing = |cue: Option<w::Id>| Revealing {
            id: instance,
            flag,
            duration: 2.0,
            cue,
        };
        let revealed =
            |r: &Revealing, seconds, cues: &HashMap<w::Id, f64>| r.at(seconds, cues).revealed;
        let none = HashMap::new();
        let loaded = revealing(None);
        assert_eq!(
            [0.0, 0.5, 2.0, 9.0].map(|t| revealed(&loaded, t, &none)),
            [0.0, 0.25, 1.0, 1.0]
        );
        let cued = revealing(Some(id("tifo")));
        assert_eq!(revealed(&cued, 9.0, &none), 0.0, "hidden until cued");
        let cues = HashMap::from([(id("tifo"), 3.0), (id("other"), 0.0)]);
        assert_eq!(
            [2.0, 4.0, 5.0].map(|t| revealed(&cued, t, &cues)),
            [0.0, 0.5, 1.0]
        );
        assert_eq!(cued.at(4.0, &cues).artwork_turns, 1);
        assert_eq!(
            cued.at(4.0, &cues).reveal_direction,
            scene::RevealDirection::BottomToTop
        );
    }
}
