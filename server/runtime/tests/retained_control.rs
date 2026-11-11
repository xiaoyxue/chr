//! Synthetic CPU/control acceptance. This port never renders or transmits pixels.
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use chr_common::{jsonrpc::ErrorObject, scene_frame as w};
use chr_renderer::{
    camera::CameraView,
    coordinates::ImageExtent,
    glb::{GlbAsset, import_glb},
    scene::{EnvironmentMap, TextureData},
};
use chr_server::retained::{
    BoundSubmission, CameraContext, NativeSettings, Registrations, RenderAdmission, SceneAdapter,
    SceneControl,
};
use serde_json::{Value, json};

#[path = "retained_control/submissions.rs"]
mod submissions;

fn id(value: &str) -> w::Id {
    w::Id::try_from(value.to_owned()).unwrap()
}
fn error(kind: w::RejectionKind, message: &str) -> ErrorObject {
    w::Rejection {
        kind,
        message: message.into(),
        frame: None,
    }
    .into()
}
fn fixture(name: &str, side: &str) -> Value {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../common/tests/fixtures/scene_frame.json"
    ))
    .unwrap();
    fixtures["calls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap()[side]
        .clone()
}
fn config() -> w::Configuration {
    serde_json::from_value(fixture("configuration", "result")).unwrap()
}

fn model() -> GlbAsset {
    // One +Y-up triangle with a translated node; importer must bake the basis once.
    let mut bin = Vec::new();
    for n in [0_f32, 0., 0., 2., 0., 0., 0., 1., 0.] {
        bin.extend(n.to_le_bytes());
    }
    let mut document = json!({
        "asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],
        "nodes":[{"mesh":0,"translation":[0,2,0]}],"buffers":[{"byteLength":36}],
        "bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36}],
        "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[2,1,0]}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}]
    }).to_string().into_bytes();
    while !document.len().is_multiple_of(4) {
        document.push(b' ');
    }
    let mut glb = Vec::from(*b"glTF");
    glb.extend(2_u32.to_le_bytes());
    glb.extend((28 + document.len() as u32 + bin.len() as u32).to_le_bytes());
    glb.extend((document.len() as u32).to_le_bytes());
    glb.extend(*b"JSON");
    glb.extend(document);
    glb.extend((bin.len() as u32).to_le_bytes());
    glb.extend(*b"BIN\0");
    glb.extend(bin);
    let mut asset = import_glb(&glb).unwrap();
    asset.materials[0].maps.base_color = Some(Arc::new(
        TextureData::new(ImageExtent::new(1, 1).unwrap(), vec![255, 64, 16, 255]).unwrap(),
    ));
    asset
}

fn registrations() -> Registrations {
    let mut registry = Registrations::default();
    let world =
        serde_json::from_value(fixture("create", "params")["state"]["world"].clone()).unwrap();
    registry.register_world(world).unwrap();
    registry
        .register_model(id("synthetic-cube"), model())
        .unwrap();
    registry
        .register_environment(
            id("synthetic-exr"),
            EnvironmentMap::new(ImageExtent::new(2, 1).unwrap(), vec![[1.; 3]; 2]).unwrap(),
        )
        .unwrap();
    registry
        .register_context(
            id("synthetic-context"),
            CameraContext {
                world: serde_json::from_value(
                    fixture("create", "params")["state"]["world"].clone(),
                )
                .unwrap(),
                extent: config().native_extent,
                camera_center: [0., 0., -10.].map(|n| w::Finite::try_from(n).unwrap()),
            },
        )
        .unwrap();
    registry
}

#[derive(Default)]
struct PortState {
    accepted: Vec<BoundSubmission>,
    cancelled: HashSet<w::Id>,
}
struct MockPort(Arc<Mutex<PortState>>);

