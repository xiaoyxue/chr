use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

use chr_common::{jsonrpc::ErrorObject, methods::mesh, scene_frame as w};
use chr_renderer::{camera::Camera, scene::SceneView};
use serde_json::Value;

use super::{
    incompatible,
    mapping::{NativeSettings, Registrations},
    reject,
    rotation::{Request as RotationRequest, Rotation},
    value,
};

/// The exact retained revision handed to admission, never a lazy scene-ID lookup.
pub struct BoundScene {
    snapshot: w::SceneSnapshot,
    scene: SceneView,
    camera: Option<Camera>,
    settings: NativeSettings,
    generation: Arc<()>,
}

impl BoundScene {
    pub fn snapshot(&self) -> &w::SceneSnapshot {
        &self.snapshot
    }
    pub fn scene(&self) -> &SceneView {
        &self.scene
    }
    pub fn manual_camera(&self) -> Option<&Camera> {
        self.camera.as_ref()
    }
    pub fn settings(&self) -> NativeSettings {
        self.settings
    }
}

/// Complete, ordered input and camera list, all bound to one immutable scene.
pub struct BoundSubmission {
    pub request: w::Submit,
    pub scene: Arc<BoundScene>,
    pub cameras: Vec<Camera>,
    pub receipt: w::SubmissionAccepted,
}

/// Explicit #22 seam, not a GPU implementation. Every method must be bounded and
/// nonblocking. Admission must validate/acquire every registered image and its
/// metadata, reserve the entire batch, and retain all leases plus `scene` before
/// returning Ok. Err must publish nothing. Readiness/GPU waiting happens later.
///
/// `existing` implements normalized request deduplication BEFORE scene lookup;
/// identical retained retries keep their old binding even after scene release.
/// Different content under a retained submission ID must return Conflict.
/// The port owns result/cancellation/release lifecycle and physical capacity.
pub trait RenderAdmission: Send + 'static {
    fn publish(&mut self, _: w::FramePublish) -> Result<w::PublishedFrame, ErrorObject> {
        Err(reject(
            w::RejectionKind::UnsupportedProfile,
            "no producer adapter installed",
        ))
    }
    fn release_image(&mut self, _: w::ImageTarget) -> Result<w::ReleaseResult, ErrorObject> {
        Err(reject(
            w::RejectionKind::UnsupportedProfile,
            "no producer adapter installed",
        ))
    }
    /// Validate the installed target/device support, or explicit synthetic port limits.
    fn configure(
        &mut self,
        config: &w::Configuration,
        settings: NativeSettings,
    ) -> Result<(), ErrorObject>;
    fn existing(&self, request: &w::Submit) -> Result<Option<w::SubmissionAccepted>, ErrorObject>;
    fn admit(&mut self, submission: BoundSubmission) -> Result<(), ErrorObject>;
    fn get(&self, target: &w::SubmissionTarget) -> Result<w::SubmissionSnapshot, ErrorObject>;
    fn cancel(&mut self, target: &w::SubmissionTarget) -> Result<w::CancelResult, ErrorObject>;
    fn release(&mut self, target: &w::SubmissionTarget) -> Result<w::ReleaseResult, ErrorObject>;
}

/// One authoritative CPU owner. Constructing it explicitly opts into the approved
/// synthetic profile; it neither replaces triangle output nor opens a listener.
pub struct SceneAdapter {
    config: w::Configuration,
    registrations: Registrations,
    scenes: HashMap<w::Id, Arc<BoundScene>>,
    generations: Vec<Weak<()>>,
    sequence: w::U64,
    admission: Box<dyn RenderAdmission>,
    rotation_enabled: bool,
    rotation: Option<Rotation>,
}

impl SceneAdapter {
    pub fn new(
        config: w::Configuration,
        registrations: Registrations,
        mut admission: Box<dyn RenderAdmission>,
    ) -> Result<Self, ErrorObject> {
        let settings = NativeSettings::new(&config, w::ShadowMode::Off)?;
        admission.configure(&config, settings)?;
        Ok(Self {
            config,
            registrations,
            scenes: HashMap::new(),
            generations: Vec::new(),
            sequence: w::U64::from(0),
            admission,
            rotation_enabled: false,
            rotation: None,
        })
    }

