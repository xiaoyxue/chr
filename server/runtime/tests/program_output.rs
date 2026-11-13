//! Synthetic program acceptance, including the real GPU -> simulated SDI -> program tap.
use chr_common::{methods::mesh, scene_frame as w};
use chr_renderer::Gpu;
use chr_runtime::{
    program::{
        ProgramRenderer,
        synthetic::{Synthetic, id, runtime_id},
    },
    retained::SceneControl,
    sdi::{SdiOutput, SdiSlot, Taps, VideoMode},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

fn mode() -> VideoMode {
    VideoMode {
        width: 192,
        height: 128,
        fps: 50,
    }
}
async fn call(control: &SceneControl, method: &str, params: Value) -> Value {
    control.call(method, Some(params)).await.unwrap()
}
fn target(synthetic: &Synthetic) -> Value {
    json!({"runtime_id":synthetic.configuration.runtime_id,"scene_id":synthetic.scene_id})
}
async fn snapshot(control: &SceneControl, synthetic: &Synthetic, submission: &w::Id) -> Value {
    call(
        control,
        w::RENDER_GET,
        json!({"runtime_id":synthetic.configuration.runtime_id,"submission_id":submission}),
    )
    .await
}
async fn mutate(control: &SceneControl, synthetic: &Synthetic, operations: Value) {
    let scene = call(control, w::SCENE_GET, target(synthetic)).await;
    let mut request = target(synthetic);
    request["expected_revision"] = scene["scene_revision"].clone();
    request["operations"] = operations;
    call(control, w::SCENE_MUTATE, request).await;
}
fn receive(tap: &mpsc::Receiver<SdiSlot>, sequence: u64) -> SdiSlot {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let slot = tap
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("actual SDI tick");
        if slot
            .association
            .as_ref()
            .unwrap()
            .provenance
            .input
            .frame
            .sequence
            .get()
            == sequence
        {
            return slot;
        }
    }
}