impl RenderAdmission for MockPort {
    fn configure(
        &mut self,
        config: &w::Configuration,
        native: NativeSettings,
    ) -> Result<(), ErrorObject> {
        if config.native_extent.width > 1280 || config.native_extent.height > 720 {
            return Err(error(
                w::RejectionKind::UnsupportedProfile,
                "mock supports at most 1280x720",
            ));
        }
        assert_eq!(native.output.pbr.exposure, config.exposure.get() as f32);
        Ok(())
    }
    fn existing(&self, request: &w::Submit) -> Result<Option<w::SubmissionAccepted>, ErrorObject> {
        let state = self.0.lock().unwrap();
        let Some(old) = state
            .accepted
            .iter()
            .find(|v| v.request.submission_id == request.submission_id)
        else {
            return Ok(None);
        };
        if old.request != *request {
            return Err(error(
                w::RejectionKind::Conflict,
                "submission content differs",
            ));
        }
        Ok(Some(old.receipt.clone()))
    }
    fn admit(&mut self, submission: BoundSubmission) -> Result<(), ErrorObject> {
        let mut state = self.0.lock().unwrap();
        if state.accepted.len() >= 2
            || state
                .accepted
                .iter()
                .map(|s| s.request.frames.len())
                .sum::<usize>()
                + submission.request.frames.len()
                > 2
        {
            return Err(error(
                w::RejectionKind::Capacity,
                "declared two-frame mock port is full",
            ));
        }
        // A fixed, pre-registered synthetic resource set, NOT the production #21 owner.
        for frame in &submission.request.frames {
            if frame.image_ref != id("image-17") && frame.image_ref != id("image-18") {
                return Err(error(
                    w::RejectionKind::NotFound,
                    "mock image is not registered",
                ));
            }
            let sequence = if frame.image_ref == id("image-17") {
                17
            } else {
                18
            };
            if frame.frame.stream_id != id("stream-01")
                || frame.frame.sequence.get() != sequence
                || frame.position.timeline_id != id("capture-01")
                || frame.position.ticks.get() != sequence
            {
                return Err(error(
                    w::RejectionKind::IncompatibleInput,
                    "mock frame metadata differs",
                ));
            }
        }
        state.accepted.push(submission);
        Ok(())
    }
    fn get(&self, target: &w::SubmissionTarget) -> Result<w::SubmissionSnapshot, ErrorObject> {
        let state = self.0.lock().unwrap();
        let submission = state
            .accepted
            .iter()
            .find(|v| v.request.submission_id == target.submission_id)
            .ok_or_else(|| error(w::RejectionKind::NotFound, "mock submission absent"))?;
        let receipt = &submission.receipt;
        Ok(w::SubmissionSnapshot {
            runtime_id: receipt.runtime_id.clone(),
            submission_id: receipt.submission_id.clone(),
            scene_id: receipt.scene_id.clone(),
            scene_revision: receipt.scene_revision,
            effect: receipt.effect.clone(),
            frames: receipt
                .frames
                .iter()
                .map(|f| {
                    if state.cancelled.contains(&receipt.submission_id) {
                        w::FrameResult::Failed {
                            frame: f.frame.clone(),
                            position: f.position.clone(),
                            reason: w::FailureKind::Cancelled,
                        }
                    } else {
                        w::FrameResult::Pending {
                            frame: f.frame.clone(),
                            position: f.position.clone(),
                        }
                    }
                })
                .collect(),
        })
    }
    fn cancel(&mut self, target: &w::SubmissionTarget) -> Result<w::CancelResult, ErrorObject> {
        let snapshot = self.get(target)?;
        let mut state = self.0.lock().unwrap();
        let first = state.cancelled.insert(target.submission_id.clone());
        let frames = snapshot.frames.iter().map(|f| f.frame().clone()).collect();
        Ok(w::CancelResult {
            runtime_id: target.runtime_id.clone(),
            submission_id: target.submission_id.clone(),
            cancelled: if first { frames } else { Vec::new() },
            too_late: if first {
                Vec::new()
            } else {
                snapshot
                    .frames
                    .into_iter()
                    .map(|f| f.frame().clone())
                    .collect()
            },
        })
    }
    fn release(&mut self, target: &w::SubmissionTarget) -> Result<w::ReleaseResult, ErrorObject> {
        let mut state = self.0.lock().unwrap();
        let Some(index) = state
            .accepted
            .iter()
            .position(|v| v.request.submission_id == target.submission_id)
        else {
            return Ok(w::ReleaseResult { released: false });
        };
        if !state.cancelled.remove(&target.submission_id) {
            return Err(error(
                w::RejectionKind::InvalidState,
                "mock submission is still pending",
            ));
        }
        state.accepted.remove(index);
        Ok(w::ReleaseResult { released: true })
    }
}