    /// Software-video-only animation; ordinary retained profiles remain unchanged.
    pub fn with_rotation(mut self) -> Self {
        self.rotation_enabled = true;
        self
    }

    fn rotation_instance(&self, target: &mesh::RotationTarget) -> Result<w::Instance, ErrorObject> {
        self.runtime(&target.runtime_id)?;
        self.scene(&target.scene_id)?
            .snapshot
            .state
            .instances
            .iter()
            .find(|instance| instance.instance_id == target.instance_id)
            .cloned()
            .ok_or_else(|| reject(w::RejectionKind::NotFound, "rotation instance is absent"))
    }

    pub(super) fn rotate(
        &mut self,
        request: RotationRequest,
        frame: u64,
    ) -> Result<Value, ErrorObject> {
        if !self.rotation_enabled {
            return Err(ErrorObject::method_not_found("mesh rotation"));
        }
        let target = request.target();
        self.rotation_instance(&target)?;
        if let RotationRequest::Start(start) = &request
            && !start.angular_velocity_rad_per_s.is_finite()
        {
            return Err(ErrorObject::invalid_params("rotation speed must be finite"));
        }
        let same = self
            .rotation
            .as_ref()
            .is_some_and(|rotation| rotation.matches(&target));
        if !same
            && self
                .rotation
                .as_ref()
                .is_some_and(|rotation| rotation.state.rotating)
            && !matches!(request, RotationRequest::Get(_))
        {
            return Err(reject(
                w::RejectionKind::Conflict,
                "another instance is rotating",
            ));
        }
        if !matches!(request, RotationRequest::Get(_)) && same {
            self.advance_rotation(frame)?;
        }
        let instance = self.rotation_instance(&target)?;
        match request {
            RotationRequest::Get(_) => {
                if same {
                    value(&self.rotation.as_ref().expect("matching rotation").state)
                } else {
                    value(Rotation::new(target, instance, frame).state)
                }
            }
            RotationRequest::Start(start) => {
                if !same
                    || self
                        .rotation
                        .as_ref()
                        .is_some_and(|rotation| rotation.last != instance)
                {
                    self.rotation = Some(Rotation::new(target, instance, frame));
                }
                let rotation = self.rotation.as_mut().expect("selected rotation");
                rotation.state.rotating = true;
                rotation.state.angular_velocity_rad_per_s = start.angular_velocity_rad_per_s;
                rotation.state.frame = frame;
                value(&rotation.state)
            }
            RotationRequest::Stop(_) => {
                if same {
                    let rotation = self.rotation.as_mut().expect("matching rotation");
                    rotation.state.rotating = false;
                    value(&rotation.state)
                } else {
                    value(Rotation::new(target, instance, frame).state)
                }
            }
        }
    }

    pub(super) fn advance_rotation(&mut self, frame: u64) -> Result<(), ErrorObject> {
        let Some(rotation) = &self.rotation else {
            return Ok(());
        };
        if !rotation.state.rotating || frame <= rotation.state.frame {
            return Ok(());
        }
        let target = mesh::RotationTarget {
            runtime_id: rotation.state.runtime_id.clone(),
            scene_id: rotation.state.scene_id.clone(),
            instance_id: rotation.state.instance_id.clone(),
        };
        // Never overwrite an external instance edit or a released/recreated scene.
        if self.rotation_instance(&target).as_ref().ok() != Some(&rotation.last) {
            self.rotation
                .as_mut()
                .expect("active rotation")
                .state
                .rotating = false;
            return Ok(());
        }
        let (angle, instance) = rotation.pose(frame, self.config.output_rate)?;
        self.mutate(w::SceneMutate {
            runtime_id: target.runtime_id,
            scene_id: target.scene_id.clone(),
            expected_revision: self.scene(&target.scene_id)?.snapshot.scene_revision,
            operations: vec![w::SceneOperation::InstancePut {
                instance: instance.clone(),
            }],
        })?;
        let rotation = self.rotation.as_mut().expect("active rotation");
        rotation.last = instance;
        rotation.state.angle_rad = angle;
        rotation.state.frame = frame;
        Ok(())
    }

    fn runtime(&self, id: &w::Id) -> Result<(), ErrorObject> {
        if id != &self.config.runtime_id {
            return Err(reject(
                w::RejectionKind::RuntimeChanged,
                "runtime identity changed",
            ));
        }
        Ok(())
    }

