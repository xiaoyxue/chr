use serde::{Deserialize, Serialize};

use super::{
    core::{integer, required_nullable},
    *,
};

macro_rules! strings {
    ($name:ident { $($variant:ident => $wire:literal),* $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $wire)] $variant),* }
    };
}

strings!(Profile { SceneFrameV1 => "scene_frame_v1" });
strings!(ShadowMode { Off => "off", Hard => "hard", Pcf => "pcf", Pcss => "pcss" });
strings!(Fit { Contain => "contain", Cover => "cover", Stretch => "stretch" });
strings!(InputStage { SceneBackground => "scene_background", DisplayBackground => "display_background" });
strings!(OutputTransfer { Srgb => "srgb", Bt709 => "bt709" });
strings!(ToneMap { Aces => "aces", Reinhard => "reinhard" });
strings!(ImageFormat { Rgba8Unorm => "rgba8_unorm", Bgra8Unorm => "bgra8_unorm", Rgba16Float => "rgba16_float" });
strings!(Transfer { Linear => "linear", Srgb => "srgb", Bt709 => "bt709" });
strings!(Alpha { Straight => "straight", Premultiplied => "premultiplied" });
strings!(FailureKind {
    WriteFailed => "write_failed", DeadlineExceeded => "deadline_exceeded",
    Cancelled => "cancelled", RenderFailed => "render_failed", RuntimeFailed => "runtime_failed",
});
strings!(DropReason { QueueFull => "queue_full", OutputFailed => "output_failed" });

object!(RuntimeTarget { runtime_id: Id });
object!(Target {
    runtime_id: Id,
    scene_id: Id
});
object!(ImageTarget {
    runtime_id: Id,
    image_ref: Id
});
object!(SubmissionTarget {
    runtime_id: Id,
    submission_id: Id
});
object!(Pcss {
    #[serde(deserialize_with = "integer")]
    blocker_samples: u32,
    #[serde(deserialize_with = "integer")]
    filter_samples: u32,
    max_search_radius: Finite,
    max_filter_radius: Finite,
});
object!(Cascades {
    #[serde(deserialize_with = "integer")]
    count: u32,
    max_distance: Finite,
    split_lambda: Finite,
    blend_fraction: Finite
});
object!(Shadows {
    #[serde(deserialize_with = "integer")]
    resolution: u32,
    filter_radius: Finite,
    #[serde(deserialize_with = "integer")]
    pcf_samples: u32,
    depth_bias: Finite,
    normal_bias: Finite,
    max_bytes: U64,
    max_samples_per_fragment: U64,
    pcss: Pcss,
    cascades: Cascades,
});
object!(EnvironmentSettings {
    #[serde(deserialize_with = "integer")]
    resolution: u32,
    #[serde(deserialize_with = "integer")]
    samples: u32,
    max_bytes: U64
});
object!(Limits {
    #[serde(deserialize_with = "integer")]
    scenes: u32,
    #[serde(deserialize_with = "integer")]
    instances_per_scene: u32,
    #[serde(deserialize_with = "integer")]
    lights_per_scene: u32,
    #[serde(deserialize_with = "integer")]
    operations_per_mutation: u32,
    #[serde(deserialize_with = "integer")]
    published_frames: u32,
    #[serde(deserialize_with = "integer")]
    frames_per_submission: u32,
    #[serde(deserialize_with = "integer")]
    inflight_frames: u32,
    #[serde(deserialize_with = "integer")]
    retained_submissions: u32,
    #[serde(deserialize_with = "integer")]
    render_timeout_ms: u32,
});
object!(Configuration {
    runtime_id: Id,
    configuration_id: Id,
    profile: Profile,
    native_extent: Extent,
    output_extent: Extent,
    output_rate: TimeBase,
    fit: Fit,
    input_stage: InputStage,
    output_transfer: OutputTransfer,
    exposure: Finite,
    tone_map: ToneMap,
    #[serde(deserialize_with = "integer")]
    sample_count: u32,
    shadows: Shadows,
    environment: EnvironmentSettings,
    max_intermediate_bytes: U64,
    limits: Limits,
});

object!(PbrFactors {
    base_color: Vec4, metallic: Finite, roughness: Finite, emissive: Vec3,
    #[serde(deserialize_with = "required_nullable")]
    alpha_cutoff: Option<Finite>,
    double_sided: bool, normal_scale: Finite, occlusion_strength: Finite, unlit: bool,
});
object!(MaterialOverride {
    #[serde(deserialize_with = "integer")]
    part: u32,
    factors: PbrFactors
});
strings!(FlagMaterial { Simple => "simple", Pbr => "pbr", Cloth => "cloth" });
strings!(FlagType { Default => "default", Native => "native" });