fn setup(
    config: w::Configuration,
    capacity: usize,
) -> (
    SceneControl,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<PortState>>,
) {
    let state = Arc::new(Mutex::new(PortState::default()));
    let owner = SceneAdapter::new(
        config,
        registrations(),
        Box::new(MockPort(Arc::clone(&state))),
    )
    .unwrap();
    let (control, worker) = SceneControl::spawn(owner, capacity).unwrap();
    (control, worker, state)
}
async fn call(control: &SceneControl, method: &str, params: Value) -> Value {
    let result = control.call(method, Some(params)).await.unwrap();
    w::validate_result(method, result.clone()).unwrap();
    result
}
fn target(scene: &str) -> Value {
    json!({"runtime_id":"runtime-01","scene_id":scene})
}
fn submission_target() -> Value {
    json!({"runtime_id":"runtime-01","submission_id":"submission-17"})
}
fn mutate(operations: Value, revision: u64) -> Value {
    json!({"runtime_id":"runtime-01","scene_id":"scene-demo","expected_revision":revision.to_string(),"operations":operations})
}

#[tokio::test]
async fn failed_native_ranges_and_budgets_never_publish_a_scene() {
    let (control, worker, _) = setup(config(), 8);
    for (pointer, replacement) in [
        ("/state/instances/0/model/0/0", json!(0)),
        ("/state/instances/0/model/0/3", json!(1e100)),
        (
            "/state/instances/0/material_overrides/0/factors/metallic",
            json!(1.00000000001),
        ),
        (
            "/state/instances/0/material_overrides/0/factors/emissive/0",
            json!(-1e-100),
        ),
        ("/state/lighting/lights/0/direction", json!([1e300, 0, 0])),
        ("/state/lighting/lights/1/source_radius", json!(-1e-100)),
        ("/state/camera/camera/camera_to_world/0/0", json!(-1)),
        ("/state/camera/camera/intrinsics/fx", json!(-1)),
        ("/state/configuration_id", json!("other")),
    ] {
        let mut create = fixture("create", "params");
        *create.pointer_mut(pointer).unwrap() = replacement;
        assert_eq!(
            control
                .call(w::SCENE_CREATE, Some(create))
                .await
                .unwrap_err()
                .code,
            2004,
            "{pointer}"
        );
        assert_eq!(
            control
                .call(w::SCENE_GET, Some(target("scene-demo")))
                .await
                .unwrap_err()
                .code,
            2002
        );
    }
    assert_eq!(
        call(&control, w::SCENE_CREATE, fixture("create", "params")).await["effect"]["sequence"],
        "1"
    );
    let typed = control
        .call_typed(
            w::SCENE_GET,
            w::Params::SceneGet(Box::new(w::Target {
                runtime_id: id("runtime-01"),
                scene_id: id("scene-demo"),
            })),
        )
        .await
        .unwrap();
    assert_eq!(typed["scene_revision"], "0");
    drop(control);
    worker.await.unwrap();

    let mut low = config();
    low.shadows.max_bytes = w::U64::from(1);
    let (control, worker, _) = setup(low, 8);
    assert_eq!(
        control
            .call(w::SCENE_CREATE, Some(fixture("create", "params")))
            .await
            .unwrap_err()
            .code,
        2005
    );
    drop(control);
    worker.await.unwrap();

    for (pointer, replacement) in [
        ("/exposure", json!(-1)),
        ("/shadows/depth_bias", json!(2)),
        ("/shadows/normal_bias", json!(-1)),
        ("/shadows/pcss/max_search_radius", json!(1e-100)),
        ("/max_intermediate_bytes", json!("1")),
    ] {
        let mut raw = fixture("configuration", "result");
        *raw.pointer_mut(pointer).unwrap() = replacement;
        let port = MockPort(Arc::new(Mutex::new(PortState::default())));
        assert!(
            SceneAdapter::new(
                serde_json::from_value(raw).unwrap(),
                registrations(),
                Box::new(port)
            )
            .is_err(),
            "{pointer}"
        );
    }
}

