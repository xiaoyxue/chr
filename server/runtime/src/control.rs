//! Control plane: maps JSON-RPC methods (the args clients send) onto the render
//! pipeline, the SDI port's status, and WebRTC signaling for the program monitor.

use std::sync::Arc;

use chr_common::{
    jsonrpc::{ErrorObject, INVALID_PARAMS, SERVER_BUSY, SERVER_UNAVAILABLE},
    methods::{
        sdi,
        triangle::{self, StartRotationParams, TriangleState},
        webrtc::{self, SdpType, SessionDescription},
    },
    server::{ConnectionInfo, Handler},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::{
    monitor::webrtc::{WebRtcError, WebRtcHub},
    pipeline::Command,
    sdi::SdiStats,
};

pub struct ControlHandler {
    commands: mpsc::Sender<Command>,
    webrtc: Arc<WebRtcHub>,
    sdi: SdiStats,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

impl ControlHandler {
    pub fn new(commands: mpsc::Sender<Command>, webrtc: Arc<WebRtcHub>, sdi: SdiStats) -> Self {
        Self {
            commands,
            webrtc,
            sdi,
        }
    }

    /// Queues a render command and waits until the worker applied it.
    async fn render<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<T, ErrorObject> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(make(reply))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => {
                    ErrorObject::new(SERVER_BUSY, "render command queue is full")
                }
                mpsc::error::TrySendError::Closed(_) => render_stopped(),
            })?;
        response.await.map_err(|_| render_stopped())
    }

    async fn offer(
        &self,
        connection: &ConnectionInfo,
        offer: SessionDescription,
    ) -> Result<SessionDescription, ErrorObject> {
        if offer.sdp_type != SdpType::Offer {
            return Err(ErrorObject::invalid_params("type must be \"offer\""));
        }
        let sdp =
            self.webrtc
                .answer(connection.id, offer.sdp)
                .await
                .map_err(|error| match error {
                    WebRtcError::TooManyPeers => ErrorObject::new(SERVER_BUSY, error.to_string()),
                    WebRtcError::InvalidOffer(_) => {
                        ErrorObject::new(INVALID_PARAMS, error.to_string())
                    }
                    WebRtcError::GatheringTimeout | WebRtcError::Stack(_) => {
                        ErrorObject::internal(error.to_string())
                    }
                })?;
        Ok(SessionDescription {
            sdp_type: SdpType::Answer,
            sdp,
        })
    }
}

fn render_stopped() -> ErrorObject {
    ErrorObject::new(SERVER_UNAVAILABLE, "render loop is not running")
}

/// Missing params mean defaults; present params must match `T` exactly.
fn params<T: DeserializeOwned + Default>(params: Option<Value>) -> Result<T, ErrorObject> {
    match params {
        None => Ok(T::default()),
        Some(value) => serde_json::from_value(value)
            .map_err(|error| ErrorObject::invalid_params(error.to_string())),
    }
}

fn required<T: DeserializeOwned>(params: Option<Value>) -> Result<T, ErrorObject> {
    let value = params.ok_or_else(|| ErrorObject::invalid_params("params are required"))?;
    serde_json::from_value(value).map_err(|error| ErrorObject::invalid_params(error.to_string()))
}

fn to_value(state: TriangleState) -> Result<Value, ErrorObject> {
    serde_json::to_value(state).map_err(|error| ErrorObject::internal(error.to_string()))
}

impl Handler for ControlHandler {
    async fn handle(
        &self,
        connection: &ConnectionInfo,
        method: &str,
        raw_params: Option<Value>,
    ) -> Result<Value, ErrorObject> {
        match method {
            triangle::START_ROTATION => {
                let request: StartRotationParams = params(raw_params)?;
                let state = self
                    .render(|reply| Command::StartRotation {
                        angular_velocity_rad_per_s: request.angular_velocity_rad_per_s,
                        reply,
                    })
                    .await?
                    .map_err(ErrorObject::invalid_params)?;
                log::info!("{} {method}: frame {}", connection.id, state.frame);
                to_value(state)
            }
            triangle::STOP_ROTATION => {
                params::<NoParams>(raw_params)?;
                let state = self.render(|reply| Command::StopRotation { reply }).await?;
                log::info!("{} {method}: frame {}", connection.id, state.frame);
                to_value(state)
            }
            triangle::GET_STATE => {
                params::<NoParams>(raw_params)?;
                to_value(self.render(|reply| Command::GetState { reply }).await?)
            }
            sdi::GET_STATUS => {
                params::<NoParams>(raw_params)?;
                serde_json::to_value(self.sdi.status())
                    .map_err(|error| ErrorObject::internal(error.to_string()))
            }
            webrtc::OFFER => {
                let answer = self.offer(connection, required(raw_params)?).await?;
                serde_json::to_value(answer)
                    .map_err(|error| ErrorObject::internal(error.to_string()))
            }
            _ => Err(ErrorObject::method_not_found(method)),
        }
    }