    fn next_effect(&self) -> Result<w::Effect, ErrorObject> {
        Ok(w::Effect {
            runtime_id: self.config.runtime_id.clone(),
            sequence: self
                .sequence
                .checked_add(1)
                .ok_or_else(|| reject(w::RejectionKind::Capacity, "effect sequence exhausted"))?,
        })
    }

    fn scene(&self, id: &w::Id) -> Result<&Arc<BoundScene>, ErrorObject> {
        self.scenes
            .get(id)
            .ok_or_else(|| reject(w::RejectionKind::NotFound, "scene is not live"))
    }

    /// Rebinds a scene snapshot with this adapter's registrations, as a render
    /// worker does for each request. The snapshot must belong to this runtime.
    pub fn bind(&self, snapshot: w::SceneSnapshot) -> Result<BoundScene, ErrorObject> {
        self.runtime(&snapshot.runtime_id)?;
        self.prepare(
            snapshot.scene_id,
            snapshot.state,
            snapshot.scene_revision,
            snapshot.effect,
            Arc::new(()),
        )
    }

    /// The camera of one submitted frame of `scene`, as admission derives it.
    pub fn frame_camera(
        &self,
        scene: &BoundScene,
        geometry: &w::FrameGeometry,
    ) -> Result<Camera, ErrorObject> {
        self.registrations.frame_camera(
            &self.config,
            &scene.snapshot.state,
            scene.camera.as_ref(),
            geometry,
        )
    }

    fn prepare(
        &self,
        id: w::Id,
        state: w::SceneState,
        revision: w::U64,
        effect: w::Effect,
        generation: Arc<()>,
    ) -> Result<BoundScene, ErrorObject> {
        let (scene, camera) = self.registrations.prepare(&self.config, &state)?;
        let settings = NativeSettings::new(&self.config, state.shadow_mode)?;
        Ok(BoundScene {
            snapshot: w::SceneSnapshot {
                runtime_id: self.config.runtime_id.clone(),
                scene_id: id,
                scene_revision: revision,
                effect,
                state,
            },
            scene,
            camera,
            settings,
            generation,
        })
    }

    fn create(&mut self, request: w::SceneCreate) -> Result<w::SceneApplied, ErrorObject> {
        self.runtime(&request.runtime_id)?;
        if self.scenes.contains_key(&request.scene_id) {
            return Err(reject(w::RejectionKind::Conflict, "scene is already live"));
        }
        self.generations.retain(|g| g.strong_count() != 0);
        if self.generations.len() >= self.config.limits.scenes as usize {
            return Err(reject(
                w::RejectionKind::Capacity,
                "live and draining scene capacity exhausted",
            ));
        }
        let effect = self.next_effect()?;
        let generation = Arc::new(());
        let prepared = self.prepare(
            request.scene_id,
            request.state,
            w::U64::from(0),
            effect,
            Arc::clone(&generation),
        )?;
        self.generations.push(Arc::downgrade(&generation));
        Ok(self.publish(prepared))
    }

    fn publish(&mut self, prepared: BoundScene) -> w::SceneApplied {
        let snapshot = &prepared.snapshot;
        let applied = w::SceneApplied {
            runtime_id: snapshot.runtime_id.clone(),
            scene_id: snapshot.scene_id.clone(),
            scene_revision: snapshot.scene_revision,
            effect: snapshot.effect.clone(),
        };
        self.sequence = applied.effect.sequence;
        self.scenes
            .insert(applied.scene_id.clone(), Arc::new(prepared));
        applied
    }