/// Flag motion of an instance whose asset is a flag. The flag-local frame is metres:
/// X along the width from the left edge, Y down from the top edge and Z their cross
/// product; the instance `model` maps it to the world and should be rigid. The
/// artwork reads upright from the -Z side and mirrored from the other.
///
/// On the wire, `type` selects the motion (absent, as it is written for `default`)
/// and only that motion's fields are accepted: `amplitude_m`, `frequency` and `speed` for `default`;
/// `wind_mps`, `wind_direction_deg`, `auto_direction`, `tension`, `slack` and `raise`
/// for `native`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "FlagFields", into = "FlagFields")]
pub struct Flag {
    pub width_m: Finite,
    pub height_m: Finite,
    pub motion: FlagMotion,
    pub material: FlagMaterial,
    /// Clockwise turn of the artwork within the flag as it reads, in degrees: 0, 90,
    /// 180 or 270. The flag's size stays; the artwork stretches to it.
    pub artwork_rotation: Option<u32>,
    /// How the flag appears; absent, it is shown from the start.
    pub reveal: Option<FlagReveal>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlagMotion {
    /// `default`: the closed-form SimpleWave.
    Wave(FlagWave),
    /// `native`: a physically based cloth simulation.
    Native(FlagCloth),
}

/// SimpleWave: the centre line stays pinned and both halves wave outwards. Absent
/// fields take a tifo's wave, as in pitch-studio (`TIFO_FLAG`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlagWave {
    /// Wave coefficient, not the peak displacement (at most 1.53 times this); 1.1.
    pub amplitude_m: Finite,
    /// Primary wave cycles from the centre line to either free edge; 1.35.
    pub frequency: Finite,
    /// Time scale of the wave; 5.5.
    pub speed: Finite,
}

/// A cloth pinned along its top (Y = 0) and bottom (Y = `height_m`) edges with free
/// sides, longer than that span by its `slack` so it billows, blown by a gusting
/// wind. Absent fields take the reference simulation's defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlagCloth {
    /// Wind speed in m/s, 0 to 18; 7.
    pub wind_mps: Finite,
    /// Base direction the wind blows towards, in degrees about the flag's Y axis:
    /// 0 along +X (towards the right edge as the artwork reads) and 90 along -Z
    /// (towards the side that reads it); 25.
    pub wind_direction_deg: Finite,
    /// Turns the wind smoothly to a new target within 45° of the base direction
    /// every 0.5 s; true.
    pub auto_direction: bool,
    /// Bending stiffness from 0 (soft and flowing) to 1 (crisp and firm); 0.65.
    pub tension: Finite,
    /// The part of the cloth's length the pinned edges do not span, 0 (taut) to 0.2;
    /// 0.1, a cloth 11 % longer than `height_m`.
    pub slack: Finite,
    /// How the flag is raised; absent, it hangs raised from the start.
    pub raise: Option<FlagRaise>,
}

object!(
    /// A native flag raised over `duration_s` seconds (finite, above 0), as the
    /// reference simulation's lift: the cloth starts folded just above its pinned
    /// bottom edge and its top edge rises to Y = 0 on a smoothstep, then stays. `load`
    /// starts with the scene; `cue` keeps it folded until a cue starts it
    /// (`description.cue_reveal`), and a repeated cue folds and raises it again.
    #[derive(Copy)]
    FlagRaise {
    duration_s: Finite,
    start: RevealStart,
});

object!(@private FlagFields {
    width_m: Finite, height_m: Finite,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<FlagType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    amplitude_m: Option<Finite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frequency: Option<Finite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    speed: Option<Finite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wind_mps: Option<Finite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wind_direction_deg: Option<Finite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto_direction: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tension: Option<Finite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    slack: Option<Finite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raise: Option<FlagRaise>,
    /// `cloth`.
    #[serde(default = "cloth_material")]
    material: FlagMaterial,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artwork_rotation: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reveal: Option<FlagReveal>,
});

fn cloth_material() -> FlagMaterial {
    FlagMaterial::Cloth
}

impl Flag {
    /// Whether a cue starts this flag's reveal or raise.
    pub fn cued(&self) -> bool {
        let raise = match &self.motion {
            FlagMotion::Native(cloth) => cloth.raise.as_ref(),
            FlagMotion::Wave(_) => None,
        };
        self.reveal
            .as_ref()
            .map(|reveal| reveal.start)
            .into_iter()
            .chain(raise.map(|raise| raise.start))
            .any(|start| start == RevealStart::Cue)
    }
}