#[tokio::test]
async fn server_rotation_advances_without_browser_edits_and_preserves_bound_scenes() {
    let (mut program, adapter) = Synthetic::install_video(mode(), runtime_id().unwrap()).unwrap();
    let (control, owner) = SceneControl::spawn(adapter.with_rotation(), 16).unwrap();
    program.create_scene(&control).await.unwrap();
    let mut request = target(&program);
    request["instance_id"] = json!("mesh");
    let initial = call(&control, w::SCENE_GET, target(&program)).await;
    let mut start = request.clone();
    start["angular_velocity_rad_per_s"] = json!(std::f64::consts::PI);
    control
        .rotate(mesh::START_ROTATION, Some(start.clone()), 7)
        .await
        .unwrap();
    let mut bound = Vec::new();
    for frame in [8, 9, 32] {
        control.advance_rotation(frame).await.unwrap();
        program.inject(&control).await.unwrap().unwrap();
        let execution = program.scheduler.next_ready().unwrap().unwrap();
        bound.push(execution.scene.clone());
        execution.failed().unwrap();
    }
    let pose = |index: usize| bound[index].snapshot().state.instances[0].model;
    assert_ne!(
        pose(0),
        pose(1),
        "each native frame gets a new pose without any browser command"
    );
    assert!((pose(2)[0][0].get()).abs() < 1e-12);
    assert!(
        (pose(2)[1][0].get() - 0.25).abs() < 1e-12,
        "phase follows slot gaps, not mutation count"
    );
    assert_ne!(
        pose(0),
        pose(2),
        "earlier admitted snapshots stay immutable"
    );
    for scene in &bound {
        assert_eq!(
            serde_json::to_value(&scene.snapshot().state.instances[1]).unwrap(),
            initial["state"]["instances"][1]
        );
    }
    start["angular_velocity_rad_per_s"] = json!(-std::f64::consts::PI);
    let reversed = control
        .rotate(mesh::START_ROTATION, Some(start), 32)
        .await
        .unwrap();
    assert!((reversed["angle_rad"].as_f64().unwrap() - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
    control.advance_rotation(57).await.unwrap();
    let stopped = control
        .rotate(mesh::STOP_ROTATION, Some(request.clone()), 57)
        .await
        .unwrap();
    let frozen = call(&control, w::SCENE_GET, target(&program)).await;
    control.advance_rotation(5000).await.unwrap();
    assert_eq!(call(&control, w::SCENE_GET, target(&program)).await, frozen);
    assert!(!stopped["rotating"].as_bool().unwrap());
    assert!(
        (frozen["state"]["instances"][0]["model"][0][0]
            .as_f64()
            .unwrap()
            - 0.25)
            .abs()
            < 1e-12
    );
    assert!(
        control
            .rotate(
                mesh::START_ROTATION,
                Some(json!({
                    "runtime_id":program.configuration.runtime_id,"scene_id":program.scene_id,
                    "instance_id":"mesh","angular_velocity_rad_per_s":1,"extra":true
                })),
                5000
            )
            .await
            .is_err()
    );
    drop(control);
    owner.await.unwrap();
}

#[tokio::test]
async fn external_model_edits_stop_animation_and_other_profiles_do_not_enable_it() {
    let (program, adapter) = Synthetic::install_video(mode(), runtime_id().unwrap()).unwrap();
    let (control, owner) = SceneControl::spawn(adapter, 16).unwrap();
    program.create_scene(&control).await.unwrap();
    let mut request = target(&program);
    request["instance_id"] = json!("mesh");
    assert_eq!(
        control
            .rotate(mesh::GET_ROTATION, Some(request), 0)
            .await
            .unwrap_err()
            .code,
        -32601
    );
    drop(control);
    owner.await.unwrap();

    let (program, adapter) = Synthetic::install_video(mode(), runtime_id().unwrap()).unwrap();
    let (control, owner) = SceneControl::spawn(adapter.with_rotation(), 16).unwrap();
    program.create_scene(&control).await.unwrap();
    let mut request = target(&program);
    request["instance_id"] = json!("mesh");
    let mut start = request.clone();
    start["angular_velocity_rad_per_s"] = json!(1.);
    control
        .rotate(mesh::START_ROTATION, Some(start), 0)
        .await
        .unwrap();
    control.advance_rotation(1).await.unwrap();
    let scene = call(&control, w::SCENE_GET, target(&program)).await;
    let mut instance = scene["state"]["instances"][0].clone();
    instance["model"][0][3] = json!(4.0);
    mutate(
        &control,
        &program,
        json!([{"kind":"instance_put","instance":instance}]),
    )
    .await;
    control.advance_rotation(2).await.unwrap();
    let state = control
        .rotate(mesh::GET_ROTATION, Some(request), 2)
        .await
        .unwrap();
    assert_eq!(state["rotating"], false);
    assert_eq!(
        call(&control, w::SCENE_GET, target(&program)).await["state"]["instances"][0],
        instance
    );
    drop(control);
    owner.await.unwrap();
}

#[tokio::test]
#[ignore = "requires a native hardware GPU"]
async fn gpu_server_rotation_changes_program_pixels_and_stop_freezes_them() {
    let gpu = Gpu::request(&Gpu::instance(), None).await.unwrap();
    assert!(matches!(
        gpu.adapter.get_info().device_type,
        wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
    ));
    let (mut program, adapter) = Synthetic::install_video(mode(), runtime_id().unwrap()).unwrap();
    let (control, owner) = SceneControl::spawn(adapter.with_rotation(), 16).unwrap();
    program.create_scene(&control).await.unwrap();
    let mut request = target(&program);
    request["instance_id"] = json!("mesh");
    let mut start = request.clone();
    start["angular_velocity_rad_per_s"] = json!(std::f64::consts::PI);
    control
        .rotate(mesh::START_ROTATION, Some(start), 0)
        .await
        .unwrap();
    let mut renderer = ProgramRenderer::new(&gpu, &program.configuration).unwrap();
    let (sdi, tap) = {
        let (taps, tap) = Taps::observed();
        (
            SdiOutput::spawn_program(mode(), id("rotation-test-output").unwrap(), taps).unwrap(),
            tap,
        )
    };
    let mut port = sdi.input();
    let mut pixels = Vec::new();
    for (sequence, frame) in [0, 25, 50, 75].into_iter().enumerate() {
        if frame == 50 {
            control
                .rotate(mesh::STOP_ROTATION, Some(request.clone()), frame)
                .await
                .unwrap();
        }
        control.advance_rotation(frame).await.unwrap();
        program.inject(&control).await.unwrap().unwrap();
        renderer.drive(&gpu, &program.scheduler, &mut port).unwrap();
        pixels.push(receive(&tap, sequence as u64).uyvy);
    }
    assert_ne!(pixels[0], pixels[1]);
    assert_ne!(pixels[1], pixels[2]);
    assert_eq!(
        pixels[2], pixels[3],
        "stopped pose produces identical program pixels"
    );
    control.shutdown().await;
    owner.await.unwrap();
}

fn video_manifest() -> chr_runtime::program::video::Manifest {
    let s = (192_f64 * 128.).sqrt();
    serde_json::from_value(json!({
        "width":192,"height":128,"fps":50,
        "ti":[[0.12,0,0],[0,0.12,0],[0,0,1]],"center":[0,0,-8],
        "radial":{"centre_px":[96,64],"scale_px":192,"coefficients":[-0.08,0,0],"support":null},
        "frame_warps":[
            [[2./s,0.,-192./s],[0.,2./s,-128./s],[0,0,1]],
            [[2./s,0.,-192./s+0.12],[0.,2./s,-128./s],[0,0,1]]
        ],
        "model":"unused.glb","height_m":2,"anchor_pixel":[96,82],"pitch_m":[105,68]
    }))
    .unwrap()
}

#[test]
fn calibrated_video_rejects_mismatched_or_invalid_geometry() {
    for edit in 0..4 {
        let mut manifest = video_manifest();
        match edit {
            0 => manifest.fps = 30,
            1 => manifest.frame_warps.clear(),
            2 => manifest.frame_warps[0] = [[0.; 3]; 3],
            _ => manifest.height_m = 0.,
        }
        assert!(
            chr_runtime::program::video::prepare(
                manifest,
                chr_runtime::program::pitch::block().unwrap(),
                mode(),
                runtime_id().unwrap(),
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn calibrated_warp_follows_native_identity_across_gaps_and_loops() {
    let (mut program, adapter) = chr_runtime::program::video::prepare(
        video_manifest(),
        chr_runtime::program::pitch::block().unwrap(),
        mode(),
        runtime_id().unwrap(),
    )
    .unwrap();
    let (control, owner) = SceneControl::spawn(adapter, 16).unwrap();
    program.create_scene(&control).await.unwrap();
    let lens = program.video_lens.as_ref().unwrap().clone();
    let mut models = Vec::new();
    for sequence in [0, 3, 8] {
        program
            .inject_image(&control, sequence, &[20, 100, 40, 255].repeat(192 * 128))
            .await
            .unwrap()
            .unwrap();
        let execution = program.scheduler.next_ready().unwrap().unwrap();
        models.push(
            lens.model(&execution.provenance.input.frame)
                .unwrap()
                .clone(),
        );
        execution.failed().unwrap();
    }
    assert_eq!(models[0], models[2], "loop must use the original warp");
    assert_ne!(
        models[0], models[1],
        "dropped frames must not shift the warp"
    );
    assert!(
        lens.model(&w::FrameKey {
            stream_id: id("unrelated").unwrap(),
            sequence: 0.into()
        })
        .is_err()
    );
    let scene = call(&control, w::SCENE_GET, target(&program)).await;
    assert_eq!(scene["state"]["shadow_mode"], "pcss");
    assert_eq!(
        scene["state"]["lighting"]["lights"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    drop(control);
    owner.await.unwrap();
}

#[tokio::test]
#[ignore = "requires a native hardware GPU"]
async fn gpu_calibrated_video_warps_cg_not_plate_and_repeats_exact_loop() {
    let gpu = Gpu::request(&Gpu::instance(), None).await.unwrap();
    assert!(matches!(
        gpu.adapter.get_info().device_type,
        wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
    ));
    let (mut program, adapter) = chr_runtime::program::video::prepare(
        video_manifest(),
        chr_runtime::program::pitch::block().unwrap(),
        VideoMode {
            width: 96,
            height: 64,
            fps: 50,
        },
        runtime_id().unwrap(),
    )
    .unwrap();
    let (control, owner) = SceneControl::spawn(adapter, 16).unwrap();
    program.create_scene(&control).await.unwrap();
    let mut renderer = ProgramRenderer::new(&gpu, &program.configuration).unwrap();
    renderer.set_video_lens(program.video_lens.clone());
    let output = VideoMode {
        width: 96,
        height: 64,
        fps: 50,
    };
    let (sdi, tap) = {
        let (taps, tap) = Taps::observed();
        (
            SdiOutput::spawn_program(output, id("calibrated-test-output").unwrap(), taps).unwrap(),
            tap,
        )
    };
    let mut port = sdi.input();
    let mut pixels = Vec::new();
    for sequence in [0, 1, 4] {
        program
            .inject_image(&control, sequence, &[20, 100, 40, 255].repeat(192 * 128))
            .await
            .unwrap()
            .unwrap();
        renderer.drive(&gpu, &program.scheduler, &mut port).unwrap();
        pixels.push(receive(&tap, sequence).uyvy);
    }
    assert_ne!(
        pixels[0], pixels[1],
        "per-frame warp must move actual CG/shadow pixels"
    );
    assert_eq!(
        pixels[0], pixels[2],
        "loop must restore identical GPU pixels"
    );
    assert_eq!(
        &pixels[0][..96 * 2],
        &pixels[1][..96 * 2],
        "plate must not be warped"
    );
    let scene = call(&control, w::SCENE_GET, target(&program)).await;
    let mut mesh = scene["state"]["instances"][0].clone();
    mesh["visible"] = json!(false);
    mutate(
        &control,
        &program,
        json!([{"kind":"instance_put","instance":mesh}]),
    )
    .await;
    program
        .inject_image(&control, 6, &[20, 100, 40, 255].repeat(192 * 128))
        .await
        .unwrap()
        .unwrap();
    renderer.drive(&gpu, &program.scheduler, &mut port).unwrap();
    let bare_plate = receive(&tap, 6).uyvy;
    assert!(
        bare_plate
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p == &bare_plate[..4]),
        "shadow catcher without a caster must leave the solid plate unchanged"
    );
    drop(control);
    owner.await.unwrap();
}

#[tokio::test]
async fn software_images_keep_capture_sequence_gaps_and_pixel_ownership() {
    let (mut program, adapter) = Synthetic::install_video(mode(), runtime_id().unwrap()).unwrap();
    let (control, owner) = SceneControl::spawn(adapter, 8).unwrap();
    program.create_scene(&control).await.unwrap();
    let scene = call(&control, w::SCENE_GET, target(&program)).await;
    assert_eq!(scene["state"]["instances"].as_array().unwrap().len(), 2);
    assert_eq!(scene["state"]["instances"][0]["instance_id"], "mesh");
    assert_eq!(scene["state"]["instances"][1]["instance_id"], "ground");
    assert_ne!(
        scene["state"]["instances"][0]["asset_ref"],
        scene["state"]["instances"][1]["asset_ref"]
    );
    let mut image = [30, 60, 90, 255].repeat(192 * 128);
    program
        .inject_image(&control, 7, &image)
        .await
        .unwrap()
        .unwrap();
    image.fill(0);
    let execution = program.scheduler.next_ready().unwrap().unwrap();
    assert_eq!(execution.provenance.input.frame.sequence.get(), 7);
    assert_eq!(execution.provenance.input.position.ticks.get(), 7);
    assert_eq!(&execution.input.pixels().unwrap()[..4], &[30, 60, 90, 255]);
    execution.failed().unwrap();
    assert!(program.inject_image(&control, 6, &image).await.is_err());
    program
        .inject_image(&control, 12, &image)
        .await
        .unwrap()
        .unwrap();
    let execution = program.scheduler.next_ready().unwrap().unwrap();
    assert_eq!(execution.provenance.input.frame.sequence.get(), 12);
    execution.failed().unwrap();
    drop(control);
    owner.await.unwrap();
}

#[tokio::test]
async fn explicit_profile_keeps_input_and_submission_bounded() {
    let (mut synthetic, adapter) = Synthetic::install(mode(), runtime_id().unwrap()).unwrap();
    let (control, owner) = SceneControl::spawn(adapter, 8).unwrap();
    synthetic.create_scene(&control).await.unwrap();
    let first = synthetic.inject(&control).await.unwrap().unwrap();
    let execution = synthetic.scheduler.next_ready().unwrap().unwrap();
    let original = execution.provenance.receipt.clone();
    mutate(
        &control,
        &synthetic,
        json!([{"kind":"shadow_mode_set","shadow_mode":"off"}]),
    )
    .await;
    assert_eq!(execution.scene.snapshot().scene_revision.get(), 0);
    assert_eq!(execution.input.pixels().unwrap()[..4], [32, 64, 96, 255]);
    call(
        &control,
        w::RENDER_CANCEL,
        json!({"runtime_id":synthetic.configuration.runtime_id,"submission_id":first}),
    )
    .await;
    execution
        .rendered(Arc::new(vec![0u8; 192 * 128 * 2]))
        .unwrap();
    synthetic.scheduler.tick().unwrap();
    let result = snapshot(&control, &synthetic, &first).await;
    assert_eq!(result["frames"][0]["reason"], "cancelled");
    assert_eq!(result["effect"], json!(original.effect));
    // More cycles than physical input capacity: explicit publisher revocation and work completion reclaim cells.
    for _ in 0..24 {
        synthetic.inject(&control).await.unwrap().unwrap();
        synthetic
            .scheduler
            .next_ready()
            .unwrap()
            .unwrap()
            .failed()
            .unwrap();
        synthetic.scheduler.tick().unwrap();
    }
    let mut invalid = synthetic.configuration.clone();
    invalid.output_transfer = w::OutputTransfer::Srgb;
    assert!(chr_runtime::program::validate_output(&invalid).is_err());
    invalid.output_transfer = w::OutputTransfer::Bt709;
    invalid.output_rate = w::TimeBase::new(1001, 60000).unwrap();
    assert!(chr_runtime::program::validate_output(&invalid).is_err());
    drop(control);
    owner.await.unwrap();
}

#[tokio::test]
#[ignore = "requires a native hardware GPU"]
async fn gpu_video_mesh_turns_independently_of_ground_and_restores_pixels() {
    let gpu = Gpu::request(&Gpu::instance(), None).await.unwrap();
    assert!(matches!(
        gpu.adapter.get_info().device_type,
        wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
    ));
    let (mut program, adapter) = Synthetic::install_video(mode(), runtime_id().unwrap()).unwrap();
    let (control, owner) = SceneControl::spawn(adapter, 16).unwrap();
    program.create_scene(&control).await.unwrap();
    let scene = call(&control, w::SCENE_GET, target(&program)).await;
    let original = scene["state"]["instances"][0].clone();
    let ground = scene["state"]["instances"][1].clone();
    let mut positive = original.clone();
    positive["model"] = json!([
        [0, -0.25, 0, 0],
        [0.25, 0, 0, 0],
        [0, 0, 0.25, 0],
        [0, 0, 0, 1]
    ]);
    let mut negative = original.clone();
    negative["model"] = json!([
        [0, 0.25, 0, 0],
        [-0.25, 0, 0, 0],
        [0, 0, 0.25, 0],
        [0, 0, 0, 1]
    ]);
    let mut renderer = ProgramRenderer::new(&gpu, &program.configuration).unwrap();
    let (sdi, tap) = {
        let (taps, tap) = Taps::observed();
        (
            SdiOutput::spawn_program(mode(), id("mesh-rotation-output").unwrap(), taps).unwrap(),
            tap,
        )
    };
    let mut port = sdi.input();
    let mut images = Vec::new();
    for (sequence, instance) in [original.clone(), positive, negative, original]
        .into_iter()
        .enumerate()
    {
        mutate(
            &control,
            &program,
            json!([{"kind":"instance_put","instance":instance}]),
        )
        .await;
        let current = call(&control, w::SCENE_GET, target(&program)).await;
        assert_eq!(current["state"]["instances"][1], ground);
        program.inject(&control).await.unwrap().unwrap();
        renderer.drive(&gpu, &program.scheduler, &mut port).unwrap();
        images.push(receive(&tap, sequence as u64).uyvy);
    }
    assert_ne!(
        images[0], images[1],
        "positive turn must affect actual program pixels"
    );
    assert_ne!(
        images[1], images[2],
        "opposite turns must produce different poses"
    );
    assert_eq!(
        images[0], images[3],
        "restoring the mesh must restore static program pixels"
    );
    drop(control);
    owner.await.unwrap();
}

#[tokio::test]
#[ignore = "requires a native hardware GPU"]
async fn gpu_program_pixels_controls_provenance_and_tap() {
    let gpu = Gpu::request(&Gpu::instance(), None).await.unwrap();
    let adapter = gpu.adapter.get_info();
    println!("program GPU adapter: {adapter:?}");
    assert!(
        matches!(
            adapter.device_type,
            wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
        ),
        "hardware required"
    );
    let (mut synthetic, adapter) = Synthetic::install(mode(), runtime_id().unwrap()).unwrap();
    let (control, owner) = SceneControl::spawn(adapter, 64).unwrap();
    synthetic.create_scene(&control).await.unwrap();
    let mut renderer = ProgramRenderer::new(&gpu, &synthetic.configuration).unwrap();
    let tap_dir = std::env::temp_dir().join(format!("chr-program-tap-{}", std::process::id()));
    let tap_path = tap_dir.join("program.tap");
    let (mut taps, tap) = Taps::observed();
    taps.ring = Taps::ring(mode(), &tap_path).unwrap().ring;
    let sdi = SdiOutput::spawn_program(mode(), id("test-output-stream").unwrap(), taps).unwrap();
    let program_tap = chr_shm::Reader::open(&tap_path).unwrap();
    let mut port = sdi.input();

    let first_id = synthetic.inject(&control).await.unwrap().unwrap();
    renderer
        .drive(&gpu, &synthetic.scheduler, &mut port)
        .unwrap();
    let first = receive(&tap, 0);
    let association = first.association.as_ref().unwrap();
    assert_eq!(association.provenance.receipt.scene_revision.get(), 0);
    assert_eq!(
        association.provenance.input.frame.stream_id,
        synthetic.stream_id
    );
    assert_eq!(association.provenance.input.position.ticks.get(), 0);
    let baseline = Arc::clone(&first.uyvy);
    // Display background is untouched by scene exposure/tone; the RGB transfer is BT.709, not sRGB.
    assert!(
        (i16::from(baseline[1]) - 67).abs() <= 2,
        "background luma {}",
        baseline[1]
    );
    let first_slot = first.slot;
    drop(first);
    // Until the next injection every slot repeats source frame 0, so the newest
    // tap slot is that frame however far the port has ticked.
    let mut tapped = Vec::new();
    let read = program_tap
        .read(program_tap.latest_slot().unwrap(), &mut tapped)
        .unwrap();
    let chr_shm::Read::Frame(header) = read else {
        panic!("program tap slot not readable: {read:?}");
    };
    assert!(header.index >= first_slot);
    assert_eq!(
        chr_shm::tap::SlotMeta::from_meta(header.meta),
        chr_shm::tap::SlotMeta {
            source_frame: 0,
            program: true,
            width: mode().width,
            height: mode().height,
            fps: mode().fps,
        }
    );
    assert_eq!(tapped, *baseline, "the tap carries the transmitted pixels");
    let repeated = receive(&tap, 0);
    assert!(repeated.slot > first_slot);
    assert!(Arc::ptr_eq(&baseline, &repeated.uyvy));
    let result = snapshot(&control, &synthetic, &first_id).await;
    assert_eq!(
        result["frames"][0]["delivery"]["first_slot"],
        first_slot.to_string()
    );
    assert_eq!(
        result["frames"][0]["output_frame_id"],
        json!(repeated.association.as_ref().unwrap().output_frame_id)
    );
    drop(repeated);

    // Revisions bind at admission, not at GPU dispatch.
    let second_id = synthetic.inject(&control).await.unwrap().unwrap();
    assert!(
        call(
            &control,
            w::RENDER_GET,
            json!({"runtime_id":synthetic.configuration.runtime_id,"submission_id":second_id})
        )
        .await["frames"][0]["state"]
            == "pending"
    );
    mutate(
        &control,
        &synthetic,
        json!([{"kind":"lighting_set","lighting":{"lights":[],"environment":null}}]),
    )
    .await;
    renderer
        .drive(&gpu, &synthetic.scheduler, &mut port)
        .unwrap();
    let second = receive(&tap, 1);
    assert_eq!(
        *second.uyvy, *baseline,
        "admitted revision survived a concurrent mutation"
    );
    synthetic.inject(&control).await.unwrap().unwrap();
    renderer
        .drive(&gpu, &synthetic.scheduler, &mut port)
        .unwrap();
    let dark = receive(&tap, 2);
    let changed = dark
        .uyvy
        .iter()
        .zip(baseline.iter())
        .filter(|(a, b)| a.abs_diff(**b) > 8)
        .count();
    assert!(
        changed > 100,
        "lighting must alter actual program pixels: {changed}"
    );
    println!("lighting changed {changed} UYVY bytes; first transmission slot {first_slot}");
    drop(second);
    drop(dark);

    mutate(
        &control,
        &synthetic,
        json!([{"kind":"lighting_set","lighting":{"lights":[{
        "light_id":"sun","kind":"directional","direction":[0.4,0.2,1],"irradiance":[3,3,3],
        "angular_radius":0.00465,"shadow_mode":null}],"environment":null}},
        {"kind":"shadow_mode_set","shadow_mode":"off"}]),
    )
    .await;
    synthetic.inject(&control).await.unwrap().unwrap();
    renderer
        .drive(&gpu, &synthetic.scheduler, &mut port)
        .unwrap();
    let unshadowed = receive(&tap, 3);
    let changed = unshadowed
        .uyvy
        .iter()
        .zip(baseline.iter())
        .filter(|(a, b)| a.abs_diff(**b) > 3)
        .count();
    assert!(
        changed > 5,
        "shadow controls must alter real pixels: {changed}"
    );
    println!("shadow mode changed {changed} UYVY bytes");

    let state = call(&control, w::SCENE_GET, target(&synthetic)).await;
    let mut camera = state["state"]["camera"]["camera"].clone();
    camera["intrinsics"]["fx"] = json!(300);
    camera["intrinsics"]["fy"] = json!(300);
    mutate(
        &control,
        &synthetic,
        json!([{"kind":"camera_set","camera":camera}]),
    )
    .await;
    let final_id = synthetic.inject(&control).await.unwrap().unwrap();
    renderer
        .drive(&gpu, &synthetic.scheduler, &mut port)
        .unwrap();
    let zoomed = receive(&tap, 4);
    let changed = zoomed
        .uyvy
        .iter()
        .zip(unshadowed.uyvy.iter())
        .filter(|(a, b)| a.abs_diff(**b) > 8)
        .count();
    assert!(
        changed > 100,
        "camera edit must alter real pixels: {changed}"
    );
    println!("camera zoom changed {changed} UYVY bytes");
    let weak = Arc::downgrade(&zoomed.uyvy);
    call(
        &control,
        w::RENDER_RELEASE,
        json!({"runtime_id":synthetic.configuration.runtime_id,"submission_id":final_id}),
    )
    .await;
    drop(zoomed);
    assert!(
        weak.upgrade().is_some(),
        "public release cannot free repeated-current output"
    );
    // Stop consuming: the observer skips slots and nobody reads the tap, but SDI
    // keeps repeating and the tap writer keeps advancing.
    let (before, written) = (
        sdi.stats().status().frames_output,
        program_tap.write_count(),
    );
    std::thread::sleep(Duration::from_millis(120));
    assert!(sdi.stats().status().frames_output > before);
    assert!(program_tap.write_count() > written);
    drop(sdi);
    drop(tap);
    std::fs::remove_dir_all(&tap_dir).unwrap();
    assert!(
        weak.upgrade().is_none(),
        "shutdown frees current and queued tap allocations"
    );
    drop(control);
    owner.await.unwrap();
}
