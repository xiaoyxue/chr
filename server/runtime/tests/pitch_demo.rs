use chr_common::scene_frame as w;
use chr_renderer::{
    Gpu,
    camera::{Camera, Intrinsics},
    coordinates::{ImageExtent, Ti},
    scene::ModelMatrix,
};
use chr_server::{
    program::{
        ProgramRenderer,
        pitch::{self, CameraFixture, Prepared},
        synthetic::{Synthetic, id, runtime_id},
    },
    retained::SceneControl,
    sdi::{SdiInput, SdiOutput, SdiSlot, VideoMode},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/pitch-demo.json"
);
fn mode() -> VideoMode {
    VideoMode {
        width: 640,
        height: 360,
        fps: 25,
    }
}
fn fixture() -> CameraFixture {
    serde_json::from_str(include_str!("fixtures/pitch-camera.json")).unwrap()
}
fn install() -> Prepared {
    Prepared::load(Path::new(FIXTURE), mode(), runtime_id().unwrap()).unwrap()
}

#[test]
fn all_parts_rotated_nonuniform_mirrored_grounding_and_original_projection() {
    let asset = pitch::block().unwrap();
    assert_eq!(asset.mesh.parts().len(), 2);
    for mirror in [-1., 1.] {
        let model = ModelMatrix::new([
            [2. * mirror, 0., 0., 5.],
            [0., 0., -3., 6.],
            [0., 4., 0., 20.],
            [0., 0., 0., 1.],
        ])
        .unwrap()
        .grounded(asset.mesh.bounds())
        .unwrap();
        let bounds = asset.mesh.bounds().transformed(model).unwrap();
        assert!(bounds.max()[2].abs() < 1e-12);
        assert!(bounds.min()[2] < -1.);
        for point in asset.mesh.positions() {
            assert!(model.transform_point(point.map(f64::from)).unwrap()[2] <= 0.);
        }
    }
    let f = fixture();
    let original = Camera::from_ti(
        ImageExtent::new(f.width, f.height).unwrap(),
        Ti::new(f.ti).unwrap(),
        f.center,
        0.1,
        10000.,
    )
    .unwrap();
    let preview = Camera::from_pose(
        original.extent(),
        original.intrinsics(),
        original.camera_to_world(),
        0.1,
        10000.,
    )
    .unwrap();
    for point in [[-30., 20., 0.], [8., -12., -7.], [0., 0., -20.]] {
        assert_eq!(
            original.project(point).unwrap(),
            preview.project(point).unwrap()
        );
    }
    assert_eq!(original.source_ti().unwrap().rows(), f.ti);
    let prepared = install();
    let selected = &prepared.controls["models"][1]["instance"];
    let model =
        ModelMatrix::new(serde_json::from_value(selected["model"].clone()).unwrap()).unwrap();
    let bounds = asset.mesh.bounds().transformed(model).unwrap();
    assert!((bounds.max()[2] - bounds.min()[2] - 10.).abs() < 1e-10);
    assert!(bounds.max()[2].abs() < 1e-10);
    let anchor = original.ray([400., 230.]).unwrap();
    let distance = -anchor.origin[2] / anchor.direction[2];
    for axis in 0..2 {
        assert!(
            (model.rows()[axis][3] - anchor.origin[axis] - distance * anchor.direction[axis]).abs()
                < 1e-10
        );
    }
}

