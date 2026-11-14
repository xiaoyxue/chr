//! Control plane: maps JSON-RPC methods (the args clients send) onto the render
//! pipeline and the SDI port's status. WebRTC signaling belongs to `chr-monitor`.

use chr_common::{
    jsonrpc::{ErrorObject, SERVER_BUSY, SERVER_UNAVAILABLE},
    methods::{
        mesh, render_worker, sdi, tracking,
        triangle::{self, StartRotationParams, TriangleState},
    },
    server::{ConnectionInfo, Handler},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::{
    calibration::tracking::TrackingFeed, pipeline::Command, sdi::SdiStats,
    worker::supervisor::StatusHandle,
};

pub struct ControlHandler {
    commands: mpsc::Sender<Command>,
    sdi: SdiStats,
    worker: Option<StatusHandle>,
    scenes: Option<crate::retained::SceneControl>,
    tracking: Option<TrackingFeed>,
    tracker: Option<chr_tracking::supervisor::StatusHandle>,
    description: Option<std::sync::Arc<crate::program::description::Loader>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

impl ControlHandler {
    pub fn new(commands: mpsc::Sender<Command>, sdi: SdiStats) -> Self {
        Self {
            commands,
            sdi,
            worker: None,
            scenes: None,
            tracking: None,
            tracker: None,
            description: None,
        }
    }

    /// Accepts `description.load` for a render description program.
    pub fn with_description(
        mut self,
        loader: std::sync::Arc<crate::program::description::Loader>,
    ) -> Self {
        self.description = Some(loader);
        self
    }

    /// Accepts `tracking.frame` for a tracked program.
    pub fn with_tracking(mut self, feed: TrackingFeed) -> Self {
        self.tracking = Some(feed);
        self
    }

    /// Cameras come from the supervised tracking worker, reported through
    /// `tracking.get_worker_status`; `tracking.frame` is then rejected.
    pub fn with_tracking_worker(mut self, status: chr_tracking::supervisor::StatusHandle) -> Self {
        self.tracker = Some(status);
        self
    }

    fn tracking(&self, method: &str) -> Result<&TrackingFeed, ErrorObject> {
        self.tracking
            .as_ref()
            .ok_or_else(|| ErrorObject::method_not_found(method))
    }

    /// Reports the supervised render worker through `render.get_worker_status`.
    pub fn with_worker(mut self, status: StatusHandle) -> Self {
        self.worker = Some(status);
        self
    }

    /// Native opt-in only: the default daemon continues to render its triangle.
    pub fn with_scenes(mut self, scenes: crate::retained::SceneControl) -> Self {
        self.scenes = Some(scenes);
        self
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
            method if mesh::METHODS.contains(&method) => match &self.scenes {
                Some(scenes) => {
                    scenes
                        .rotate(method, raw_params, self.sdi.status().frames_output)
                        .await
                }
                None => Err(ErrorObject::method_not_found(method)),
            },
            method if chr_common::scene_frame::METHODS.contains(&method) => match &self.scenes {
                Some(scenes) => scenes.call(method, raw_params).await,
                None => Err(ErrorObject::method_not_found(method)),
            },
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
            tracking::FRAME => {
                let frame: tracking::TrackingFrame = raw_params
                    .ok_or_else(|| ErrorObject::invalid_params("tracking.frame needs params"))
                    .and_then(|value| {
                        serde_json::from_value(value)
                            .map_err(|error| ErrorObject::invalid_params(error.to_string()))
                    })?;
                if self.tracker.is_some() {
                    return Err(crate::retained::reject(
                        chr_common::scene_frame::RejectionKind::InvalidState,
                        "cameras come from the tracking worker (chr-server --tracking-worker)",
                    ));
                }
                self.tracking(method)?.push(frame)?;
                Ok(Value::Null)
            }
            chr_common::description::LOAD => {
                let loader = self
                    .description
                    .as_ref()
                    .ok_or_else(|| ErrorObject::method_not_found(method))?;
                let request = raw_params
                    .ok_or_else(|| ErrorObject::invalid_params("description.load needs params"))
                    .and_then(|value| {
                        serde_json::from_value(value)
                            .map_err(|error| ErrorObject::invalid_params(error.to_string()))
                    })?;
                let loaded = loader.load(request).await?;
                log::info!(
                    "{} {method}: scene revision {}",
                    connection.id,
                    loaded.scene_revision.get()
                );
                serde_json::to_value(loaded)
                    .map_err(|error| ErrorObject::internal(error.to_string()))
            }
            tracking::GET_STATUS => {
                params::<NoParams>(raw_params)?;
                serde_json::to_value(self.tracking(method)?.status())
                    .map_err(|error| ErrorObject::internal(error.to_string()))
            }
            tracking::GET_WORKER_STATUS => {
                params::<NoParams>(raw_params)?;
                let status = self
                    .tracker
                    .as_ref()
                    .ok_or_else(|| ErrorObject::method_not_found(method))?
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                serde_json::to_value(status)
                    .map_err(|error| ErrorObject::internal(error.to_string()))
            }
            render_worker::GET_STATUS => {
                params::<NoParams>(raw_params)?;
                let status = self
                    .worker
                    .as_ref()
                    .ok_or_else(|| ErrorObject::method_not_found(method))?
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                serde_json::to_value(status)
                    .map_err(|error| ErrorObject::internal(error.to_string()))
            }
            sdi::GET_STATUS => {
                params::<NoParams>(raw_params)?;
                serde_json::to_value(self.sdi.status())
                    .map_err(|error| ErrorObject::internal(error.to_string()))
            }
            _ => Err(ErrorObject::method_not_found(method)),
        }
    }
}

#[cfg(test)]
mod tests {
    use chr_common::{
        jsonrpc::{INVALID_PARAMS, METHOD_NOT_FOUND},
        methods::webrtc,
        server::{ConnectionId, Transport},
    };
    use serde_json::json;

    use super::*;
    use crate::{scene::Scene, sdi::VideoMode};

    fn test_handler(commands: mpsc::Sender<Command>) -> ControlHandler {
        let mode = VideoMode {
            width: 64,
            height: 36,
            fps: 30,
        };
        ControlHandler::new(commands, SdiStats::idle(mode))
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
        ] {
            let error = handler
                .handle(&CONNECTION, method, Some(params.clone()))
                .await
                .unwrap_err();
            assert_eq!(error.code, INVALID_PARAMS, "{method} {params}");
        }
        // WebRTC signaling moved to chr-monitor.
        for method in ["triangle.spin", webrtc::OFFER] {
            let error = handler.handle(&CONNECTION, method, None).await.unwrap_err();
            assert_eq!(error.code, METHOD_NOT_FOUND, "{method}");
        }
    }

    #[tokio::test]
    async fn reports_worker_status_only_with_a_worker() {
        let (handler, _worker) = fake_worker();
        let error = handler
            .handle(&CONNECTION, render_worker::GET_STATUS, None)
            .await
            .unwrap_err();
        assert_eq!(error.code, METHOD_NOT_FOUND);
        let status = crate::worker::supervisor::status_handle();
        status.lock().unwrap().clean_feed_frames = 3;
        let handler = handler.with_worker(status);
        let reported = handler
            .handle(&CONNECTION, render_worker::GET_STATUS, None)
            .await
            .unwrap();
        assert_eq!(reported["state"], json!("backoff"));
        assert_eq!(reported["clean_feed_frames"], json!(3));
        assert_eq!(reported["last_error"], Value::Null);
        let error = handler
            .handle(
                &CONNECTION,
                render_worker::GET_STATUS,
                Some(json!({"x": 1})),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, INVALID_PARAMS);
    }

    #[tokio::test]
    async fn a_tracking_worker_reports_status_and_rejects_external_cameras() {
        let (handler, _worker) = fake_worker();
        let feed = TrackingFeed::new(4);
        let handler = handler.with_tracking(feed.clone());
        let camera = json!({"sequence": "1", "ti": [0.02, 0, 0, 0, 0.02, 0, 0, 0, 1],
                            "camera_center": [0, 0, -10], "frame_warp": null});
        let error = handler
            .handle(&CONNECTION, tracking::GET_WORKER_STATUS, None)
            .await
            .unwrap_err();
        assert_eq!(error.code, METHOD_NOT_FOUND);
        handler
            .handle(&CONNECTION, tracking::FRAME, Some(camera.clone()))
            .await
            .unwrap();
        let status = chr_tracking::supervisor::status_handle();
        status.lock().unwrap().cameras = 2;
        let handler = handler.with_tracking_worker(status);
        let reported = handler
            .handle(&CONNECTION, tracking::GET_WORKER_STATUS, None)
            .await
            .unwrap();
        assert_eq!(
            (&reported["state"], &reported["cameras"]),
            (&json!("backoff"), &json!(2))
        );
        let mut external = camera;
        external["sequence"] = json!("2");
        let error = handler
            .handle(&CONNECTION, tracking::FRAME, Some(external))
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            chr_common::scene_frame::RejectionKind::InvalidState.code()
        );
        assert_eq!(feed.status().samples_received, 1);
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
