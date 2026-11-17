//! Resource-backed synthetic mechanics, not hardware/GPU/SDI acceptance.
use super::*;
use chr_runtime::{
    frame_resources::{FrameMetadata, FrameResources, ProducerGuard, ResourceConfig},
    frame_submissions::{FrameJoin, FrameSubmissions, MonotonicClock, OutputFrame, OutputPort},
};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
struct Clock(AtomicU64);
impl MonotonicClock for Clock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}
impl Clock {
    fn set(&self, time: u64) {
        self.0.store(time, Ordering::Relaxed);
    }
}

fn configuration(frames: u32, inflight: u32, retained: u32) -> w::Configuration {
    let mut config = config();
    config.native_extent = w::Extent {
        width: 4,
        height: 4,
    };
    config.output_extent = config.native_extent;
    config.limits.published_frames = frames;
    config.limits.frames_per_submission = inflight;
    config.limits.inflight_frames = inflight;
    config.limits.retained_submissions = retained;
    config.limits.render_timeout_ms = 100;
    config
}
fn real_port(config: w::Configuration) -> (FrameSubmissions, Arc<Clock>) {
    let resources = resources(&config);
    let producer = resources.producer(&id("stream-01")).unwrap();
    let clock = Arc::new(Clock::default());
    let port = FrameSubmissions::new(config, resources, producer, clock.clone()).unwrap();
    (port, clock)
}
fn resources(config: &w::Configuration) -> FrameResources {
    FrameResources::new(
        ResourceConfig {
            runtime_id: config.runtime_id.clone(),
            native_extent: config.native_extent,
            max_frames: config.limits.published_frames as usize,
            max_frame_bytes: config.limits.published_frames as usize * 64,
            max_assets: 1,
            max_asset_bytes: 64,
            max_producers: 2,
        },
        vec![id("stream-01"), id("other-stream")],
    )
    .unwrap()
}
fn create() -> Value {
    let mut create = fixture("create", "params");
    create["state"]["camera"]["camera"]["extent"] = json!({"width":4,"height":4});
    create
}
struct Harness {
    control: SceneControl,
    worker: tokio::task::JoinHandle<()>,
    port: FrameSubmissions,
    clock: Arc<Clock>,
}
impl Harness {
    async fn new(frames: u32, inflight: u32, retained: u32) -> Self {
        let config = configuration(frames, inflight, retained);
        let (port, clock) = real_port(config.clone());
        let owner = SceneAdapter::new(config, registrations(), Box::new(port.clone())).unwrap();
        let (control, worker) = SceneControl::spawn(owner, 8).unwrap();
        call(&control, w::SCENE_CREATE, create()).await;
        Self {
            control,
            worker,
            port,
            clock,
        }
    }
    async fn image(&self, sequence: u64) -> (w::PublishedFrame, ProducerGuard<FrameMetadata>) {
        let (publish, guard) = registration(&self.port, sequence);
        let result = call(&self.control, w::FRAME_PUBLISH, json!(publish)).await;
        (serde_json::from_value(result).unwrap(), guard)
    }
    async fn submit(&self, name: &str, frames: Vec<w::FrameInput>) -> Value {
        call(&self.control, w::RENDER_SUBMIT, submit(name, frames)).await
    }
    async fn get(&self, name: &str) -> Value {
        call(&self.control, w::RENDER_GET, target_submission(name)).await
    }
    async fn stop(self) {
        drop(self.control);
        self.worker.await.unwrap();
    }
}
fn registration(
    port: &FrameSubmissions,
    sequence: u64,
) -> (w::FramePublish, ProducerGuard<FrameMetadata>) {
    let mut publish: w::FramePublish =
        serde_json::from_value(fixture("publish", "params")).unwrap();
    publish.frame.sequence = sequence.into();
    publish.position.ticks = (sequence + 10_000).into();
    publish.image.extent = w::Extent {
        width: 4,
        height: 4,
    };
    let metadata = FrameMetadata {
        frame: publish.frame.clone(),
        timeline: publish.timeline.clone(),
        position: publish.position.clone(),
        image: publish.image.clone(),
    };
    let (ticket, guard) = port.register(metadata, &[sequence as u8; 64]).unwrap();
    publish.producer_ticket = ticket.id().clone();
    (publish, guard)
}
fn input(image: &w::PublishedFrame) -> w::FrameInput {
    w::FrameInput {
        frame: image.frame.clone(),
        image_ref: image.image_ref.clone(),
        position: image.position.clone(),
        geometry: w::FrameGeometry::SceneCamera {},
    }
}
fn submit(name: &str, frames: Vec<w::FrameInput>) -> Value {
    json!({"runtime_id":"runtime-01","scene_id":"scene-demo","submission_id":name,
        "expected_scene_revision":null,"frames":frames})
}
fn target_submission(name: &str) -> Value {
    json!({"runtime_id":"runtime-01","submission_id":name})
}
#[derive(Default)]
struct Port {
    capacity: usize,
    frames: Vec<OutputFrame>,
}
impl OutputPort for Port {
    fn try_handoff(&mut self, frame: OutputFrame) -> Result<(), OutputFrame> {
        if self.frames.len() >= self.capacity {
            Err(frame)
        } else {
            self.frames.push(frame);
            Ok(())
        }
    }
}