#[test]
fn authored_corners_targets_exclusive_rigs_and_invalid_metadata() {
    for height in [30., 35., 40.] {
        let towers = pitch::towers([100., 64.], height).unwrap();
        assert_eq!(towers.len(), 4);
        for (i, p) in [[-50., -32.], [-50., 32.], [50., -32.], [50., 32.]]
            .iter()
            .enumerate()
        {
            assert_eq!(towers[i]["model"][0][3], p[0]);
            assert_eq!(towers[i]["model"][1][3], p[1]);
            assert_eq!(towers[i]["model"][2][2], (height - 1.) / 2.);
        }
        for rig in ["none", "points", "spots"] {
            for sun in [false, true] {
                let targets = [[10., 5.], [-10., 5.], [10., -5.], [-10., -5.]];
                let lighting = pitch::lighting([100., 64.], height, rig, sun, targets).unwrap();
                let lights = lighting["lights"].as_array().unwrap();
                assert_eq!(
                    lights.len(),
                    if rig == "none" { 0 } else { 4 } + usize::from(sun)
                );
                for (i, light) in lights.iter().filter(|l| l["kind"] == "spot").enumerate() {
                    for axis in 0..2 {
                        assert_eq!(
                            light["position"][axis].as_f64().unwrap()
                                + light["direction"][axis].as_f64().unwrap(),
                            targets[i][axis]
                        );
                    }
                    assert_eq!(
                        light["position"][2].as_f64().unwrap()
                            + light["direction"][2].as_f64().unwrap(),
                        0.
                    );
                }
            }
        }
    }
    assert!(pitch::towers([100., 64.], 29.9).is_err());
    assert!(pitch::towers([100., 64.], 40.1).is_err());
    for invalid in 0..5 {
        let mut f = fixture();
        match invalid {
            0 => f.source_frame.clear(),
            1 => f.ground_status.clear(),
            2 => f.pitch[0] = 0.,
            3 => f.ti = [[0.; 3]; 3],
            _ => f.center[2] = 0.,
        }
        assert!(
            Prepared::new(
                f,
                vec![(pitch::block().unwrap(), ModelMatrix::IDENTITY)],
                mode(),
                runtime_id().unwrap()
            )
            .is_err()
        );
    }
    let mut missing: Value =
        serde_json::from_str(include_str!("fixtures/pitch-camera.json")).unwrap();
    missing.as_object_mut().unwrap().remove("source_frame");
    assert!(serde_json::from_value::<CameraFixture>(missing).is_err());
    assert!(
        serde_json::from_value::<pitch::ModelFixture>(
            json!({"model":ModelMatrix::IDENTITY.rows()})
        )
        .is_err(),
        "missing asset path must not silently select a synthetic model"
    );
}

