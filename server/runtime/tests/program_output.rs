//! Synthetic program acceptance, including the real GPU -> simulated SDI -> software monitor.
use chr_common::scene_frame as w;
use chr_renderer::Gpu;
use chr_server::{
    monitor::{Monitor, encoder::VideoFeed},
    program::{
        ProgramRenderer,
        synthetic::{Synthetic, id, runtime_id},
    },
    retained::SceneControl,
    sdi::{SdiOutput, SdiSlot, VideoMode},
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
    assert!(chr_server::program::validate_output(&invalid).is_err());
    invalid.output_transfer = w::OutputTransfer::Bt709;
    invalid.output_rate = w::TimeBase::new(1001, 60000).unwrap();
    assert!(chr_server::program::validate_output(&invalid).is_err());
    drop(control);
    owner.await.unwrap();
}

#[tokio::test]
#[ignore = "requires a native hardware GPU"]
async fn gpu_program_pixels_controls_provenance_and_monitor() {
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
    let (sdi, tap) = SdiOutput::spawn_program(mode(), id("test-output-stream").unwrap()).unwrap();
    let mut port = sdi.input();
    let feed = Arc::new(VideoFeed::new());
    let mut viewer = feed.subscribe();
    let (monitor_tx, monitor_rx) = mpsc::sync_channel(2);
    let monitor = Monitor::spawn(mode(), monitor_rx, feed).unwrap();

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
    monitor_tx.send(first).unwrap();
    let encoded = tokio::time::timeout(Duration::from_secs(5), viewer.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(encoded.frame, first_slot);
    assert_eq!(
        encoded
            .program
            .as_ref()
            .unwrap()
            .provenance
            .receipt
            .submission_id,
        first_id
    );
    assert!(!encoded.data.is_empty());
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
    // Stop consuming: the bounded monitor tap drops, but SDI keeps repeating.
    let before = sdi.stats().status().frames_output;
    std::thread::sleep(Duration::from_millis(120));
    assert!(sdi.stats().status().frames_output > before);
    assert!(sdi.stats().status().monitor_dropped_frames > 0);
    drop(sdi);
    drop(tap);
    drop(monitor_tx);
    drop(monitor);
    assert!(
        weak.upgrade().is_none(),
        "shutdown frees current and queued tap allocations"
    );
    drop(control);
    owner.await.unwrap();
}
