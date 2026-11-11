//! Opt-in, CPU-only scene control. Acquisition never waits for GPU completion.
//!
//! The embedding runtime supplies immutable registration and an admission port.
//! The normal triangle daemon does not install this synthetic integration profile.

mod mapping;
mod owner;

pub use mapping::{CameraContext, NativeSettings, Registrations};
pub use owner::{BoundScene, BoundSubmission, RenderAdmission, SceneAdapter};

use chr_common::{jsonrpc::ErrorObject, scene_frame as wire};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

type Reply = Result<Value, ErrorObject>;

struct Command {
    params: wire::Params,
    reply: oneshot::Sender<Reply>,
}

/// Cloneable acquisition endpoint shared by NativeSocket, WebSocket and native callers.
#[derive(Clone)]
pub struct SceneControl {
    commands: mpsc::Sender<Command>,
}

impl SceneControl {
    /// Explicit bounded queue, with one CPU owner independent of the render worker.
    /// Dropping every endpoint drains the admitted commands and stops the task.
    pub fn spawn(
        mut adapter: SceneAdapter,
        capacity: usize,
    ) -> Result<(Self, tokio::task::JoinHandle<()>), ErrorObject> {
        if capacity == 0 || capacity > tokio::sync::Semaphore::MAX_PERMITS {
            return Err(reject(
                wire::RejectionKind::Capacity,
                "invalid command capacity",
            ));
        }
        let (commands, mut receiver) = mpsc::channel::<Command>(capacity);
        let worker = tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                // An uncertain/disconnected reply does not retract an applied effect.
                let result = adapter.apply(command.params);
                let _ = command.reply.send(result);
            }
        });
        Ok((Self { commands }, worker))
    }

    /// Uses the shared strict parser on every transport, before native mapping.
    pub async fn call(&self, method: &str, params: Option<Value>) -> Reply {
        self.enqueue(wire::parse_params(method, params)?).await
    }

    /// Native producers use the same validation, queue and serialization boundary.
    pub async fn call_typed(&self, method: &str, params: wire::Params) -> Reply {
        self.call(method, Some(value(params)?)).await
    }

    async fn enqueue(&self, params: wire::Params) -> Reply {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command { params, reply })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => {
                    reject(wire::RejectionKind::Capacity, "scene command queue is full")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    reject(wire::RejectionKind::InvalidState, "scene control stopped")
                }
            })?;
        response
            .await
            .map_err(|_| reject(wire::RejectionKind::InvalidState, "scene control stopped"))?
    }
}

pub(crate) fn reject(kind: wire::RejectionKind, message: impl ToString) -> ErrorObject {
    wire::Rejection {
        kind,
        message: message.to_string(),
        frame: None,
    }
    .into()
}

pub(crate) fn incompatible(message: impl ToString) -> ErrorObject {
    reject(wire::RejectionKind::IncompatibleInput, message)
}

fn value(data: impl serde::Serialize) -> Reply {
    serde_json::to_value(data).map_err(|e| ErrorObject::internal(e.to_string()))
}