    fn mutate(&mut self, request: w::SceneMutate) -> Result<w::SceneApplied, ErrorObject> {
        self.runtime(&request.runtime_id)?;
        if request.operations.len() > self.config.limits.operations_per_mutation as usize {
            return Err(reject(
                w::RejectionKind::Capacity,
                "mutation operation limit exceeded",
            ));
        }
        let current = self.scene(&request.scene_id)?;
        if request.expected_revision != current.snapshot.scene_revision {
            return Err(reject(w::RejectionKind::Conflict, "scene revision differs"));
        }
        let revision = current
            .snapshot
            .scene_revision
            .checked_add(1)
            .ok_or_else(|| reject(w::RejectionKind::Capacity, "scene revision exhausted"))?;
        let effect = self.next_effect()?;
        let generation = Arc::clone(&current.generation);
        let mut state = current.snapshot.state.clone();
        let mut prepared = None;
        for operation in request.operations {
            match operation {
                w::SceneOperation::InstancePut { instance } => {
                    if let Some(old) = state
                        .instances
                        .iter_mut()
                        .find(|v| v.instance_id == instance.instance_id)
                    {
                        *old = instance;
                    } else {
                        state.instances.push(instance);
                    }
                }
                w::SceneOperation::InstanceRemove { instance_id } => {
                    let index = state
                        .instances
                        .iter()
                        .position(|v| v.instance_id == instance_id)
                        .ok_or_else(|| reject(w::RejectionKind::NotFound, "instance is absent"))?;
                    state.instances.remove(index);
                }
                w::SceneOperation::LightingSet { lighting } => state.lighting = lighting,
                w::SceneOperation::ShadowModeSet { shadow_mode } => state.shadow_mode = shadow_mode,
                w::SceneOperation::CameraSet { camera } => {
                    if matches!(state.camera, w::CameraMode::Tracked { .. }) {
                        return Err(incompatible("tracked camera cannot be replaced"));
                    }
                    state.camera = w::CameraMode::Manual { camera };
                }
            }
            // Validate every intermediate operation, even one overwritten later.
            // Rebuild uses shared immutable mesh/map data but fresh native handles.
            prepared = Some(self.prepare(
                request.scene_id.clone(),
                state.clone(),
                revision,
                effect.clone(),
                Arc::clone(&generation),
            )?);
        }
        let prepared = prepared.ok_or_else(|| incompatible("mutation must be nonempty"))?;
        Ok(self.publish(prepared))
    }