impl TryFrom<FlagFields> for Flag {
    type Error = ValidationError;
    fn try_from(f: FlagFields) -> Result<Self, Self::Error> {
        let or = |value: Option<Finite>, default: f64| {
            value.unwrap_or_else(|| Finite::try_from(default).expect("a finite literal"))
        };
        let wave = [f.amplitude_m, f.frequency, f.speed];
        let cloth = [f.wind_mps, f.wind_direction_deg, f.tension, f.slack];
        let motion = match f.kind.unwrap_or(FlagType::Default) {
            FlagType::Default
                if cloth.iter().any(Option::is_some)
                    || f.auto_direction.is_some()
                    || f.raise.is_some() =>
            {
                return Err(ValidationError(
                    "wind, tension, slack and raise are for native flags",
                ));
            }
            FlagType::Native if wave.iter().any(Option::is_some) => {
                return Err(ValidationError(
                    "amplitude, frequency and speed are for default flags",
                ));
            }
            FlagType::Default => FlagMotion::Wave(FlagWave {
                amplitude_m: or(f.amplitude_m, 1.1),
                frequency: or(f.frequency, 1.35),
                speed: or(f.speed, 5.5),
            }),
            FlagType::Native => FlagMotion::Native(FlagCloth {
                wind_mps: or(f.wind_mps, 7.0),
                wind_direction_deg: or(f.wind_direction_deg, 25.0),
                auto_direction: f.auto_direction.unwrap_or(true),
                tension: or(f.tension, 0.65),
                slack: or(f.slack, 0.1),
                raise: f.raise,
            }),
        };
        Ok(Self {
            width_m: f.width_m,
            height_m: f.height_m,
            motion,
            material: f.material,
            artwork_rotation: f.artwork_rotation,
            reveal: f.reveal,
        })
    }
}

impl From<Flag> for FlagFields {
    fn from(flag: Flag) -> Self {
        let mut fields = Self {
            width_m: flag.width_m,
            height_m: flag.height_m,
            kind: None,
            amplitude_m: None,
            frequency: None,
            speed: None,
            wind_mps: None,
            wind_direction_deg: None,
            auto_direction: None,
            tension: None,
            slack: None,
            raise: None,
            material: flag.material,
            artwork_rotation: flag.artwork_rotation,
            reveal: flag.reveal,
        };
        match flag.motion {
            FlagMotion::Wave(wave) => {
                fields.amplitude_m = Some(wave.amplitude_m);
                fields.frequency = Some(wave.frequency);
                fields.speed = Some(wave.speed);
            }
            FlagMotion::Native(cloth) => {
                fields.kind = Some(FlagType::Native);
                fields.wind_mps = Some(cloth.wind_mps);
                fields.wind_direction_deg = Some(cloth.wind_direction_deg);
                fields.auto_direction = Some(cloth.auto_direction);
                fields.tension = Some(cloth.tension);
                fields.slack = Some(cloth.slack);
                fields.raise = cloth.raise;
            }
        }
        fields
    }
}
strings!(RevealDirection {
    LeftToRight => "left_to_right", RightToLeft => "right_to_left",
    TopToBottom => "top_to_bottom", BottomToTop => "bottom_to_top",
    CenterHorizontal => "center_horizontal", CenterVertical => "center_vertical",
});
strings!(RevealStart { Load => "load", Cue => "cue" });
object!(
    /// A flag appearing over `duration_s` seconds (finite, above 0): at progress
    /// `p = t / duration_s` it shows the part of its rectangle that `direction` has
    /// opened, in flag-local `x / width` and `y / height` (from the top-left corner,
    /// whatever the artwork rotation): `left_to_right` `x <= p`, `right_to_left`
    /// `x >= 1 - p`, `top_to_bottom` `y <= p`, `bottom_to_top` `y >= 1 - p`,
    /// `center_horizontal` `|2 x - 1| <= p` and `center_vertical` `|2 y - 1| <= p`.
    /// `load` starts with the scene (the motion's time zero); `cue` hides the flag
    /// until a cue starts it (`description.cue_reveal`).
    FlagReveal {
    direction: RevealDirection,
    duration_s: Finite,
    start: RevealStart,
});
strings!(RotationMode { Swing => "swing", Continuous => "continuous" });
strings!(Playback { Once => "once", Loop => "loop", PingPong => "ping_pong" });
object!(
    /// Rotation of a whole instance about a fixed world axis through the motion's pivot.
    MotionRotation {
    /// World direction of the axis; it need not be unit length. Positive angles turn
    /// right-handed about it (about +Z, from +X towards +Y).
    axis: Vec3,
    mode: RotationMode,
    /// `swing`: radians on each side of the neutral pose, `amplitude * sin(2 pi t / period)`,
    /// in (0, pi]. Ignored by `continuous`, which turns a full circle every period.
    amplitude: Finite,
    /// Seconds, 0.1 to 60.
    period: Finite,
});
object!(
    /// One end of a motion path: a world offset of the pivot in metres and a uniform
    /// scale multiplier of the neutral pose.
    MotionKey { offset: Vec3, scale: Finite });