    fn disconnected(&self, connection: &ConnectionInfo) {
        self.webrtc.close_connection(connection.id);
    }
}

#[cfg(test)]
mod tests {
    use chr_common::{
        jsonrpc::METHOD_NOT_FOUND,
        server::{ConnectionId, Transport},
    };
    use serde_json::json;

    use super::*;
    use crate::{
        monitor::{encoder::VideoFeed, webrtc::IceConfig},
        scene::Scene,
        sdi::VideoMode,
    };

    fn test_handler(commands: mpsc::Sender<Command>) -> ControlHandler {
        let hub = Arc::new(WebRtcHub::new(
            Arc::new(VideoFeed::new()),
            30,
            IceConfig::default(),
        ));
        let mode = VideoMode {
            width: 64,
            height: 36,
            fps: 30,
        };
        ControlHandler::new(commands, hub, SdiStats::idle(mode))
    }

    /// Stands in for the render worker: applies commands to a scene without a GPU.
    fn fake_worker() -> (ControlHandler, tokio::task::JoinHandle<Scene>) {
        let (commands, mut receiver) = mpsc::channel(4);
        let worker = tokio::spawn(async move {
            let mut scene = Scene::new(30);
            while let Some(command) = receiver.recv().await {
                match command {
                    Command::StartRotation {
                        angular_velocity_rad_per_s,
                        reply,
                    } => {
                        let _ = reply.send(scene.start_rotation(angular_velocity_rad_per_s));
                    }
                    Command::StopRotation { reply } => {
                        let _ = reply.send(scene.stop_rotation());
                    }
                    Command::GetState { reply } => {
                        let _ = reply.send(scene.state());
                    }
                }
                scene.advance();
            }
            scene
        });
        (test_handler(commands), worker)
    }

    const CONNECTION: ConnectionInfo = ConnectionInfo {
        id: ConnectionId(1),
        transport: Transport::NativeSocket,
    };

    #[tokio::test]
    async fn rotation_commands_reach_the_scene() {
        let (handler, _worker) = fake_worker();
        let started = handler
            .handle(
                &CONNECTION,
                triangle::START_ROTATION,
                Some(json!({"angular_velocity_rad_per_s": 3.0})),
            )
            .await
            .unwrap();
        assert_eq!(started["rotating"], json!(true));
        assert_eq!(started["angular_velocity_rad_per_s"], json!(3.0));
        let stopped = handler
            .handle(&CONNECTION, triangle::STOP_ROTATION, None)
            .await
            .unwrap();
        assert_eq!(stopped["rotating"], json!(false));
        assert!(stopped["frame"].as_u64() > started["frame"].as_u64());
        let state = handler
            .handle(&CONNECTION, triangle::GET_STATE, Some(json!({})))
            .await
            .unwrap();
        assert_eq!(state["angle_rad"], stopped["angle_rad"]);
    }

    #[tokio::test]
    async fn rejects_bad_params_and_unknown_methods() {
        let (handler, _worker) = fake_worker();
        for (method, params) in [
            (
                triangle::START_ROTATION,
                json!({"angular_velocity_rad_per_s": "fast"}),
            ),
            (triangle::START_ROTATION, json!({"unknown": 1})),
            (triangle::STOP_ROTATION, json!({"now": true})),
            (webrtc::OFFER, json!({"type": "answer", "sdp": "v=0"})),
            (webrtc::OFFER, json!({"type": "offer", "sdp": "not sdp"})),
        ] {
            let error = handler
                .handle(&CONNECTION, method, Some(params.clone()))
                .await
                .unwrap_err();
            assert_eq!(error.code, INVALID_PARAMS, "{method} {params}");
        }
        let error = handler
            .handle(&CONNECTION, webrtc::OFFER, None)
            .await
            .unwrap_err();
        assert_eq!(error.code, INVALID_PARAMS);
        let error = handler
            .handle(&CONNECTION, "triangle.spin", None)
            .await
            .unwrap_err();
        assert_eq!(error.code, METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn reports_sdi_status() {
        let (handler, _worker) = fake_worker();
        let status = handler
            .handle(&CONNECTION, sdi::GET_STATUS, None)
            .await
            .unwrap();
        assert_eq!(status["video_mode"], json!("64x36p30"));
        assert_eq!(status["frames_output"], json!(0));
        assert_eq!(status["last_source_frame"], Value::Null);
        let error = handler
            .handle(&CONNECTION, sdi::GET_STATUS, Some(json!({"x": 1})))
            .await
            .unwrap_err();
        assert_eq!(error.code, INVALID_PARAMS);
    }

    #[tokio::test]
    async fn reports_a_stopped_render_loop() {
        let (commands, receiver) = mpsc::channel(1);
        drop(receiver);
        let handler = test_handler(commands);
        let error = handler
            .handle(&CONNECTION, triangle::GET_STATE, None)
            .await
            .unwrap_err();
        assert_eq!(error.code, SERVER_UNAVAILABLE);
    }
}