#[tokio::test]
async fn ordered_mapping_rollback_and_bound_lifetimes() {
    let (control, worker, port) = setup(config(), 8);
    let mut create = fixture("create", "params");
    create["state"]["lighting"]["environment"] =
        json!({"asset_ref":"synthetic-exr","gain":1,"rotation":0.3,"visible":false});
    let mut second = create["state"]["instances"][0].clone();
    second["instance_id"] = json!("second");
    second["material_overrides"] = json!([]);
    create["state"]["instances"]
        .as_array_mut()
        .unwrap()
        .push(second);
    assert_eq!(
        call(&control, w::SCENE_CREATE, create.clone()).await["effect"]["sequence"],
        "1"
    );
    let initial = call(&control, w::SCENE_GET, target("scene-demo")).await;
    for operations in [
        json!([{"kind":"instance_put","instance":{ "instance_id":"bad","asset_ref":"missing","model":[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]],"visible":true,"material_overrides":[]}},{"kind":"instance_remove","instance_id":"bad"}]),
        json!([{"kind":"instance_remove","instance_id":"cube"},{"kind":"instance_remove","instance_id":"absent"}]),
        json!([{"kind":"camera_set","camera":{"kind":"ti_c","extent":{"width":1280,"height":720},"near_m":0.1,"far_m":1000,"ti":[0,0,0,0,0,0,0,0,0],"camera_center":[0,0,-10]}},{"kind":"camera_set","camera":create["state"]["camera"]["camera"]}]),
        json!([{"kind":"lighting_set","lighting":{"lights":[{"kind":"directional","light_id":"bad","direction":[0,0,0],"irradiance":[1,1,1],"angular_radius":0,"shadow_mode":null}],"environment":null}},{"kind":"lighting_set","lighting":create["state"]["lighting"]}]),
    ] {
        assert!(
            control
                .call(w::SCENE_MUTATE, Some(mutate(operations, 0)))
                .await
                .is_err()
        );
        assert_eq!(
            call(&control, w::SCENE_GET, target("scene-demo")).await,
            initial
        );
    }
    let mut invalid = create.clone();
    invalid["scene_id"] = json!("invalid");
    invalid["state"]["instances"][0]["material_overrides"][0]["part"] = json!(99);
    assert_eq!(
        control
            .call(w::SCENE_CREATE, Some(invalid))
            .await
            .unwrap_err()
            .code,
        2004
    );
    let submit = fixture("submit", "params");
    let mut bad_ref = submit.clone();
    bad_ref["frames"][0]["image_ref"] = json!("absent");
    assert_eq!(
        control
            .call(w::RENDER_SUBMIT, Some(bad_ref))
            .await
            .unwrap_err()
            .code,
        2002
    );
    assert!(port.lock().unwrap().accepted.is_empty());
    let accepted = call(&control, w::RENDER_SUBMIT, submit.clone()).await;
    assert_eq!(accepted["effect"]["sequence"], "2");
    {
        let state = port.lock().unwrap();
        let bound = &state.accepted[0];
        let view = bound.scene.scene();
        let instances: Vec<_> = view.instances().collect();
        assert_eq!(
            view.mesh(instances[0].1.mesh()).unwrap().positions()[0],
            [0., 0., -2.]
        );
        assert_eq!(
            view.mesh(instances[0].1.mesh())
                .unwrap()
                .positions()
                .as_ptr(),
            view.mesh(instances[1].1.mesh())
                .unwrap()
                .positions()
                .as_ptr()
        );
        let a = instances[0].1.materials()[0];
        let b = instances[1].1.materials()[0];
        assert!(Arc::ptr_eq(
            view.material_maps(a).unwrap().base_color.as_ref().unwrap(),
            view.material_maps(b).unwrap().base_color.as_ref().unwrap()
        ));
        assert_eq!(view.material(a).unwrap().metallic, 0.);
        assert_eq!(view.material(b).unwrap().metallic, 1.);
        assert!(!view.lighting().sky().unwrap().visible);
        assert_eq!(
            bound.scene.settings().output.pbr.shadows.mode,
            chr_renderer::ShadowMode::Pcf
        );
        assert_eq!(bound.scene.settings().raster.sample_count, 4);
        assert_eq!(bound.cameras[0].center(), [0., 0., -10.]);
    }
    let changed = call(
        &control,
        w::SCENE_MUTATE,
        mutate(
            json!([
                {"kind":"instance_remove","instance_id":"cube"},
                {"kind":"instance_put","instance":create["state"]["instances"][0]},
                {"kind":"shadow_mode_set","shadow_mode":"pcss"}
            ]),
            0,
        ),
    )
    .await;
    assert_eq!(changed["scene_revision"], "1");
    assert_eq!(changed["effect"]["sequence"], "3");
    assert_eq!(
        port.lock().unwrap().accepted[0]
            .scene
            .snapshot()
            .scene_revision
            .get(),
        0
    );
    assert_eq!(
        call(&control, w::RENDER_SUBMIT, submit.clone()).await,
        accepted
    );
    let mut different = submit.clone();
    different["expected_scene_revision"] = json!("1");
    assert_eq!(
        control
            .call(w::RENDER_SUBMIT, Some(different))
            .await
            .unwrap_err()
            .code,
        2003
    );
    call(&control, w::SCENE_RELEASE, target("scene-demo")).await;
    assert_eq!(call(&control, w::RENDER_SUBMIT, submit).await, accepted);
    call(&control, w::SCENE_CREATE, create).await;
    assert_eq!(
        port.lock().unwrap().accepted[0]
            .scene
            .snapshot()
            .effect
            .sequence
            .get(),
        1
    );
    assert_eq!(
        control
            .call(w::RENDER_RELEASE, Some(submission_target()))
            .await
            .unwrap_err()
            .code,
        2006
    );
    assert_eq!(
        call(&control, w::RENDER_GET, submission_target()).await["frames"][0]["state"],
        "pending"
    );
    call(&control, w::RENDER_CANCEL, submission_target()).await;
    call(&control, w::RENDER_RELEASE, submission_target()).await;
    drop(control);
    worker.await.unwrap();
}