object!(
    /// Linear motion from `start` to `end` over `duration` seconds. `once` holds the end,
    /// `loop` restarts at the start, `ping_pong` returns over another `duration`.
    MotionPath {
    start: MotionKey, end: MotionKey, duration: Finite, playback: Playback,
});
object!(
    /// Rigid animation of a whole instance (#157). At a time `t` the instance's model is
    /// `T(pivot + offset(t)) * scale(t) * R(axis, angle(t)) * T(-pivot) * model`: the
    /// path moves the pivot in fixed world axes, independent of heading and rotation,
    /// and scale and rotation act about the pivot. `t` counts seconds from the first
    /// frame rendered with this scene.
    Motion {
    /// World point that rotation and scale act about, such as a model's bottom centre.
    pivot: Vec3,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rotation: Option<MotionRotation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<MotionPath>,
});
object!(Instance {
    instance_id: Id, asset_ref: Id, model: Matrix4, visible: bool,
    material_overrides: Vec<MaterialOverride>,
    /// Required for flag assets and rejected for every other asset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flag: Option<Flag>,
    /// Rigid animation of the whole instance; boxed, as few instances move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    motion: Option<Box<Motion>>,
});

// Keep common fields on each variant: flatten + deny_unknown_fields is not a
// supported serde combination and can silently weaken exact wire schemas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Light {
    Directional {
        light_id: Id,
        #[serde(deserialize_with = "required_nullable")]
        shadow_mode: Option<ShadowMode>,
        direction: Vec3,
        irradiance: Vec3,
        angular_radius: Finite,
    },
    Point {
        light_id: Id,
        #[serde(deserialize_with = "required_nullable")]
        shadow_mode: Option<ShadowMode>,
        position: Vec3,
        intensity: Vec3,
        source_radius: Finite,
    },
    Spot {
        light_id: Id,
        #[serde(deserialize_with = "required_nullable")]
        shadow_mode: Option<ShadowMode>,
        position: Vec3,
        direction: Vec3,
        intensity: Vec3,
        source_radius: Finite,
        inner_angle: Finite,
        outer_angle: Finite,
    },
}

impl Light {
    pub fn light_id(&self) -> &Id {
        match self {
            Self::Directional { light_id, .. }
            | Self::Point { light_id, .. }
            | Self::Spot { light_id, .. } => light_id,
        }
    }
}