#[tokio::test]
async fn reusing_a_released_submission_slot_does_not_reorder_ready_video_frames() {
    let h = Harness::new(4, 4, 4).await;
    for sequence in 0..2 {
        let (image, guard) = h.image(sequence).await;
        guard.ready().unwrap();
        guard.complete();
        h.submit(&format!("frame-{sequence}"), vec![input(&image)])
            .await;
    }
    let first = h.port.next_ready().unwrap().unwrap();
    assert_eq!(first.provenance.input.frame.sequence.get(), 0);
    first.rendered("first").unwrap();
    let mut output = Port {
        capacity: 4,
        ..Default::default()
    };
    h.port.handoff(&mut output).unwrap();
    output.frames[0]
        .observer
        .transmitted(id("output-stream"), 0.into())
        .unwrap();
    call(&h.control, w::RENDER_RELEASE, target_submission("frame-0")).await;

    let (image, guard) = h.image(2).await;
    guard.ready().unwrap();
    guard.complete();
    h.submit("frame-2", vec![input(&image)]).await;
    for sequence in 1..3 {
        let execution = h.port.next_ready().unwrap().unwrap();
        assert_eq!(
            execution.provenance.input.frame.sequence.get(),
            sequence,
            "a newly reused slot must not bypass an older ready frame"
        );
        execution.rendered(sequence).unwrap();
    }
    h.port.handoff(&mut output).unwrap();
    assert_eq!(
        output
            .frames
            .iter()
            .map(|frame| frame.provenance.input.frame.sequence.get())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    h.stop().await;
}

#[tokio::test]
async fn reordered_join_atomic_last_ref_snapshot_and_retained_retry() {
    let h = Harness::new(4, 2, 2).await;
    call(&h.control, w::SCENE_RELEASE, target("scene-demo")).await;
    let mut tracked = create();
    tracked["state"]["camera"] = json!({"mode":"tracked","near_m":0.25,"far_m":500});
    call(&h.control, w::SCENE_CREATE, tracked).await;
    let (b, gb) = h.image(18).await;
    let (a, ga) = h.image(17).await;
    let mut geometry = fixture("tracked_submit", "params")["frames"][0]["geometry"].clone();
    geometry["extent"] = json!({"width":4,"height":4});
    let geometry_a: w::FrameGeometry = serde_json::from_value(geometry.clone()).unwrap();
    geometry["ti"][2] = json!(0.25);
    let geometry_b: w::FrameGeometry = serde_json::from_value(geometry).unwrap();
    let mut join = FrameJoin::new(2).unwrap();
    join.image(b.clone()).unwrap();
    join.geometry(a.frame.clone(), geometry_a.clone()).unwrap();
    join.image(a.clone()).unwrap();
    join.geometry(b.frame.clone(), geometry_b.clone()).unwrap();
    let frames = join
        .finish(&a.runtime_id, &[a.frame.clone(), b.frame.clone()])
        .unwrap();
    let mut bad = frames.clone();
    bad[1].image_ref = id("missing-last");
    let error = h
        .control
        .call(w::RENDER_SUBMIT, Some(submit("bad", bad)))
        .await
        .unwrap_err();
    assert_eq!(error.code, 2002);
    assert_eq!(error.data.unwrap()["frame"], json!(b.frame));
    assert!(h.port.next_ready().unwrap().is_none());
    let mut mismatched = frames.clone();
    mismatched[1].position.ticks = 99.into();
    assert_eq!(
        h.control
            .call(w::RENDER_SUBMIT, Some(submit("bad", mismatched)))
            .await
            .unwrap_err()
            .code,
        2004
    );
    let accepted = h.submit("batch", frames.clone()).await;
    assert_eq!(accepted["effect"]["sequence"], "4");
    assert_eq!(accepted["scene_revision"], "0");
    assert_eq!(accepted["frames"][0]["frame"], json!(a.frame));
    call(
        &h.control,
        w::SCENE_MUTATE,
        mutate(json!([{"kind":"shadow_mode_set","shadow_mode":"off"}]), 0),
    )
    .await;
    call(&h.control, w::SCENE_RELEASE, target("scene-demo")).await;
    assert_eq!(h.submit("batch", frames.clone()).await, accepted);
    let mut different = frames;
    different[0].position.ticks = 123.into();
    assert_eq!(
        h.control
            .call(w::RENDER_SUBMIT, Some(submit("batch", different)))
            .await
            .unwrap_err()
            .code,
        2003
    );
    for image in [&a, &b] {
        call(
            &h.control,
            w::FRAME_RELEASE,
            json!({"runtime_id":image.runtime_id,"image_ref":image.image_ref}),
        )
        .await;
    }
    assert!(h.port.next_ready().unwrap().is_none());
    gb.ready().unwrap();
    gb.complete();
    let work_b = h.port.next_ready().unwrap().unwrap();
    assert_eq!(work_b.provenance.input.frame, b.frame);
    assert_eq!(work_b.provenance.input.geometry, geometry_b);
    assert_eq!(work_b.input.pixels().unwrap()[0], 18);
    ga.ready().unwrap();
    ga.complete();
    let work_a = h.port.next_ready().unwrap().unwrap();
    assert_eq!(work_a.provenance.input.frame, a.frame);
    assert_eq!(work_a.provenance.input.geometry, geometry_a);
    assert_eq!(work_a.scene.snapshot().scene_revision.get(), 0);
    assert_eq!(
        work_a.scene.snapshot().state.shadow_mode,
        w::ShadowMode::Pcf
    );
    work_a.rendered(vec![1_u8, 2, 3]).unwrap();
    work_b.failed().unwrap();
    let snapshot = h.get("batch").await;
    assert_eq!(snapshot["frames"][0]["state"], "rendered");
    assert_eq!(snapshot["frames"][1]["reason"], "render_failed");
    let mut output = Port::default();
    h.port.handoff(&mut output).unwrap();
    assert_eq!(
        h.get("batch").await["frames"][0]["delivery"]["reason"],
        "queue_full"
    );
    call(&h.control, w::RENDER_RELEASE, target_submission("batch")).await;
    h.stop().await;
}

#[tokio::test]
async fn deadlines_and_write_failure_do_not_rollback_siblings_or_follow_media_time() {
    let h = Harness::new(4, 3, 2).await;
    let (a, ga) = h.image(17).await;
    let (b, gb) = h.image(18).await;
    let (c, gc) = h.image(19).await;
    h.submit("batch", vec![input(&a), input(&b), input(&c)])
        .await;
    ga.ready().unwrap();
    ga.complete();
    let work = h.port.next_ready().unwrap().unwrap();
    work.rendered(17_u64).unwrap();
    gb.fail().unwrap();
    h.clock.set(99);
    let snapshot = h.get("batch").await;
    assert_eq!(snapshot["frames"][0]["state"], "rendered");
    assert_eq!(snapshot["frames"][1]["reason"], "write_failed");
    assert_eq!(snapshot["frames"][2]["state"], "pending");
    h.clock.set(100);
    let snapshot = h.get("batch").await;
    assert_eq!(snapshot["frames"][0]["state"], "rendered");
    assert_eq!(snapshot["frames"][2]["reason"], "deadline_exceeded");
    assert_eq!(snapshot["frames"][2]["position"], json!(c.position));
    gb.complete();
    gc.complete();
    // Rendered but not yet handed off is cancellable; no output can escape.
    let cancelled = call(&h.control, w::RENDER_CANCEL, target_submission("batch")).await;
    assert_eq!(cancelled["cancelled"], json!([a.frame]));
    let mut output = Port {
        capacity: 1,
        ..Port::default()
    };
    h.port.handoff(&mut output).unwrap();
    assert!(output.frames.is_empty());
    h.stop().await;
}

#[tokio::test]
async fn running_cancel_release_stays_charged_until_actual_completion() {
    let h = Harness::new(2, 1, 1).await;
    let (a, ga) = h.image(17).await;
    ga.ready().unwrap();
    ga.complete();
    h.submit("running", vec![input(&a)]).await;
    let work = h.port.next_ready().unwrap().unwrap();
    call(
        &h.control,
        w::FRAME_RELEASE,
        json!({"runtime_id":a.runtime_id,"image_ref":a.image_ref}),
    )
    .await;
    call(&h.control, w::RENDER_CANCEL, target_submission("running")).await;
    assert_eq!(
        call(&h.control, w::RENDER_RELEASE, target_submission("running")).await["released"],
        true
    );
    assert_eq!(
        h.control
            .call(w::RENDER_GET, Some(target_submission("running")))
            .await
            .unwrap_err()
            .code,
        2002
    );
    let (b, gb) = h.image(18).await;
    assert_eq!(
        h.control
            .call(w::RENDER_SUBMIT, Some(submit("new", vec![input(&b)])))
            .await
            .unwrap_err()
            .code,
        2005
    );
    assert!(
        h.port
            .register(work.input.metadata().clone(), &[0; 64])
            .is_err()
    );
    assert_eq!(work.input.pixels().unwrap()[0], 17);
    work.rendered(17_u64).unwrap();
    h.port.tick().unwrap();
    h.submit("new", vec![input(&b)]).await;
    let mut output = Port {
        capacity: 1,
        ..Port::default()
    };
    h.port.handoff(&mut output).unwrap();
    assert!(output.frames.is_empty());
    // A pending producer remains charged by #21 even after scheduler cancellation.
    call(&h.control, w::RENDER_CANCEL, target_submission("new")).await;
    call(&h.control, w::RENDER_RELEASE, target_submission("new")).await;
    gb.complete();
    h.stop().await;
}

#[tokio::test]
async fn timeout_observed_while_running_and_abandoned_observer_fails_closed() {
    let h = Harness::new(2, 1, 1).await;
    let (a, ga) = h.image(17).await;
    ga.ready().unwrap();
    ga.complete();
    h.submit("running", vec![input(&a)]).await;
    let work = h.port.next_ready().unwrap().unwrap();
    h.clock.set(100);
    assert_eq!(
        h.get("running").await["frames"][0]["reason"],
        "deadline_exceeded"
    );
    call(&h.control, w::RENDER_RELEASE, target_submission("running")).await;
    drop(work);
    h.port.tick().unwrap();
    assert_eq!(
        h.control
            .call(w::RENDER_SUBMIT, Some(submit("new", vec![input(&a)])))
            .await
            .unwrap_err()
            .code,
        2005
    );
    h.stop().await;

    let h = Harness::new(1, 1, 1).await;
    let (a, ga) = h.image(17).await;
    ga.ready().unwrap();
    ga.complete();
    h.submit("abandoned", vec![input(&a)]).await;
    drop(h.port.next_ready().unwrap().unwrap());
    assert_eq!(
        h.get("abandoned").await["frames"][0]["reason"],
        "runtime_failed"
    );
    h.stop().await;
}

#[tokio::test]
async fn actual_delivery_first_slot_output_hold_and_stale_observation() {
    let h = Harness::new(2, 2, 2).await;
    let (a, ga) = h.image(17).await;
    ga.ready().unwrap();
    ga.complete();
    h.submit("same-id", vec![input(&a)]).await;
    h.port
        .next_ready()
        .unwrap()
        .unwrap()
        .rendered(vec![42_u8; 64])
        .unwrap();
    let mut output = Port {
        capacity: 1,
        ..Port::default()
    };
    h.port.handoff(&mut output).unwrap();
    assert_eq!(
        h.get("same-id").await["frames"][0]["delivery"]["state"],
        "queued"
    );
    assert_eq!(
        h.control
            .call(w::RENDER_RELEASE, Some(target_submission("same-id")))
            .await
            .unwrap_err()
            .code,
        2006
    );
    let cancelled = call(&h.control, w::RENDER_CANCEL, target_submission("same-id")).await;
    assert_eq!(cancelled["too_late"], json!([a.frame]));
    let observer = output.frames[0].observer.clone();
    assert_ne!(output.frames[0].resource.id(), &a.image_ref);
    observer
        .transmitted(id("sdi-synthetic"), 71.into())
        .unwrap();
    observer.transmitted(id("other"), 99.into()).unwrap();
    observer.failed().unwrap();
    let snapshot = h.get("same-id").await;
    assert_eq!(
        snapshot["frames"][0]["delivery"],
        json!({"state":"transmitted","output_stream_id":"sdi-synthetic","first_slot":"71"})
    );
    call(&h.control, w::RENDER_RELEASE, target_submission("same-id")).await;
    assert_eq!(
        output.frames[0].resource.payload::<Vec<u8>>().unwrap()[0],
        42
    );
    // Caller reuse is forbidden by contract; native stale events still cannot alias it.
    h.submit("same-id", vec![input(&a)]).await;
    observer.transmitted(id("stale"), 999.into()).unwrap();
    assert_eq!(h.get("same-id").await["frames"][0]["state"], "pending");
    h.port
        .next_ready()
        .unwrap()
        .unwrap()
        .rendered(0_u8)
        .unwrap();
    output.frames.clear();
    h.port.handoff(&mut output).unwrap();
    output.frames.clear();
    assert_eq!(
        h.get("same-id").await["frames"][0]["delivery"]["reason"],
        "output_failed"
    );
    h.stop().await;
}

#[tokio::test]
async fn stale_physical_completion_cannot_update_reused_submission_id() {
    let h = Harness::new(1, 2, 2).await;
    let (a, ga) = h.image(17).await;
    ga.ready().unwrap();
    ga.complete();
    h.submit("same", vec![input(&a)]).await;
    let old = h.port.next_ready().unwrap().unwrap();
    call(&h.control, w::RENDER_CANCEL, target_submission("same")).await;
    call(&h.control, w::RENDER_RELEASE, target_submission("same")).await;
    h.submit("same", vec![input(&a)]).await;
    old.rendered(123_u64).unwrap();
    assert_eq!(h.get("same").await["frames"][0]["state"], "pending");
    h.port.next_ready().unwrap().unwrap().failed().unwrap();
    assert_eq!(h.get("same").await["frames"][0]["reason"], "render_failed");
    h.stop().await;
}

#[test]
fn native_join_missing_duplicate_capacity_and_runtime_are_explicit() {
    let image = w::PublishedFrame {
        runtime_id: id("runtime-01"),
        frame: w::FrameKey {
            stream_id: id("stream-01"),
            sequence: 1.into(),
        },
        image_ref: id("image"),
        position: w::Position {
            timeline_id: id("time"),
            ticks: 9.into(),
        },
    };
    let mut join = FrameJoin::new(1).unwrap();
    join.image(image.clone()).unwrap();
    assert_eq!(join.image(image.clone()).unwrap_err().code, 2003);
    assert_eq!(
        join.finish(&image.runtime_id, std::slice::from_ref(&image.frame))
            .unwrap_err()
            .code,
        2004
    );
    let mut join = FrameJoin::new(1).unwrap();
    join.image(image.clone()).unwrap();
    let mut other = image.clone();
    other.frame.sequence = 2.into();
    assert_eq!(join.image(other).unwrap_err().code, 2005);
    join.geometry(image.frame.clone(), w::FrameGeometry::SceneCamera {})
        .unwrap();
    assert_eq!(
        join.finish(&id("old"), &[image.frame]).unwrap_err().code,
        2001
    );
}

#[tokio::test]
async fn deadline_overflow_and_regression_reject_without_an_effect() {
    let h = Harness::new(1, 1, 1).await;
    let (a, ga) = h.image(17).await;
    h.clock.set(u64::MAX - 50);
    assert_eq!(
        h.control
            .call(w::RENDER_SUBMIT, Some(submit("overflow", vec![input(&a)])))
            .await
            .unwrap_err()
            .code,
        2005
    );
    h.clock.set(1);
    assert_eq!(h.port.tick().unwrap_err().code, 2006);
    let scene = call(&h.control, w::SCENE_GET, target("scene-demo")).await;
    assert_eq!(scene["effect"]["sequence"], "1");
    ga.complete();
    h.stop().await;
}

#[tokio::test]
async fn installed_producer_capability_cannot_publish_another_allowed_stream_ticket() {
    let config = configuration(2, 2, 2);
    let mut resources = resources(&config);
    let installed = resources.producer(&id("stream-01")).unwrap();
    let other = resources.producer(&id("other-stream")).unwrap();
    let mut publish: w::FramePublish =
        serde_json::from_value(fixture("publish", "params")).unwrap();
    publish.frame.stream_id = id("other-stream");
    publish.image.extent = config.native_extent;
    let metadata = FrameMetadata {
        frame: publish.frame.clone(),
        timeline: publish.timeline.clone(),
        position: publish.position.clone(),
        image: publish.image.clone(),
    };
    let (ticket, guard) = resources.register(&other, metadata, &[0; 64]).unwrap();
    publish.producer_ticket = ticket.id().clone();
    let port = FrameSubmissions::new(
        config.clone(),
        resources,
        installed,
        Arc::new(Clock::default()),
    )
    .unwrap();
    let adapter =
        SceneAdapter::new(config.clone(), registrations(), Box::new(port.clone())).unwrap();
    assert!(SceneAdapter::new(config, registrations(), Box::new(port.clone())).is_err());
    let (control, worker) = SceneControl::spawn(adapter, 2).unwrap();
    assert_eq!(
        control
            .call(w::FRAME_PUBLISH, Some(json!(publish)))
            .await
            .unwrap_err()
            .code,
        2004
    );
    publish.frame.stream_id = id("stream-01");
    assert_eq!(
        control
            .call(w::FRAME_PUBLISH, Some(json!(publish)))
            .await
            .unwrap_err()
            .code,
        2004
    );
    assert!(port.cancel_write(&ticket).unwrap());
    assert!(!port.cancel_write(&ticket).unwrap());
    guard.complete();
    drop(control);
    worker.await.unwrap();
}

#[tokio::test]
async fn terminal_receipts_remain_bounded_until_explicit_release() {
    let h = Harness::new(1, 1, 1).await;
    let (a, guard) = h.image(17).await;
    guard.ready().unwrap();
    guard.complete();
    let accepted = h.submit("retained", vec![input(&a)]).await;
    h.port.next_ready().unwrap().unwrap().failed().unwrap();
    assert_eq!(
        h.get("retained").await["frames"][0]["reason"],
        "render_failed"
    );
    assert_eq!(
        h.control
            .call(w::RENDER_SUBMIT, Some(submit("new", vec![input(&a)])))
            .await
            .unwrap_err()
            .code,
        2005
    );
    assert_eq!(h.submit("retained", vec![input(&a)]).await, accepted);
    call(&h.control, w::RENDER_RELEASE, target_submission("retained")).await;
    let accepted = h.submit("new", vec![input(&a)]).await;
    assert_eq!(accepted["effect"]["sequence"], "3");
    call(&h.control, w::RENDER_CANCEL, target_submission("new")).await;
    h.stop().await;
}

#[tokio::test]
async fn undispatched_cancel_frees_admission_but_not_pending_producer_storage() {
    let h = Harness::new(1, 1, 1).await;
    let (a, guard) = h.image(17).await;
    h.submit("pending", vec![input(&a)]).await;
    call(&h.control, w::RENDER_CANCEL, target_submission("pending")).await;
    call(&h.control, w::RENDER_RELEASE, target_submission("pending")).await;
    call(
        &h.control,
        w::FRAME_RELEASE,
        json!({"runtime_id":a.runtime_id,"image_ref":a.image_ref}),
    )
    .await;
    let publish: w::FramePublish = serde_json::from_value(fixture("publish", "params")).unwrap();
    let metadata = FrameMetadata {
        frame: a.frame.clone(),
        timeline: publish.timeline,
        position: a.position.clone(),
        image: w::ImageDescription {
            extent: w::Extent {
                width: 4,
                height: 4,
            },
            ..publish.image
        },
    };
    assert!(matches!(h.port.register(metadata, &[0; 64]), Err(e) if e.code == 2005));
    assert_eq!(
        h.control
            .call(w::RENDER_SUBMIT, Some(submit("new", vec![input(&a)])))
            .await
            .unwrap_err()
            .code,
        2002
    );
    guard.complete();
    let (b, guard) = h.image(18).await;
    assert_ne!(a.image_ref, b.image_ref);
    h.submit("new", vec![input(&b)]).await;
    guard.complete();
    assert_eq!(h.get("new").await["frames"][0]["reason"], "write_failed");
    h.stop().await;
}

#[tokio::test]
async fn replay_batch_sizes_preserve_resolved_scene_boundaries_and_positions() {
    async fn replay(
        batch_size: usize,
        elapsed: u64,
    ) -> Vec<(w::FrameKey, w::Position, w::U64, w::ShadowMode)> {
        let h = Harness::new(4, 4, 4).await;
        let mut images = Vec::new();
        for sequence in [20, 19, 18, 17] {
            let (image, guard) = h.image(sequence).await;
            guard.ready().unwrap();
            guard.complete();
            images.push(image);
        }
        images.reverse();
        let mut observed = Vec::new();
        for (boundary, group) in images.chunks(2).enumerate() {
            if boundary == 1 {
                call(
                    &h.control,
                    w::SCENE_MUTATE,
                    mutate(json!([{"kind":"shadow_mode_set","shadow_mode":"off"}]), 0),
                )
                .await;
            }
            for (batch, frames) in group.chunks(batch_size).enumerate() {
                let name = format!("b-{boundary}-{batch}");
                h.submit(&name, frames.iter().map(input).collect()).await;
                h.clock.set(h.clock.now_ms() + elapsed);
                while let Some(work) = h.port.next_ready().unwrap() {
                    observed.push((
                        work.provenance.input.frame.clone(),
                        work.provenance.input.position.clone(),
                        work.scene.snapshot().scene_revision,
                        work.scene.snapshot().state.shadow_mode,
                    ));
                    work.failed().unwrap();
                }
                call(&h.control, w::RENDER_RELEASE, target_submission(&name)).await;
            }
        }
        h.stop().await;
        observed
    }
    assert_eq!(replay(1, 0).await, replay(2, 40).await);
}

#[cfg(unix)]
mod real_ipc {
    use super::*;
    use chr_common::{
        client::NativeSocketClient,
        server::{IpcServer, NativeSocketListener, ServerConfig, WebSocketListener},
    };
    use chr_runtime::{
        control::ControlHandler,
        sdi::{SdiOutput, Taps, VideoMode},
    };
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

    async fn rpc(
        websocket: bool,
        native: &mut NativeSocketClient,
        ws: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
        method: &str,
        params: Value,
    ) -> Value {
        let text =
            json!({"jsonrpc":"2.0","id":"not-a-frame","method":method,"params":params}).to_string();
        let response: Value = if websocket {
            ws.send(Message::text(text)).await.unwrap();
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap()
        } else {
            serde_json::from_slice(&native.send_raw(text.as_bytes()).await.unwrap().unwrap())
                .unwrap()
        };
        if let Some(result) = response.get("result") {
            w::validate_result(method, result.clone()).unwrap();
        }
        response
    }

    #[tokio::test]
    async fn both_ipcs_publish_and_admit_real_resources_through_the_same_control_queue() {
        tokio::time::timeout(std::time::Duration::from_secs(20), conformance())
            .await
            .unwrap();
    }
    async fn conformance() {
        let config = configuration(2, 2, 2);
        let (port, _clock) = real_port(config.clone());
        let adapter = SceneAdapter::new(config, registrations(), Box::new(port.clone())).unwrap();
        let (control, worker) = SceneControl::spawn(adapter, 8).unwrap();
        let (commands, receiver) = tokio::sync::mpsc::channel(1);
        drop(receiver);
        let sdi = SdiOutput::spawn(
            VideoMode {
                width: 4,
                height: 4,
                fps: chr_common::rate::Rate::fps(50),
            },
            Taps::default(),
        )
        .unwrap();
        let handler = Arc::new(ControlHandler::new(commands, sdi.stats()).with_scenes(control));
        let server = IpcServer::new(Arc::clone(&handler), ServerConfig::default());
        let directory = std::path::PathBuf::from("../../output/frame-submissions");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("frames-{}.sock", std::process::id()));
        let native_listener = NativeSocketListener::bind(&path).await.unwrap();
        let ws_listener = WebSocketListener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let url = format!("ws://{}", ws_listener.local_addr().unwrap());
        let native_server = server.clone();
        let native_task =
            tokio::spawn(async move { native_server.serve_native_socket(native_listener).await });
        let ws_task = tokio::spawn(async move { server.serve_websocket(ws_listener).await });
        let mut native = NativeSocketClient::connect(&path).await.unwrap();
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        assert_eq!(
            rpc(false, &mut native, &mut ws, w::SCENE_CREATE, create()).await["result"]["scene_revision"],
            "0"
        );
        assert_eq!(
            rpc(
                true,
                &mut native,
                &mut ws,
                w::SCENE_GET,
                target("scene-demo")
            )
            .await["result"]["scene_revision"],
            "0"
        );
        for (index, websocket) in [false, true].into_iter().enumerate() {
            let (publish, guard) = registration(&port, 17 + index as u64);
            let mut bad = json!(publish);
            bad["runtime_id"] = json!("old-runtime");
            bad["producer_ticket"] = json!("unknown");
            assert_eq!(
                rpc(websocket, &mut native, &mut ws, w::FRAME_PUBLISH, bad).await["error"]["code"],
                2001
            );
            let mut bad = json!(publish);
            bad["frame"]["stream_id"] = json!("other-stream");
            assert_eq!(
                rpc(websocket, &mut native, &mut ws, w::FRAME_PUBLISH, bad).await["error"]["code"],
                2004
            );
            let mut bad = json!(publish);
            bad["position"]["ticks"] = json!("1");
            let rejected = rpc(websocket, &mut native, &mut ws, w::FRAME_PUBLISH, bad).await;
            assert_eq!(rejected["error"]["code"], 2004);
            assert_eq!(rejected["error"]["data"]["frame"], json!(publish.frame));
            let mut bad = json!(publish);
            bad["ready"] = json!(true);
            assert_eq!(
                rpc(websocket, &mut native, &mut ws, w::FRAME_PUBLISH, bad).await["error"]["code"],
                -32602
            );
            let published = rpc(
                websocket,
                &mut native,
                &mut ws,
                w::FRAME_PUBLISH,
                json!(publish),
            )
            .await;
            let image: w::PublishedFrame =
                serde_json::from_value(published["result"].clone()).unwrap();
            assert_eq!(
                rpc(
                    !websocket,
                    &mut native,
                    &mut ws,
                    w::FRAME_PUBLISH,
                    json!(publish)
                )
                .await["error"]["code"],
                2002
            );
            let name = format!("sub-{index}");
            let target = target_submission(&name);
            let request = submit(&name, vec![input(&image)]);
            let mut bad = request.clone();
            bad["frames"][0]["frame"]["sequence"] = json!("100");
            assert_eq!(
                rpc(websocket, &mut native, &mut ws, w::RENDER_SUBMIT, bad).await["error"]["code"],
                2004
            );
            let accepted = rpc(
                websocket,
                &mut native,
                &mut ws,
                w::RENDER_SUBMIT,
                request.clone(),
            )
            .await;
            assert_eq!(
                accepted["result"]["effect"]["sequence"],
                (index + 2).to_string()
            );
            assert_eq!(
                rpc(!websocket, &mut native, &mut ws, w::RENDER_SUBMIT, request).await,
                accepted
            );
            assert_eq!(
                rpc(
                    websocket,
                    &mut native,
                    &mut ws,
                    w::RENDER_GET,
                    target.clone()
                )
                .await["result"]["frames"][0]["state"],
                "pending"
            );
            assert!(port.next_ready().unwrap().is_none());
            let image_target = json!({"runtime_id":image.runtime_id,"image_ref":image.image_ref});
            assert_eq!(
                rpc(
                    websocket,
                    &mut native,
                    &mut ws,
                    w::FRAME_RELEASE,
                    image_target.clone()
                )
                .await["result"]["released"],
                true
            );
            assert_eq!(
                rpc(
                    !websocket,
                    &mut native,
                    &mut ws,
                    w::FRAME_RELEASE,
                    image_target
                )
                .await["result"]["released"],
                false
            );
            assert_eq!(
                rpc(
                    websocket,
                    &mut native,
                    &mut ws,
                    w::RENDER_CANCEL,
                    target.clone()
                )
                .await["result"]["cancelled"],
                json!([image.frame])
            );
            assert_eq!(
                rpc(
                    !websocket,
                    &mut native,
                    &mut ws,
                    w::RENDER_GET,
                    target.clone()
                )
                .await["result"]["frames"][0]["reason"],
                "cancelled"
            );
            assert_eq!(
                rpc(
                    websocket,
                    &mut native,
                    &mut ws,
                    w::RENDER_RELEASE,
                    target.clone()
                )
                .await["result"]["released"],
                true
            );
            assert_eq!(
                rpc(
                    !websocket,
                    &mut native,
                    &mut ws,
                    w::RENDER_RELEASE,
                    target.clone()
                )
                .await["result"]["released"],
                false
            );
            for method in [w::RENDER_GET, w::RENDER_CANCEL] {
                assert_eq!(
                    rpc(websocket, &mut native, &mut ws, method, target.clone()).await["error"]["code"],
                    2002
                );
            }
            guard.complete();
        }
        ws.close(None).await.unwrap();
        drop(ws);
        drop(native);
        native_task.abort();
        ws_task.abort();
        let _ = native_task.await;
        let _ = ws_task.await;
        drop(handler);
        worker.await.unwrap();
        drop(sdi);
        assert!(!path.exists());
    }
}
