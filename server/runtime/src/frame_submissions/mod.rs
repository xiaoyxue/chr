//! Opt-in synthetic admission and native execution/output seams. No daemon,
//! GPU, hardware clock, tracking-loss policy, or second scene registry.
mod execution;
mod join;

pub use execution::{
    DeliveryObserver, Execution, OutputFrame, OutputPort, OutputResource, Provenance,
};
pub use join::FrameJoin;

use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicU64, Ordering},
};

use chr_common::{jsonrpc::ErrorObject, scene_frame as w};
use w::Validate;

use crate::{
    frame_resources::{
        FrameMetadata, FrameResources, Lease, Producer, ProducerGuard, Readiness, ResourceError,
        WriteTicket,
    },
    retained::{BoundSubmission, NativeSettings, RenderAdmission, reject},
};
use execution::Completion;

static NEXT_OWNER: AtomicU64 = AtomicU64::new(0);

/// Independent monotonic milliseconds, unrelated to media ticks or wall time.
/// Embedders must keep advancing/polling even when media playback is paused.
pub trait MonotonicClock: Send + Sync + 'static {
    fn now_ms(&self) -> u64;
}

/// A clone is a native driver handle to the SAME owner, not another registry.
/// Locks cover bounded CPU operations only. Never wait for physical work here.
#[derive(Clone)]
pub struct FrameSubmissions(Arc<Mutex<State>>);

struct State {
    config: w::Configuration,
    configured: bool,
    resources: FrameResources,
    producer: Producer,
    clock: Arc<dyn MonotonicClock>,
    last_now: u64,
    namespace: u64,
    next_output: u64,
    records: Vec<Option<Record>>,
}

struct Record {
    bound: BoundSubmission,
    public: bool,
    deadline: u64,
    frames: Vec<Frame>,
}

struct Frame {
    result: w::FrameResult,
    input: Option<Lease<FrameMetadata>>,
    completion: Option<Arc<Mutex<Completion>>>,
    output: Option<OutputResource>,
    delivery: Option<DeliveryObserver>,
    output_id: w::Id,
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, ErrorObject> {
    mutex
        .lock()
        .map_err(|_| reject(w::RejectionKind::InvalidState, "native owner failed"))
}

fn resource_error(error: ResourceError) -> ErrorObject {
    let kind = match error {
        ResourceError::NotFound => w::RejectionKind::NotFound,
        ResourceError::Capacity => w::RejectionKind::Capacity,
        ResourceError::IncompatibleInput | ResourceError::InvalidPixels => {
            w::RejectionKind::IncompatibleInput
        }
        ResourceError::InvalidConfiguration => w::RejectionKind::UnsupportedProfile,
        _ => w::RejectionKind::InvalidState,
    };
    reject(kind, error)
}

fn terminal(result: &w::FrameResult) -> bool {
    matches!(
        result,
        w::FrameResult::Failed { .. }
            | w::FrameResult::Rendered {
                delivery: w::Delivery::Transmitted { .. } | w::Delivery::Dropped { .. },
                ..
            }
    )
}

impl Frame {
    fn fail(&mut self, reason: w::FailureKind, input: &w::FrameInput) {
        self.result = w::FrameResult::Failed {
            frame: input.frame.clone(),
            position: input.position.clone(),
            reason,
        };
        self.input = None;
        self.output = None;
    }
    fn charged(&self) -> bool {
        self.completion.is_some() || !terminal(&self.result)
    }
}

impl FrameSubmissions {
    /// `capabilities` is the complete explicitly supported synthetic configuration,
    /// not production defaults. The adapter must install exactly this profile.
    /// `producer` is installed by a trusted local controller, never selected by RPC.
    pub fn new(
        capabilities: w::Configuration,
        resources: FrameResources,
        producer: Producer,
        clock: Arc<dyn MonotonicClock>,
    ) -> Result<Self, ErrorObject> {
        capabilities
            .validate()
            .map_err(|e| reject(w::RejectionKind::UnsupportedProfile, e))?;
        resources
            .validate_producer(&producer)
            .map_err(resource_error)?;
        let resource_config = resources.configuration();
        if capabilities.runtime_id != resource_config.runtime_id
            || capabilities.native_extent != resource_config.native_extent
            || capabilities.limits.published_frames as usize != resource_config.max_frames
        {
            return Err(reject(
                w::RejectionKind::UnsupportedProfile,
                "resource capabilities differ",
            ));
        }
        let namespace = NEXT_OWNER
            .try_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_add(1))
            .map_err(|_| reject(w::RejectionKind::Capacity, "output namespace exhausted"))?;
        let count = capabilities.limits.retained_submissions as usize;
        Ok(Self(Arc::new(Mutex::new(State {
            config: capabilities,
            configured: false,
            resources,
            producer,
            last_now: clock.now_ms(),
            clock,
            namespace,
            next_output: 0,
            records: (0..count).map(|_| None).collect(),
        }))))
    }

