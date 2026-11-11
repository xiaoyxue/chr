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
object!(Instance {
    instance_id: Id, asset_ref: Id, model: Matrix4, visible: bool,
    material_overrides: Vec<MaterialOverride>,
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
    visible: bool
});
object!(Lighting {
    lights: Vec<Light>,
    #[serde(deserialize_with = "required_nullable")]
    environment: Option<Environment>,
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
    },
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