#[tokio::test]
async fn tracked_context_extent_world_and_camera_math_reject_atomically() {
    let (control, worker, port) = setup(config(), 8);
    let mut create = fixture("create", "params");
    create["scene_id"] = json!("scene-tracked");
    create["state"]["camera"] = json!({"mode":"tracked","near_m":0.25,"far_m":500});
    call(&control, w::SCENE_CREATE, create).await;
    let mut submit = fixture("tracked_submit", "params");
    submit["frames"][0]["geometry"]["context_id"] = json!("synthetic-context");
    for (pointer, replacement, code) in [
        ("/frames/0/geometry/world/revision", json!("2"), 2004),
        ("/frames/0/geometry/context_id", json!("missing"), 2002),
        ("/frames/0/geometry/camera_center", json!([0, 0, -11]), 2004),
        ("/frames/0/geometry/extent/width", json!(640), 2004),
        (
            "/frames/0/geometry/ti",
            json!([0, 0, 0, 0, 0, 0, 0, 0, 0]),
            2004,
        ),
    ] {
        let mut bad = submit.clone();
        *bad.pointer_mut(pointer).unwrap() = replacement;
        let failure = control.call(w::RENDER_SUBMIT, Some(bad)).await.unwrap_err();
        assert_eq!(failure.code, code);
        assert_eq!(failure.data.unwrap()["frame"]["sequence"], "18");
        assert!(port.lock().unwrap().accepted.is_empty());
    }
    let mut camera_set = mutate(
        json!([{"kind":"camera_set","camera":fixture("create","params")["state"]["camera"]["camera"]}]),
        0,
    );
    camera_set["scene_id"] = json!("scene-tracked");
    assert_eq!(
        control
            .call(w::SCENE_MUTATE, Some(camera_set))
            .await
            .unwrap_err()
            .code,
        2004
    );
    let result = call(&control, w::RENDER_SUBMIT, submit).await;
    assert_eq!(result["effect"]["sequence"], "2");
    {
        let state = port.lock().unwrap();
        let camera = &state.accepted[0].cameras[0];
        assert_eq!(camera.depth_range(), [0.25, 500.]);
        assert_eq!(
            camera.source_ti().unwrap().rows(),
            [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]]
        );
        let pixel = camera.project([0., 0., 0.]).unwrap().0;
        assert!((pixel[0] - 640.).abs() < 1e-9 && (pixel[1] - 360.).abs() < 1e-9);
    }
    drop(control);
    worker.await.unwrap();
}