    /// Trusted native injection into the existing resource owner. No new wire schema.
    pub fn register(
        &self,
        metadata: FrameMetadata,
        pixels: &[u8],
    ) -> Result<(WriteTicket, ProducerGuard<FrameMetadata>), ErrorObject> {
        let mut state = lock(&self.0)?;
        let producer = state.producer.clone();
        state
            .resources
            .register(&producer, metadata, pixels)
            .map_err(resource_error)
    }

    /// Revoke a registration that will not be published; not producer completion.
    pub fn cancel_write(&self, ticket: &WriteTicket) -> Result<bool, ErrorObject> {
        lock(&self.0)?
            .resources
            .cancel_write(ticket)
            .map_err(resource_error)
    }

    /// Required progress call: observe readiness, deadlines, physical completion,
    /// and actual output events. Never waits for any of those events.
    pub fn tick(&self) -> Result<(), ErrorObject> {
        lock(&self.0)?.poll()
    }

    /// One bounded execution handoff. WorkGuard exists before returning the job.
    /// Ready frames may bypass pending siblings; identities remain request ordered.
    pub fn next_ready(&self) -> Result<Option<Execution>, ErrorObject> {
        let mut state = lock(&self.0)?;
        state.poll()?;
        for record in state.records.iter_mut().flatten() {
            for (index, frame) in record.frames.iter_mut().enumerate() {
                if !matches!(frame.result, w::FrameResult::Pending { .. }) {
                    continue;
                }
                let Some(input) = frame.input.as_ref() else {
                    continue;
                };
                if input.readiness() != Readiness::Ready {
                    continue;
                }
                let guard = input.begin_work().map_err(resource_error)?;
                let completion = Arc::new(Mutex::new(Completion::Running));
                let input = frame.input.take().ok_or_else(|| {
                    reject(w::RejectionKind::InvalidState, "input already dispatched")
                })?;
                frame.completion = Some(Arc::clone(&completion));
                return Ok(Some(Execution {
                    provenance: Provenance {
                        receipt: record.bound.receipt.clone(),
                        input: record.bound.request.frames[index].clone(),
                    },
                    scene: Arc::clone(&record.bound.scene),
                    camera: record.bound.cameras[index].clone(),
                    input,
                    output_id: frame.output_id.clone(),
                    guard: Some(guard),
                    completion,
                }));
            }
        }
        Ok(None)
    }

    /// Serializes cancellation with handoff. The port MUST be bounded/nonblocking.
    /// A full queue records dropped/queue_full, never transmitted.
    pub fn handoff(&self, port: &mut impl OutputPort) -> Result<(), ErrorObject> {
        let mut state = lock(&self.0)?;
        state.poll()?;
        for record in state.records.iter_mut().flatten() {
            for (index, frame) in record.frames.iter_mut().enumerate() {
                let Some(resource) = frame.output.take() else {
                    continue;
                };
                let observer = DeliveryObserver(Arc::new(Mutex::new(w::Delivery::Queued {})));
                let output = OutputFrame {
                    provenance: Arc::new(Provenance {
                        receipt: record.bound.receipt.clone(),
                        input: record.bound.request.frames[index].clone(),
                    }),
                    resource,
                    observer: observer.clone(),
                };
                if port.try_handoff(output).is_err() {
                    if let w::FrameResult::Rendered { delivery, .. } = &mut frame.result {
                        *delivery = w::Delivery::Dropped {
                            reason: w::DropReason::QueueFull,
                        };
                    }
                } else {
                    frame.delivery = Some(observer);
                }
            }
        }
        Ok(())
    }
}