async fn query(control: &SceneControl, p: &Synthetic) -> Value {
    control
        .call(
            w::SCENE_GET,
            Some(json!({"runtime_id":p.configuration.runtime_id,"scene_id":p.scene_id})),
        )
        .await
        .unwrap()
}
async fn mutate(control: &SceneControl, p: &Synthetic, operations: Value) {
    let state = query(control, p).await;
    control
        .call(
            w::SCENE_MUTATE,
            Some(json!({
                "runtime_id":p.configuration.runtime_id,"scene_id":p.scene_id,
                "expected_revision":state["scene_revision"],"operations":operations,
            })),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn named_installation_reset_is_exact_and_native_resources_stay_shared() {
    let Prepared {
        mut program,
        adapter,
        controls,
    } = install();
    assert_eq!(program.configuration.limits.lights_per_scene, 5);
    let (control, owner) = SceneControl::spawn(adapter, 64).unwrap();
    program.create_scene(&control).await.unwrap();
    let original = controls["original_camera"].clone();
    let before = query(&control, &program).await;
    assert_eq!(before["state"]["camera"]["camera"], original);
    let mut preview = controls["preview_camera"].clone();
    preview["intrinsics"]["fx"] = json!(800.);
    mutate(
        &control,
        &program,
        json!([{"kind":"camera_set","camera":preview}]),
    )
    .await;
    mutate(
        &control,
        &program,
        json!([{"kind":"camera_set","camera":original}]),
    )
    .await;
    assert_eq!(
        query(&control, &program).await["state"]["camera"],
        before["state"]["camera"]
    );
    assert_eq!(controls["source_frame"], "synthetic-pitch-frame-0007");
    // Admission binds camera edits without replacing the model payloads.
    program.inject(&control).await.unwrap();
    let first = program.scheduler.next_ready().unwrap().unwrap();
    let mesh = first.scene.scene().instances().next().unwrap().1.mesh();
    let positions = first.scene.scene().mesh(mesh).unwrap().positions().as_ptr();
    mutate(
        &control,
        &program,
        json!([{"kind":"camera_set","camera":controls["preview_camera"]}]),
    )
    .await;
    program.inject(&control).await.unwrap();
    let second = program.scheduler.next_ready().unwrap().unwrap();
    let mesh = second.scene.scene().instances().next().unwrap().1.mesh();
    assert_eq!(
        positions,
        second
            .scene
            .scene()
            .mesh(mesh)
            .unwrap()
            .positions()
            .as_ptr()
    );
    first.failed().unwrap();
    second.failed().unwrap();
    drop(control);
    owner.await.unwrap();
}

async fn frame(
    control: &SceneControl,
    program: &mut Synthetic,
    renderer: &mut ProgramRenderer,
    gpu: &Gpu,
    port: &mut SdiInput,
    tap: &mpsc::Receiver<SdiSlot>,
    sequence: &mut u64,
) -> (Arc<Vec<u8>>, f64) {
    let submission = program.inject(control).await.unwrap().unwrap();
    let start = Instant::now();
    renderer.drive(gpu, &program.scheduler, port).unwrap();
    let wall_ms = start.elapsed().as_secs_f64() * 1000.;
    let deadline = Instant::now() + Duration::from_secs(10);
    let slot = loop {
        let slot = tap
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        if slot
            .association
            .as_ref()
            .unwrap()
            .provenance
            .input
            .frame
            .sequence
            .get()
            == *sequence
        {
            break slot;
        }
    };
    assert_eq!(
        slot.association
            .as_ref()
            .unwrap()
            .provenance
            .receipt
            .submission_id,
        submission
    );
    assert_eq!(slot.uyvy.len(), 640 * 360 * 2);
    *sequence += 1;
    (Arc::clone(&slot.uyvy), wall_ms)
}

async fn hardware(prepared: Prepared) {
    let gpu = Gpu::request(&Gpu::instance(), None).await.unwrap();
    let info = gpu.adapter.get_info();
    assert!(matches!(
        info.device_type,
        wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
    ));
    println!("pitch adapter: {} {:?}", info.name, info.backend);
    let Prepared {
        mut program,
        adapter,
        controls,
    } = prepared;
    let (control, owner) = SceneControl::spawn(adapter, 64).unwrap();
    program.create_scene(&control).await.unwrap();
    let mut renderer = ProgramRenderer::new(&gpu, &program.configuration).unwrap();
    let (sdi, tap) = SdiOutput::spawn_program(mode(), id("pitch-test-port").unwrap()).unwrap();
    let mut port = sdi.input();
    let mut pictures = Vec::new();
    let mut sequence = 0_u64;
    let pitch = serde_json::from_value(controls["pitch"].clone()).unwrap();
    for rig in ["none", "points", "spots"] {
        for sun in [false, true] {
            let lighting = pitch::lighting(pitch, 35., rig, sun, [[0.; 2]; 4]).unwrap();
            for shadow in ["hard", "pcf", "pcss"] {
                mutate(
                    &control,
                    &program,
                    json!([
                        {"kind":"lighting_set","lighting":lighting},
                        {"kind":"shadow_mode_set","shadow_mode":shadow},
                    ]),
                )
                .await;
                let (pixels, wall_ms) = frame(
                    &control,
                    &mut program,
                    &mut renderer,
                    &gpu,
                    &mut port,
                    &tap,
                    &mut sequence,
                )
                .await;
                let mut warm = Vec::new();
                for _ in 0..5 {
                    let (stable, elapsed) = frame(
                        &control,
                        &mut program,
                        &mut renderer,
                        &gpu,
                        &mut port,
                        &tap,
                        &mut sequence,
                    )
                    .await;
                    assert_eq!(*stable, *pixels, "stable scene changes actual pixels");
                    warm.push(elapsed);
                }
                warm.sort_by(f64::total_cmp);
                println!(
                    "pitch 640x360 native={}x{} shadow512 rig={rig} sun={sun} mode={shadow} worker_wall_queue_readback_ms cold={wall_ms:.3} warm_median={:.3} warm_max={:.3}",
                    program.configuration.native_extent.width,
                    program.configuration.native_extent.height,
                    warm[2],
                    warm[4],
                );
                pictures.push(pixels);
            }
        }
    }
    let difference = |a: usize, b: usize| {
        pictures[a]
            .iter()
            .zip(pictures[b].iter())
            .filter(|(a, b)| a.abs_diff(**b) > 2)
            .count()
    };
    assert!(difference(0, 3) > 100, "independent sun");
    assert!(difference(0, 6) > 100, "independent point rig");
    assert!(difference(0, 12) > 100, "independent spot rig");
    assert!(difference(6, 9) > 100, "sun adds to points");
    assert!(difference(12, 15) > 100, "sun adds to spots");
    assert!(difference(6, 12) > 20, "point and spot differ");
    assert!(difference(3, 4) > 5, "hard/PCF differs");
    assert!(difference(4, 5) > 5, "PCF/PCSS differs");
    // Check renderer-authoritative preview pose, then exact imported reset on the live scene.
    let original = controls["original_camera"].clone();
    let pose = &controls["preview_camera"];
    let extent = &pose["extent"];
    let intrinsics = &pose["intrinsics"];
    let camera = Camera::from_pose(
        ImageExtent::new(
            extent["width"].as_u64().unwrap() as u32,
            extent["height"].as_u64().unwrap() as u32,
        )
        .unwrap(),
        Intrinsics {
            fx: intrinsics["fx"].as_f64().unwrap(),
            fy: intrinsics["fy"].as_f64().unwrap(),
            skew: intrinsics["skew"].as_f64().unwrap(),
            cx: intrinsics["cx"].as_f64().unwrap(),
            cy: intrinsics["cy"].as_f64().unwrap(),
        },
        serde_json::from_value(pose["camera_to_world"].clone()).unwrap(),
        0.1,
        10000.,
    )
    .unwrap();
    assert!(
        camera
            .ray([
                extent["width"].as_f64().unwrap() / 2.,
                extent["height"].as_f64().unwrap() / 2.
            ])
            .is_ok()
    );
    let mut zoom = pose.clone();
    zoom["intrinsics"]["fx"] = json!(intrinsics["fx"].as_f64().unwrap() * 1.25);
    zoom["intrinsics"]["fy"] = json!(intrinsics["fy"].as_f64().unwrap() * 1.25);
    mutate(
        &control,
        &program,
        json!([{"kind":"camera_set","camera":zoom}]),
    )
    .await;
    let (zoomed, edit_ms) = frame(
        &control,
        &mut program,
        &mut renderer,
        &gpu,
        &mut port,
        &tap,
        &mut sequence,
    )
    .await;
    assert!(
        zoomed
            .iter()
            .zip(pictures[17].iter())
            .filter(|(a, b)| a.abs_diff(**b) > 2)
            .count()
            > 100,
        "zoom changes real pixels"
    );
    println!("pitch camera edit worker_wall_queue_readback_ms={edit_ms:.3}");
    mutate(
        &control,
        &program,
        json!([{"kind":"camera_set","camera":original}]),
    )
    .await;
    assert_eq!(
        query(&control, &program).await["state"]["camera"]["camera"],
        original
    );
    let (reset, _) = frame(
        &control,
        &mut program,
        &mut renderer,
        &gpu,
        &mut port,
        &tap,
        &mut sequence,
    )
    .await;
    assert_eq!(
        *reset, *pictures[17],
        "exact imported reset reproduces original program pixels"
    );
    for choice in controls["models"].as_array().unwrap().iter().skip(1) {
        mutate(
            &control,
            &program,
            json!([{"kind":"instance_put","instance":choice["instance"]}]),
        )
        .await;
        let (selected, _) = frame(
            &control,
            &mut program,
            &mut renderer,
            &gpu,
            &mut port,
            &tap,
            &mut sequence,
        )
        .await;
        assert!(
            selected
                .iter()
                .zip(reset.iter())
                .filter(|(a, b)| a.abs_diff(**b) > 2)
                .count()
                > 10,
            "model selection changes actual pixels"
        );
    }
    drop(sdi);
    drop(tap);
    drop(control);
    owner.await.unwrap();
}

#[tokio::test]
#[ignore = "requires a native hardware GPU"]
async fn gpu_pitch_program_rigs_shadows_and_reset() {
    hardware(install()).await;
}

#[tokio::test]
#[ignore = "requires owner-local CHR_PITCH_FIXTURE and hardware; never uploads fixtures"]
async fn gpu_private_pitch_program() {
    let path = std::env::var("CHR_PITCH_FIXTURE").expect("explicit local fixture required");
    let prepared = Prepared::load(Path::new(&path), mode(), runtime_id().unwrap()).unwrap();
    assert_eq!(
        prepared.controls["center_semantics"],
        "conditional_fixed_ti"
    );
    hardware(prepared).await;
    println!("owner-local fixture automated acceptance: PASS");
}