#[tokio::test]
async fn declared_capacity_and_queue_rejections_do_not_apply_effects() {
    let mut configuration = config();
    configuration.limits.scenes = 1;
    let (control, worker, port) = setup(configuration, 1);
    let mut queued = Box::pin(control.call(w::SCENE_CREATE, Some(fixture("create", "params"))));
    assert!(futures_util::poll!(queued.as_mut()).is_pending());
    assert_eq!(
        control
            .call(w::SCENE_GET, Some(target("scene-demo")))
            .await
            .unwrap_err()
            .code,
        2005
    );
    queued.await.unwrap();
    call(&control, w::RENDER_SUBMIT, fixture("submit", "params")).await;
    call(&control, w::SCENE_RELEASE, target("scene-demo")).await;
    assert_eq!(
        control
            .call(w::SCENE_CREATE, Some(fixture("create", "params")))
            .await
            .unwrap_err()
            .code,
        2005
    );
    call(&control, w::RENDER_CANCEL, submission_target()).await;
    call(&control, w::RENDER_RELEASE, submission_target()).await;
    assert!(port.lock().unwrap().accepted.is_empty());
    call(&control, w::SCENE_CREATE, fixture("create", "params")).await;
    let mut bad = fixture("submit", "params");
    bad["frames"]
        .as_array_mut()
        .unwrap()
        .push(fixture("tracked_submit", "params")["frames"][0].clone());
    bad["frames"][1]["geometry"] = json!({"kind":"scene_camera"});
    bad["frames"][1]["image_ref"] = json!("absent");
    assert_eq!(
        control
            .call(w::RENDER_SUBMIT, Some(bad))
            .await
            .unwrap_err()
            .code,
        2002
    );
    assert!(port.lock().unwrap().accepted.is_empty());
    call(&control, w::RENDER_SUBMIT, fixture("submit", "params")).await;
    let mut another = fixture("submit", "params");
    another["submission_id"] = json!("second");
    call(&control, w::RENDER_SUBMIT, another.clone()).await;
    another["submission_id"] = json!("third");
    assert_eq!(
        control
            .call(w::RENDER_SUBMIT, Some(another))
            .await
            .unwrap_err()
            .code,
        2005
    );
    assert_eq!(port.lock().unwrap().accepted.len(), 2);
    drop(control);
    worker.await.unwrap();
}

#[cfg(unix)]
mod ipc {
    use super::*;
    use chr_common::{
        client::NativeSocketClient,
        methods::{sdi, triangle, webrtc},
        server::{IpcServer, NativeSocketListener, ServerConfig, WebSocketListener},
    };
    use chr_server::{
        control::ControlHandler,
        monitor::{
            encoder::VideoFeed,
            webrtc::{IceConfig, WebRtcHub},
        },
        sdi::{SdiOutput, VideoMode},
    };
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn both_ipcs_share_the_real_handler_queue_scene_and_admission_boundary() {
        tokio::time::timeout(std::time::Duration::from_secs(20), conformance())
            .await
            .unwrap();
    }

