use std::{
    any::Any,
    sync::{Arc, Mutex},
};

use chr_common::{jsonrpc::ErrorObject, scene_frame as w};
use chr_renderer::camera::Camera;

use crate::{
    frame_resources::{FrameMetadata, Lease, WorkGuard},
    retained::BoundScene,
};

use super::lock;

/// Identity and logical content are preserved independently of dispatch order.
#[derive(Clone)]
pub struct Provenance {
    pub receipt: w::SubmissionAccepted,
    pub input: w::FrameInput,
}

pub(super) enum Completion {
    Running,
    Abandoned,
    Finished(Result<OutputResource, ()>),
    Observed,
}

/// Opaque, separately allocated native output; never a source-image handle.
pub struct OutputResource {
    id: w::Id,
    payload: Box<dyn Any + Send + Sync>,
}

impl OutputResource {
    pub fn id(&self) -> &w::Id {
        &self.id
    }
    pub fn payload<T: Any>(&self) -> Option<&T> {
        self.payload.downcast_ref()
    }
}

/// Move to the physical completion observer BEFORE asynchronous render submission.
/// Dropping this object without `rendered`/`failed` quarantines physical capacity.
#[must_use]
pub struct Execution {
    pub provenance: Provenance,
    pub scene: Arc<BoundScene>,
    pub camera: Camera,
    pub input: Lease<FrameMetadata>,
    pub(super) output_id: w::Id,
    pub(super) guard: Option<WorkGuard<FrameMetadata>>,
    pub(super) completion: Arc<Mutex<Completion>>,
}

impl Execution {
    /// Only call after every physical input access has finished. The payload is
    /// a separately owned output allocation (synthetic data or a future GPU owner).
    pub fn rendered(mut self, payload: impl Any + Send + Sync) -> Result<(), ErrorObject> {
        let output = OutputResource {
            id: self.output_id.clone(),
            payload: Box::new(payload),
        };
        self.finish(Ok(output))
    }

    /// Only call after failed physical work has actually stopped accessing input.
    pub fn failed(mut self) -> Result<(), ErrorObject> {
        self.finish(Err(()))
    }

    fn finish(&mut self, result: Result<OutputResource, ()>) -> Result<(), ErrorObject> {
        let mut completion = lock(&self.completion)?;
        if let Some(guard) = self.guard.take() {
            guard.complete();
        }
        *completion = Completion::Finished(result);
        Ok(())
    }
}

impl Drop for Execution {
    fn drop(&mut self) {
        if self.guard.is_some()
            && let Ok(mut completion) = self.completion.lock()
        {
            *completion = Completion::Abandoned;
        }
    }
}

/// One bounded delivery observation cell; repeats never accumulate history.
#[derive(Clone)]
pub struct DeliveryObserver(pub(super) Arc<Mutex<w::Delivery>>);

impl DeliveryObserver {
    /// The port calls this ONLY for actual transmission, not queue admission.
    /// First transmission wins, including when the port repeats its current frame.
    pub fn transmitted(&self, stream: w::Id, slot: w::U64) -> Result<(), ErrorObject> {
        let mut delivery = lock(&self.0)?;
        if matches!(*delivery, w::Delivery::Queued {}) {
            *delivery = w::Delivery::Transmitted {
                output_stream_id: stream,
                first_slot: slot,
            };
        }
        Ok(())
    }

    pub fn failed(&self) -> Result<(), ErrorObject> {
        let mut delivery = lock(&self.0)?;
        if matches!(*delivery, w::Delivery::Queued {}) {
            *delivery = w::Delivery::Dropped {
                reason: w::DropReason::OutputFailed,
            };
        }
        Ok(())
    }
}

/// The port owns this resource until replacement/shutdown, even after result release.
pub struct OutputFrame {
    pub provenance: Arc<Provenance>,
    pub resource: OutputResource,
    pub observer: DeliveryObserver,
}

impl Drop for OutputFrame {
    fn drop(&mut self) {
        let _ = self.observer.failed();
    }
}

/// Nonblocking bounded handoff. Err returns the unchanged frame when full.
/// Implementations must not reenter the scheduler while this method runs.
pub trait OutputPort {
    fn try_handoff(&mut self, frame: OutputFrame) -> Result<(), OutputFrame>;
}