impl State {
    fn runtime(&self, runtime: &w::Id) -> Result<(), ErrorObject> {
        if runtime != &self.config.runtime_id {
            return Err(reject(
                w::RejectionKind::RuntimeChanged,
                "runtime identity changed",
            ));
        }
        Ok(())
    }
    fn now(&mut self) -> Result<u64, ErrorObject> {
        let now = self.clock.now_ms();
        if now < self.last_now {
            return Err(reject(
                w::RejectionKind::InvalidState,
                "monotonic clock regressed",
            ));
        }
        self.last_now = now;
        Ok(now)
    }
    fn poll(&mut self) -> Result<(), ErrorObject> {
        let now = self.now()?;
        for slot in &mut self.records {
            let Some(record) = slot else { continue };
            for (index, frame) in record.frames.iter_mut().enumerate() {
                let input = &record.bound.request.frames[index];
                if matches!(frame.result, w::FrameResult::Pending { .. }) {
                    if now >= record.deadline {
                        frame.fail(w::FailureKind::DeadlineExceeded, input);
                    } else if frame
                        .input
                        .as_ref()
                        .is_some_and(|v| v.readiness() == Readiness::Failed)
                    {
                        frame.fail(w::FailureKind::WriteFailed, input);
                    }
                }
                if let Some(completion) = frame.completion.as_ref().cloned() {
                    let mut completion = lock(&completion)?;
                    match &*completion {
                        Completion::Finished(_) => {
                            let Completion::Finished(result) =
                                std::mem::replace(&mut *completion, Completion::Observed)
                            else {
                                unreachable!()
                            };
                            frame.completion = None;
                            if matches!(frame.result, w::FrameResult::Pending { .. }) {
                                match result {
                                    Ok(output) => {
                                        frame.result = w::FrameResult::Rendered {
                                            frame: input.frame.clone(),
                                            position: input.position.clone(),
                                            output_frame_id: output.id().clone(),
                                            delivery: w::Delivery::Queued {},
                                        };
                                        frame.output = Some(output);
                                    }
                                    Err(()) => frame.fail(w::FailureKind::RenderFailed, input),
                                }
                            }
                        }
                        Completion::Abandoned => {
                            if matches!(frame.result, w::FrameResult::Pending { .. }) {
                                frame.fail(w::FailureKind::RuntimeFailed, input);
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(observer) = &frame.delivery
                    && let w::FrameResult::Rendered { delivery, .. } = &mut frame.result
                {
                    *delivery = lock(&observer.0)?.clone();
                }
            }
            if !record.public && !record.frames.iter().any(Frame::charged) {
                *slot = None;
            }
        }
        self.resources.reclaim();
        Ok(())
    }
    fn find(&self, target: &w::SubmissionTarget) -> Result<&Record, ErrorObject> {
        self.runtime(&target.runtime_id)?;
        self.records
            .iter()
            .flatten()
            .find(|r| r.public && r.bound.request.submission_id == target.submission_id)
            .ok_or_else(|| reject(w::RejectionKind::NotFound, "submission is not retained"))
    }
}

impl RenderAdmission for FrameSubmissions {
    fn configure(
        &mut self,
        config: &w::Configuration,
        _: NativeSettings,
    ) -> Result<(), ErrorObject> {
        let mut state = lock(&self.0)?;
        if state.configured {
            return Err(reject(
                w::RejectionKind::InvalidState,
                "admission owner already installed",
            ));
        }
        if state.config != *config {
            return Err(reject(
                w::RejectionKind::UnsupportedProfile,
                "synthetic capabilities differ",
            ));
        }
        state.configured = true;
        Ok(())
    }
    fn publish(&mut self, request: w::FramePublish) -> Result<w::PublishedFrame, ErrorObject> {
        let mut state = lock(&self.0)?;
        state.runtime(&request.runtime_id)?;
        request
            .validate()
            .map_err(|e| reject(w::RejectionKind::IncompatibleInput, e))?;
        let metadata = FrameMetadata {
            frame: request.frame.clone(),
            timeline: request.timeline,
            position: request.position.clone(),
            image: request.image,
        };
        let producer = state.producer.clone();
        let image = state
            .resources
            .publish_id(&producer, &request.producer_ticket, &metadata)
            .map_err(|e| {
                ErrorObject::from(w::Rejection {
                    kind: match e {
                        ResourceError::NotFound => w::RejectionKind::NotFound,
                        ResourceError::IncompatibleInput => w::RejectionKind::IncompatibleInput,
                        _ => w::RejectionKind::InvalidState,
                    },
                    message: e.to_string(),
                    frame: Some(request.frame.clone()),
                })
            })?;
        Ok(w::PublishedFrame {
            runtime_id: request.runtime_id,
            frame: request.frame,
            image_ref: image.id().clone(),
            position: request.position,
        })
    }
    fn release_image(&mut self, request: w::ImageTarget) -> Result<w::ReleaseResult, ErrorObject> {
        let mut state = lock(&self.0)?;
        state.runtime(&request.runtime_id)?;
        Ok(w::ReleaseResult {
            released: state.resources.release_id(&request.image_ref),
        })
    }
    fn existing(&self, request: &w::Submit) -> Result<Option<w::SubmissionAccepted>, ErrorObject> {
        let state = lock(&self.0)?;
        state.runtime(&request.runtime_id)?;
        let Some(record) = state
            .records
            .iter()
            .flatten()
            .find(|r| r.public && r.bound.request.submission_id == request.submission_id)
        else {
            return Ok(None);
        };
        if record.bound.request != *request {
            return Err(reject(
                w::RejectionKind::Conflict,
                "submission content differs",
            ));
        }
        Ok(Some(record.bound.receipt.clone()))
    }
    fn admit(&mut self, bound: BoundSubmission) -> Result<(), ErrorObject> {
        let mut state = lock(&self.0)?;
        state.runtime(&bound.request.runtime_id)?;
        bound
            .request
            .validate()
            .map_err(|e| reject(w::RejectionKind::IncompatibleInput, e))?;
        state.poll()?;
        if state
            .records
            .iter()
            .flatten()
            .any(|r| r.public && r.bound.request.submission_id == bound.request.submission_id)
        {
            return Err(reject(
                w::RejectionKind::Conflict,
                "submission already retained",
            ));
        }
        let count = bound.request.frames.len();
        let charged = state
            .records
            .iter()
            .flatten()
            .flat_map(|r| &r.frames)
            .filter(|f| f.charged())
            .count();
        if count > state.config.limits.frames_per_submission as usize
            || count > (state.config.limits.inflight_frames as usize).saturating_sub(charged)
        {
            return Err(reject(
                w::RejectionKind::Capacity,
                "inflight frame capacity exhausted",
            ));
        }
        let slot = state
            .records
            .iter()
            .position(Option::is_none)
            .ok_or_else(|| {
                reject(
                    w::RejectionKind::Capacity,
                    "retained and draining submission capacity exhausted",
                )
            })?;
        let deadline = state
            .last_now
            .checked_add(u64::from(state.config.limits.render_timeout_ms))
            .ok_or_else(|| reject(w::RejectionKind::Capacity, "deadline exhausted"))?;
        let next_output = state
            .next_output
            .checked_add(count as u64)
            .ok_or_else(|| reject(w::RejectionKind::Capacity, "output identity exhausted"))?;
        let frames = bound
            .request
            .frames
            .iter()
            .enumerate()
            .map(|(index, input)| {
                let lease = state
                    .resources
                    .acquire_id(&input.image_ref)
                    .map_err(resource_error)?;
                let metadata = lease.metadata();
                if metadata.frame != input.frame
                    || metadata.position != input.position
                    || metadata.image.extent != state.config.native_extent
                {
                    return Err(reject(
                        w::RejectionKind::IncompatibleInput,
                        "registered frame metadata differs",
                    ));
                }
                let output_id = w::Id::try_from(format!(
                    "{}:output:{}:{}",
                    state.config.runtime_id.as_str(),
                    state.namespace,
                    state.next_output + index as u64
                ))
                .map_err(|e| reject(w::RejectionKind::InvalidState, e))?;
                Ok(Frame {
                    result: w::FrameResult::Pending {
                        frame: input.frame.clone(),
                        position: input.position.clone(),
                    },
                    input: Some(lease),
                    completion: None,
                    output: None,
                    delivery: None,
                    output_id,
                })
            })
            .enumerate()
            .map(|(index, result)| {
                result.map_err(|mut e: ErrorObject| {
                    if let Some(data) = e.data.as_mut() {
                        data["frame"] = serde_json::json!(bound.request.frames[index].frame);
                    }
                    e
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        state.next_output = next_output;
        state.records[slot] = Some(Record {
            bound,
            public: true,
            deadline,
            frames,
        });
        Ok(())
    }
    fn get(&self, target: &w::SubmissionTarget) -> Result<w::SubmissionSnapshot, ErrorObject> {
        let mut state = lock(&self.0)?;
        state.runtime(&target.runtime_id)?;
        state.poll()?;
        let record = state.find(target)?;
        let receipt = &record.bound.receipt;
        Ok(w::SubmissionSnapshot {
            runtime_id: receipt.runtime_id.clone(),
            submission_id: receipt.submission_id.clone(),
            scene_id: receipt.scene_id.clone(),
            scene_revision: receipt.scene_revision,
            effect: receipt.effect.clone(),
            frames: record.frames.iter().map(|f| f.result.clone()).collect(),
        })
    }
    fn cancel(&mut self, target: &w::SubmissionTarget) -> Result<w::CancelResult, ErrorObject> {
        let mut state = lock(&self.0)?;
        state.runtime(&target.runtime_id)?;
        state.poll()?;
        state.find(target)?;
        let mut result = w::CancelResult {
            runtime_id: target.runtime_id.clone(),
            submission_id: target.submission_id.clone(),
            cancelled: Vec::new(),
            too_late: Vec::new(),
        };
        for record in state
            .records
            .iter_mut()
            .flatten()
            .filter(|r| r.public && r.bound.request.submission_id == target.submission_id)
        {
            for (frame, input) in record.frames.iter_mut().zip(&record.bound.request.frames) {
                if terminal(&frame.result) || frame.delivery.is_some() {
                    result.too_late.push(input.frame.clone());
                } else {
                    frame.fail(w::FailureKind::Cancelled, input);
                    result.cancelled.push(input.frame.clone());
                }
            }
        }
        Ok(result)
    }
    fn release(&mut self, target: &w::SubmissionTarget) -> Result<w::ReleaseResult, ErrorObject> {
        let mut state = lock(&self.0)?;
        state.runtime(&target.runtime_id)?;
        state.poll()?;
        let Some(record) = state
            .records
            .iter_mut()
            .flatten()
            .find(|r| r.public && r.bound.request.submission_id == target.submission_id)
        else {
            return Ok(w::ReleaseResult { released: false });
        };
        if !record.frames.iter().all(|f| terminal(&f.result)) {
            return Err(reject(
                w::RejectionKind::InvalidState,
                "submission is not terminal",
            ));
        }
        record.public = false;
        state.poll()?;
        Ok(w::ReleaseResult { released: true })
    }
}
