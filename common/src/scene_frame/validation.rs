use std::collections::HashSet;

use super::*;

fn require(condition: bool, message: &'static str) -> Result<(), ValidationError> {
    if condition {
        Ok(())
    } else {
        Err(ValidationError(message))
    }
}

fn unique<'a, T: Eq + std::hash::Hash + 'a>(
    values: impl Iterator<Item = &'a T>,
) -> Result<(), ValidationError> {
    let mut seen = HashSet::new();
    require(
        values.into_iter().all(|value| seen.insert(value)),
        "duplicate identity",
    )
}

fn clipping(near: Finite, far: Finite) -> Result<(), ValidationError> {
    require(
        near.get() > 0.0 && near.get() < far.get(),
        "require 0 < near_m < far_m",
    )
}

impl Validate for Pinhole {
    fn validate(&self) -> Result<(), ValidationError> {
        let (Self::IntrinsicsPose {
            extent,
            near_m,
            far_m,
            ..
        }
        | Self::TiC {
            extent,
            near_m,
            far_m,
            ..
        }) = self;
        extent.validate()?;
        clipping(*near_m, *far_m)
    }
}

impl Validate for CameraMode {
    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Manual { camera } => camera.validate(),
            Self::Tracked { near_m, far_m } => clipping(*near_m, *far_m),
        }
    }
}

impl Validate for Instance {
    fn validate(&self) -> Result<(), ValidationError> {
        unique(self.material_overrides.iter().map(|v| &v.part))
    }
}

impl Validate for Lighting {
    fn validate(&self) -> Result<(), ValidationError> {
        unique(self.lights.iter().map(Light::light_id))
    }
}

impl Validate for SceneState {
    fn validate(&self) -> Result<(), ValidationError> {
        self.camera.validate()?;
        self.lighting.validate()?;
        unique(self.instances.iter().map(|v| &v.instance_id))?;
        self.instances.iter().try_for_each(Validate::validate)
    }
}

impl Validate for SceneCreate {
    fn validate(&self) -> Result<(), ValidationError> {
        self.state.validate()
    }
}

impl Validate for SceneApplied {
    fn validate(&self) -> Result<(), ValidationError> {
        require(
            self.runtime_id == self.effect.runtime_id,
            "effect runtime mismatch",
        )
    }
}

impl Validate for SceneSnapshot {
    fn validate(&self) -> Result<(), ValidationError> {
        require(
            self.runtime_id == self.effect.runtime_id,
            "effect runtime mismatch",
        )?;
        self.state.validate()
    }
}

impl Validate for SceneMutate {
    fn validate(&self) -> Result<(), ValidationError> {
        require(!self.operations.is_empty(), "mutation must be nonempty")?;
        self.operations
            .iter()
            .try_for_each(|operation| match operation {
                SceneOperation::InstancePut { instance } => instance.validate(),
                SceneOperation::LightingSet { lighting } => lighting.validate(),
                SceneOperation::CameraSet { camera } => camera.validate(),
                _ => Ok(()),
            })
    }
}

impl Validate for ImageDescription {
    fn validate(&self) -> Result<(), ValidationError> {
        self.extent.validate()?;
        require(
            self.format != ImageFormat::Rgba16Float || self.transfer == Transfer::Linear,
            "float images require linear transfer",
        )
    }
}

impl Validate for FramePublish {
    fn validate(&self) -> Result<(), ValidationError> {
        require(
            self.timeline.timeline_id == self.position.timeline_id,
            "timeline mismatch",
        )?;
        self.image.validate()
    }
}

impl Validate for Submit {
    fn validate(&self) -> Result<(), ValidationError> {
        require(!self.frames.is_empty(), "submission must be nonempty")?;
        unique(self.frames.iter().map(|v| &v.frame))?;
        self.frames
            .iter()
            .try_for_each(|frame| match &frame.geometry {
                FrameGeometry::SceneCamera {} => Ok(()),
                FrameGeometry::TiC { extent, .. } => extent.validate(),
            })
    }
}

impl Validate for SubmissionAccepted {
    fn validate(&self) -> Result<(), ValidationError> {
        require(
            self.runtime_id == self.effect.runtime_id,
            "effect runtime mismatch",
        )?;
        require(!self.frames.is_empty(), "receipt must be nonempty")?;
        unique(self.frames.iter().map(|v| &v.frame))
    }
}

impl Validate for SubmissionSnapshot {
    fn validate(&self) -> Result<(), ValidationError> {
        require(
            self.runtime_id == self.effect.runtime_id,
            "effect runtime mismatch",
        )?;
        require(!self.frames.is_empty(), "receipt must be nonempty")?;
        unique(self.frames.iter().map(FrameResult::frame))
    }
}

impl Validate for CancelResult {
    fn validate(&self) -> Result<(), ValidationError> {
        unique(self.cancelled.iter().chain(self.too_late.iter()))
    }
}

impl Validate for Configuration {
    fn validate(&self) -> Result<(), ValidationError> {
        self.native_extent.validate()?;
        self.output_extent.validate()?;
        require(
            matches!(self.sample_count, 1 | 4),
            "unsupported sample count",
        )?;
        let s = &self.shadows;
        require(s.resolution > 0, "shadow resolution must be positive")?;
        require(
            matches!(s.pcf_samples, 1 | 4 | 9 | 16 | 25 | 36 | 49 | 64),
            "unsupported PCF samples",
        )?;
        require(
            s.max_bytes.get() > 0
                && s.max_samples_per_fragment.get() > 0
                && self.environment.max_bytes.get() > 0
                && self.max_intermediate_bytes.get() > 0,
            "budgets must be positive",
        )?;
        require(
            (1..=64).contains(&s.pcss.blocker_samples)
                && (1..=64).contains(&s.pcss.filter_samples)
                && s.pcss.max_search_radius.get() > 0.0
                && s.pcss.max_filter_radius.get() > 0.0,
            "unsupported PCSS settings",
        )?;
        require(
            (1..=4).contains(&s.cascades.count)
                && s.cascades.max_distance.get() > 0.0
                && (0.0..=1.0).contains(&s.cascades.split_lambda.get())
                && (0.0..=0.5).contains(&s.cascades.blend_fraction.get()),
            "unsupported cascade settings",
        )?;
        let env = &self.environment;
        require(
            (64..=1024).contains(&env.resolution)
                && env.resolution.is_power_of_two()
                && (1..=1024).contains(&env.samples),
            "unsupported environment settings",
        )?;
        let l = &self.limits;
        require(
            [
                l.scenes,
                l.instances_per_scene,
                l.lights_per_scene,
                l.operations_per_mutation,
                l.published_frames,
                l.frames_per_submission,
                l.inflight_frames,
                l.retained_submissions,
                l.render_timeout_ms,
            ]
            .iter()
            .all(|v| *v > 0),
            "limits must be positive",
        )
    }
}

macro_rules! shape_only {
    ($($ty:ty),* $(,)?) => {$(
        impl Validate for $ty {
            fn validate(&self) -> Result<(), ValidationError> { Ok(()) }
        }
    )*};
}
shape_only!(
    RuntimeTarget,
    Target,
    ImageTarget,
    SubmissionTarget,
    PublishedFrame,
    ReleaseResult,
    FrameKey,
    Timeline,
    Position,
    Effect,
    Version,
    Id,
    U64,
    TimeBase
);