object!(Environment {
    asset_ref: Id,
    gain: Finite,
    rotation: Finite,
    visible: bool,
    /// Largest radiance taken from the panorama, linear RGB W/(m^2 sr) before
    /// `gain`; brighter texels are clamped. Absent means none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_radiance: Option<Finite>,
});
object!(Lighting {
    lights: Vec<Light>,
    #[serde(deserialize_with = "required_nullable")]
    environment: Option<Environment>,
    /// Uniform, unoccluded radiance from every direction, linear RGB W/(m^2 sr),
    /// added to the environment's; absent means none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    constant: Option<Vec3>,
});
object!(Intrinsics {
    fx: Finite,
    fy: Finite,
    skew: Finite,
    cx: Finite,
    cy: Finite
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Pinhole {
    IntrinsicsPose {
        extent: Extent,
        near_m: Finite,
        far_m: Finite,
        intrinsics: Intrinsics,
        camera_to_world: Matrix4,
    },
    TiC {
        extent: Extent,
        near_m: Finite,
        far_m: Finite,
        ti: Matrix3,
        camera_center: Vec3,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum CameraMode {
    Manual { camera: Pinhole },
    Tracked { near_m: Finite, far_m: Finite },
}

object!(SceneState {
    world: Version, configuration_id: Id, camera: CameraMode,
    instances: Vec<Instance>, lighting: Lighting, shadow_mode: ShadowMode,
});
object!(SceneCreate {
    runtime_id: Id,
    scene_id: Id,
    state: SceneState
});
object!(SceneSnapshot {
    runtime_id: Id,
    scene_id: Id,
    scene_revision: U64,
    effect: Effect,
    state: SceneState,
});
object!(SceneApplied {
    runtime_id: Id,
    scene_id: Id,
    scene_revision: U64,
    effect: Effect
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SceneOperation {
    InstancePut { instance: Instance },
    InstanceRemove { instance_id: Id },
    LightingSet { lighting: Lighting },
    ShadowModeSet { shadow_mode: ShadowMode },
    CameraSet { camera: Pinhole },
}

object!(SceneMutate {
    runtime_id: Id, scene_id: Id, expected_revision: U64, operations: Vec<SceneOperation>,
});
object!(ImageDescription {
    extent: Extent,
    format: ImageFormat,
    transfer: Transfer,
    alpha: Alpha
});
object!(FramePublish {
    runtime_id: Id,
    producer_ticket: Id,
    frame: FrameKey,
    timeline: Timeline,
    position: Position,
    image: ImageDescription,
});
object!(PublishedFrame {
    runtime_id: Id,
    frame: FrameKey,
    image_ref: Id,
    position: Position
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FrameGeometry {
    SceneCamera {},
    TiC {
        world: Version,
        #[serde(deserialize_with = "required_nullable")]
        context_id: Option<Id>,
        extent: Extent,
        ti: Matrix3,
        camera_center: Vec3,
        /// Opt-in `radial-poly-native-px-v1` lens of this frame. Absent means the
        /// pinhole camera `S^-1 Ti A(C)`; omitted when absent, so the pinhole
        /// profile's messages are unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lens: Option<Box<RadialLens>>,
        /// Opt-in `radial-poly-native-px-v1` frame warp: row-major, reference native
        /// pixel to this frame's SNC. Only accepted with a `lens`; absent means
        /// `W = S`. Omitted when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame_warp: Option<Box<Matrix3>>,
    },
}

strings!(LensModel { RadialPolyNativePxV1 => "radial-poly-native-px-v1" });

/// A `radial-poly-native-px-v1` lens in native pixels:
/// `r = centre + (a - centre) * (1 + k1 p^2 + k2 p^4 + k3 p^6)` with
/// `p = |a - centre| / scale_px`, where `a` is the ideal pixel of `S^-1 Ti A(C)`.
/// The runtime also requires a positive scale and rejects a support rectangle
/// that is empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadialLens {
    pub model: LensModel,
    pub centre_px: [Finite; 2],
    pub scale_px: Finite,
    pub k: [Finite; 3],
    /// Validated ideal-pixel rectangle `[[min_x, min_y], [max_x, max_y]]`;
    /// outside it, results are extrapolation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support: Option<[[Finite; 2]; 2]>,
}

object!(FrameInput {
    frame: FrameKey,
    image_ref: Id,
    position: Position,
    geometry: FrameGeometry
});
object!(Submit {
    runtime_id: Id, scene_id: Id, submission_id: Id,
    #[serde(deserialize_with = "required_nullable")]
    expected_scene_revision: Option<U64>,
    frames: Vec<FrameInput>,
});
object!(AcceptedFrame {
    frame: FrameKey,
    position: Position
});
object!(SubmissionAccepted {
    runtime_id: Id, submission_id: Id, scene_id: Id,
    scene_revision: U64, effect: Effect, frames: Vec<AcceptedFrame>,
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Delivery {
    Queued {},
    Transmitted {
        output_stream_id: Id,
        first_slot: U64,
    },
    Dropped {
        reason: DropReason,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum FrameResult {
    Pending {
        frame: FrameKey,
        position: Position,
    },
    Failed {
        frame: FrameKey,
        position: Position,
        reason: FailureKind,
    },
    Rendered {
        frame: FrameKey,
        position: Position,
        output_frame_id: Id,
        delivery: Delivery,
    },
}

impl FrameResult {
    pub fn frame(&self) -> &FrameKey {
        match self {
            Self::Pending { frame, .. }
            | Self::Failed { frame, .. }
            | Self::Rendered { frame, .. } => frame,
        }
    }
}

object!(SubmissionSnapshot {
    runtime_id: Id, submission_id: Id, scene_id: Id,
    scene_revision: U64, effect: Effect, frames: Vec<FrameResult>,
});
object!(CancelResult { runtime_id: Id, submission_id: Id, cancelled: Vec<FrameKey>, too_late: Vec<FrameKey> });
object!(ReleaseResult { released: bool });