    fn submit(&mut self, request: w::Submit) -> Result<w::SubmissionAccepted, ErrorObject> {
        self.runtime(&request.runtime_id)?;
        if let Some(receipt) = self.admission.existing(&request)? {
            return Ok(receipt);
        }
        if request.frames.len() > self.config.limits.frames_per_submission as usize {
            return Err(reject(
                w::RejectionKind::Capacity,
                "frame batch exceeds configured limit",
            ));
        }
        let scene = Arc::clone(self.scene(&request.scene_id)?);
        if request
            .expected_scene_revision
            .is_some_and(|r| r != scene.snapshot.scene_revision)
        {
            return Err(reject(w::RejectionKind::Conflict, "scene revision differs"));
        }
        let cameras = request
            .frames
            .iter()
            .map(|frame| {
                self.registrations
                    .frame_camera(
                        &self.config,
                        &scene.snapshot.state,
                        scene.camera.as_ref(),
                        &frame.geometry,
                    )
                    .map_err(|mut error| {
                        if let Some(data) = error.data.as_mut() {
                            data["frame"] = serde_json::json!(frame.frame);
                        }
                        error
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let receipt = w::SubmissionAccepted {
            runtime_id: request.runtime_id.clone(),
            submission_id: request.submission_id.clone(),
            scene_id: request.scene_id.clone(),
            scene_revision: scene.snapshot.scene_revision,
            effect: self.next_effect()?,
            frames: request
                .frames
                .iter()
                .map(|f| w::AcceptedFrame {
                    frame: f.frame.clone(),
                    position: f.position.clone(),
                })
                .collect(),
        };
        self.admission.admit(BoundSubmission {
            request,
            scene,
            cameras,
            receipt: receipt.clone(),
        })?;
        self.sequence = receipt.effect.sequence;
        Ok(receipt)
    }

    pub(super) fn apply(&mut self, params: w::Params) -> Result<Value, ErrorObject> {
        match params {
            w::Params::GetConfiguration(target) => {
                self.runtime(&target.runtime_id)?;
                value(&self.config)
            }
            w::Params::SceneCreate(request) => value(self.create(*request)?),
            w::Params::SceneMutate(request) => value(self.mutate(*request)?),
            w::Params::SceneGet(target) => {
                self.runtime(&target.runtime_id)?;
                value(&self.scene(&target.scene_id)?.snapshot)
            }
            w::Params::SceneRelease(target) => {
                self.runtime(&target.runtime_id)?;
                let released = self.scenes.contains_key(&target.scene_id);
                if released {
                    let effect = self.next_effect()?;
                    self.scenes.remove(&target.scene_id);
                    self.sequence = effect.sequence;
                    if self
                        .rotation
                        .as_ref()
                        .is_some_and(|rotation| rotation.state.scene_id == target.scene_id)
                    {
                        self.rotation = None;
                    }
                }
                value(w::ReleaseResult { released })
            }
            w::Params::RenderSubmit(request) => value(self.submit(*request)?),
            w::Params::RenderGet(target) => {
                self.runtime(&target.runtime_id)?;
                value(self.admission.get(&target)?)
            }
            w::Params::RenderCancel(target) => {
                self.runtime(&target.runtime_id)?;
                value(self.admission.cancel(&target)?)
            }
            w::Params::RenderRelease(target) => {
                self.runtime(&target.runtime_id)?;
                value(self.admission.release(&target)?)
            }
            w::Params::FramePublish(request) => {
                self.runtime(&request.runtime_id)?;
                value(self.admission.publish(*request)?)
            }
            w::Params::FrameRelease(request) => {
                self.runtime(&request.runtime_id)?;
                value(self.admission.release_image(*request)?)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoRender;
    impl RenderAdmission for NoRender {
        fn configure(
            &mut self,
            _: &w::Configuration,
            _: NativeSettings,
        ) -> Result<(), ErrorObject> {
            Ok(())
        }
        fn existing(&self, _: &w::Submit) -> Result<Option<w::SubmissionAccepted>, ErrorObject> {
            unreachable!()
        }
        fn admit(&mut self, _: BoundSubmission) -> Result<(), ErrorObject> {
            unreachable!()
        }
        fn get(&self, _: &w::SubmissionTarget) -> Result<w::SubmissionSnapshot, ErrorObject> {
            unreachable!()
        }
        fn cancel(&mut self, _: &w::SubmissionTarget) -> Result<w::CancelResult, ErrorObject> {
            unreachable!()
        }
        fn release(&mut self, _: &w::SubmissionTarget) -> Result<w::ReleaseResult, ErrorObject> {
            unreachable!()
        }
    }

    #[test]
    fn exhausted_effect_or_revision_cannot_publish_partial_state() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../../../../common/tests/fixtures/scene_frame.json"
        ))
        .unwrap();
        let config: w::Configuration =
            serde_json::from_value(fixtures["calls"][0]["result"].clone()).unwrap();
        let mut request: w::SceneCreate =
            serde_json::from_value(fixtures["calls"][1]["params"].clone()).unwrap();
        request.state.instances.clear();
        let mut registrations = Registrations::default();
        registrations
            .register_world(request.state.world.clone())
            .unwrap();
        let mut owner = SceneAdapter::new(config, registrations, Box::new(NoRender)).unwrap();
        owner.sequence = w::U64::from(u64::MAX);
        assert_eq!(owner.create(request.clone()).unwrap_err().code, 2005);
        assert!(owner.scenes.is_empty());
        owner.sequence = w::U64::from(u64::MAX - 1);
        owner.create(request.clone()).unwrap();
        let mutation = w::SceneMutate {
            runtime_id: request.runtime_id.clone(),
            scene_id: request.scene_id.clone(),
            expected_revision: w::U64::from(0),
            operations: vec![w::SceneOperation::ShadowModeSet {
                shadow_mode: w::ShadowMode::Off,
            }],
        };
        assert_eq!(owner.mutate(mutation.clone()).unwrap_err().code, 2005);
        assert_eq!(
            owner
                .scene(&request.scene_id)
                .unwrap()
                .snapshot
                .scene_revision
                .get(),
            0
        );
        owner.sequence = w::U64::from(0);
        Arc::get_mut(owner.scenes.get_mut(&request.scene_id).unwrap())
            .unwrap()
            .snapshot
            .scene_revision = w::U64::from(u64::MAX);
        let before = owner.scene(&request.scene_id).unwrap().snapshot.clone();
        let mut mutation = mutation;
        mutation.expected_revision = w::U64::from(u64::MAX);
        assert_eq!(owner.mutate(mutation).unwrap_err().code, 2005);
        assert_eq!(owner.scene(&request.scene_id).unwrap().snapshot, before);
        assert_eq!(owner.sequence.get(), 0);
    }
}