    async fn conformance() {
        let (control, worker, port) = setup(config(), 8);
        let (commands, receiver) = tokio::sync::mpsc::channel(1);
        drop(receiver);
        let hub = Arc::new(WebRtcHub::new(
            Arc::new(VideoFeed::new()),
            50,
            IceConfig::default(),
        ));
        let (sdi_port, _tap) = SdiOutput::spawn(VideoMode {
            width: 1280,
            height: 720,
            fps: 50,
        })
        .unwrap();
        let handler =
            Arc::new(ControlHandler::new(commands, hub, sdi_port.stats()).with_scenes(control));
        let server = IpcServer::new(Arc::clone(&handler), ServerConfig::default());
        // Relative addresses avoid Unix sun_path limits in deep feature worktrees.
        let directory = std::path::PathBuf::from("../../output/runtime-scene-adapter");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("control-{}.sock", std::process::id()));
        let native_listener = NativeSocketListener::bind(&path).await.unwrap();
        let websocket_listener = WebSocketListener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let url = format!("ws://{}", websocket_listener.local_addr().unwrap());
        let native_server = server.clone();
        let native_task =
            tokio::spawn(async move { native_server.serve_native_socket(native_listener).await });
        let websocket_task =
            tokio::spawn(async move { server.serve_websocket(websocket_listener).await });
        let mut native = NativeSocketClient::connect(&path).await.unwrap();
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let mut unknown_image = fixture("submit", "params");
        unknown_image["frames"][0]["image_ref"] = json!("unknown");
        let sequence = [
            (w::GET_CONFIGURATION, fixture("configuration", "params")),
            (w::SCENE_CREATE, fixture("create", "params")),
            (w::SCENE_GET, target("scene-demo")),
            (
                w::SCENE_MUTATE,
                mutate(
                    json!([{"kind":"shadow_mode_set","shadow_mode":"pcss"},{"kind":"camera_set","camera":fixture("mutate","params")["operations"][3]["camera"]},{"kind":"lighting_set","lighting":fixture("create","params")["state"]["lighting"]},{"kind":"instance_put","instance":fixture("create","params")["state"]["instances"][0]}]),
                    0,
                ),
            ),
            (w::SCENE_GET, target("scene-demo")),
            (w::RENDER_SUBMIT, unknown_image),
            (w::RENDER_SUBMIT, fixture("submit", "params")),
            (w::RENDER_GET, submission_target()),
            (
                w::SCENE_GET,
                json!({"runtime_id":"old","scene_id":"scene-demo"}),
            ),
            (sdi::GET_STATUS, json!({})),
            (webrtc::OFFER, json!({"type":"answer","sdp":"v=0"})),
            (triangle::GET_STATE, json!({})),
            (w::FRAME_PUBLISH, fixture("publish", "params")),
        ];
        for (index, (method, params)) in sequence.into_iter().enumerate() {
            let request =
                json!({"jsonrpc":"2.0","id":index,"method":method,"params":params}).to_string();
            let first: Value = if index % 2 == 0 {
                serde_json::from_slice(&native.send_raw(request.as_bytes()).await.unwrap().unwrap())
                    .unwrap()
            } else {
                ws.send(Message::text(request.clone())).await.unwrap();
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap()
            };
            let second: Value = if index % 2 == 0 {
                ws.send(Message::text(request)).await.unwrap();
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap()
            } else {
                serde_json::from_slice(&native.send_raw(request.as_bytes()).await.unwrap().unwrap())
                    .unwrap()
            };
            if method == w::SCENE_CREATE || method == w::SCENE_MUTATE {
                assert!(first["result"].is_object(), "{first}");
                assert_eq!(second["error"]["code"], 2003);
            } else if method == sdi::GET_STATUS {
                assert_eq!(
                    first["result"]["video_mode"],
                    second["result"]["video_mode"]
                );
                assert_eq!(first["result"]["frames_output"], 0);
                assert_eq!(second["result"]["frames_output"], 0);
            } else {
                assert_eq!(first, second, "{method}");
            }
            if method == w::RENDER_SUBMIT && first["result"].is_object() {
                assert_eq!(first["result"]["scene_revision"], "1");
                assert_eq!(first["result"]["effect"]["sequence"], "3");
                assert_eq!(port.lock().unwrap().accepted.len(), 1);
            }
            if method == w::RENDER_SUBMIT && first["error"].is_object() {
                assert_eq!(first["error"]["code"], 2002);
                assert!(port.lock().unwrap().accepted.is_empty());
            }
        }
        // Malformed/excluded input is rejected on the real transport boundary.
        let mut bad = fixture("create", "params");
        bad["state"]["camera"]["camera"]["distortion"] = json!([]);
        let request =
            json!({"jsonrpc":"2.0","id":"bad","method":w::SCENE_CREATE,"params":bad}).to_string();
        let response: Value =
            serde_json::from_slice(&native.send_raw(request.as_bytes()).await.unwrap().unwrap())
                .unwrap();
        assert_eq!(response["error"]["code"], -32602);
        ws.close(None).await.unwrap();
        drop(ws);
        drop(native);
        native_task.abort();
        websocket_task.abort();
        let _ = native_task.await;
        let _ = websocket_task.await;
        drop(handler);
        worker.await.unwrap();
        drop(sdi_port);
        assert!(!path.exists());
    }
}
