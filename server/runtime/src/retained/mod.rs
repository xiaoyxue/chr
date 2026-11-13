//! Opt-in, CPU-only scene control. Acquisition never waits for GPU completion.
//!
//! The embedding runtime supplies immutable registration and an admission port.
//! The normal triangle daemon does not install this synthetic integration profile.

mod mapping;
mod owner;
mod rotation;

pub use mapping::{CameraContext, NativeSettings, Registrations};
pub use owner::{BoundScene, BoundSubmission, RenderAdmission, SceneAdapter};

use chr_common::{jsonrpc::ErrorObject, scene_frame as wire};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

type Reply = Result<Value, ErrorObject>;

enum Command {
    Apply {
        params: wire::Params,
        reply: oneshot::Sender<Reply>,
    },
    Rotate {
        request: rotation::Request,
        frame: u64,
        reply: oneshot::Sender<Reply>,
    },
    Advance {
        frame: u64,
        reply: oneshot::Sender<Result<(), ErrorObject>>,
    },
    Register {
        id: wire::Id,
        asset: Box<chr_renderer::glb::GlbAsset>,
        reply: oneshot::Sender<Result<(), ErrorObject>>,
    },
    Shutdown(oneshot::Sender<()>),
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
            let mut stopped = None;
            while let Some(command) = receiver.recv().await {
                match command {
                    Command::Apply { params, reply } => {
                        // An uncertain/disconnected reply does not retract an applied effect.
                        let _ = reply.send(adapter.apply(params));
                    }
                    Command::Rotate {
                        request,
                        frame,
                        reply,
                    } => {
                        let _ = reply.send(adapter.rotate(request, frame));
                    }
                    Command::Advance { frame, reply } => {
                        let _ = reply.send(adapter.advance_rotation(frame));
                    }
                    Command::Register { id, asset, reply } => {
                        let _ = reply.send(adapter.register_model(id, *asset));
                    }
                    Command::Shutdown(reply) => {
                        receiver.close();
                        stopped = Some(reply);
                    }
                }
            }
            if let Some(reply) = stopped {
                let _ = reply.send(());
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

    pub async fn rotate(&self, method: &str, params: Option<Value>, frame: u64) -> Reply {
        let request = rotation::Request::parse(method, params)?;
        let (reply, response) = oneshot::channel();
        self.send(Command::Rotate {
            request,
            frame,
            reply,
        })?;
        response
            .await
            .map_err(|_| reject(wire::RejectionKind::InvalidState, "scene control stopped"))?
    }

    /// Native output-clock tick, serialized with scene edits before input admission.
    pub async fn advance_rotation(&self, frame: u64) -> Result<(), ErrorObject> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Advance { frame, reply })?;
        response
            .await
            .map_err(|_| reject(wire::RejectionKind::InvalidState, "scene control stopped"))?
    }

    /// Registers one more model after startup, serialized with scene edits. Scenes
    /// created later may reference it; it is never removed.
    pub async fn register_model(
        &self,
        id: wire::Id,
        asset: chr_renderer::glb::GlbAsset,
    ) -> Result<(), ErrorObject> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Register {
            id,
            asset: Box::new(asset),
            reply,
        })?;
        response
            .await
            .map_err(|_| reject(wire::RejectionKind::InvalidState, "scene control stopped"))?
    }

    /// Stop native admission and drain already accepted CPU commands, even when
    /// connected transport handlers still hold endpoints. This is not a GPU fence.
    pub async fn shutdown(&self) {
        let (reply, done) = oneshot::channel();
        if self.commands.send(Command::Shutdown(reply)).await.is_ok() {
            let _ = done.await;
        }
    }

    async fn enqueue(&self, params: wire::Params) -> Reply {
        let (reply, response) = oneshot::channel();
        self.send(Command::Apply { params, reply })?;
        response
            .await
            .map_err(|_| reject(wire::RejectionKind::InvalidState, "scene control stopped"))?
    }

    fn send(&self, command: Command) -> Result<(), ErrorObject> {
        self.commands.try_send(command).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                reject(wire::RejectionKind::Capacity, "scene command queue is full")
            }
            mpsc::error::TrySendError::Closed(_) => {
                reject(wire::RejectionKind::InvalidState, "scene control stopped")
            }
        })
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
