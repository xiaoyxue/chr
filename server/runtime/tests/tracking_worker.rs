//! The supervisor driving the real `chr-tracking-sim-cli` worker over the
//! VchaoFrame contract, and a worker played by the test itself for malformed
//! returns.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use chr_common::{methods::render_worker::WorkerState, scene_frame as w};
use chr_runtime::{
    calibration::lens::CHECKS,
    worker::{
        tracking::{
            supervisor::{StatusHandle, Tracker, TrackerConfig, status_handle},
            wire::{Received, attach},
        },
        transport::{Inbound, Receiver, Sender, SharedPixels},
    },
};
use chr_tracker::{
    lens,
    pose::Answer,
    protocol::{Endpoint, Pose, PoseStatus, Selection},
    replay::TrackingReplay,
};

const WIDTH: u32 = 4;
const HEIGHT: u32 = 2;
const TI: [[f64; 3]; 3] = [[0.02, 0., 0.], [0., 0.02, 0.], [0., 0., 1.]];
const WARPS: [[[f64; 3]; 3]; 2] = [
    [[0.001, 0., -1.], [0., 0.001, -0.5], [0., 0., 1.]],
    [[0.001, 0., -0.9], [0., 0.001, -0.5], [0., 0., 1.]],
];
const CENTRE: [f64; 3] = [14.52857216404143, 60.620266710663365, -9.91514362632655];

fn radial(k1: f64) -> w::RadialLens {
    lens::wire([2., 1.], 2., [k1, 0., 0.], None, &CHECKS).unwrap()
}

struct Fixture {
    dir: PathBuf,
    status: StatusHandle,
    tracker: Option<Tracker>,
}

impl Fixture {
    fn new(name: &str, args: &[&str], hang: Duration) -> Self {
        Self::with(name, None, args, hang, 16)
    }

    /// A tracker of `executable` (default: the stand-in replaying a manifest).
    fn with(
        name: &str,
        executable: Option<PathBuf>,
        args: &[&str],
        hang: Duration,
        in_flight: usize,
    ) -> Self {
        let dir = std::env::temp_dir().join(format!("chr-tracking-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("scene.json");
        std::fs::write(
            &manifest,
            serde_json::json!({"ti": TI, "center": [0., 0., -10.], "frame_warps": WARPS,
                "radial": {"centre_px": [2., 1.], "scale_px": 2., "coefficients": [-0.05, 0., 0.],
                    "support": null}})
            .to_string(),
        )
        .unwrap();
        let mut worker_args = vec!["--manifest".to_owned(), manifest.display().to_string()];
        worker_args.extend(args.iter().map(|arg| (*arg).to_owned()));
        let status = status_handle();
        let tracker = Tracker::new(
            TrackerConfig {
                executable: executable
                    .unwrap_or_else(|| env!("CARGO_BIN_EXE_chr-tracking-sim-cli").into()),
                args: worker_args,
                width: WIDTH,
                height: HEIGHT,
                fps: 50,
                camera_center: CENTRE,
                socket_dir: dir.join("sockets"),
                hang,
                in_flight,
            },
            status.clone(),
        )
        .unwrap();
        Self {
            dir,
            status,
            tracker: Some(tracker),
        }
    }

    fn tracker(&mut self) -> &mut Tracker {
        self.tracker.as_mut().unwrap()
    }

    fn frame(sequence: u64) -> SharedPixels {
        SharedPixels::copy_from(&vec![sequence as u8; (WIDTH * HEIGHT * 4) as usize]).unwrap()
    }

    /// Polls until `done` holds, collecting answers.
    fn until(&mut self, done: impl Fn(&Self, &[Answer]) -> bool) -> Vec<Answer> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut answers = Vec::new();
        while !done(self, &answers) {
            assert!(Instant::now() < deadline, "timed out: {:?}", self.status());
            answers.extend(self.tracker().poll());
            std::thread::sleep(Duration::from_millis(2));
        }
        answers
    }

    fn ready(&mut self) {
        self.until(|fixture, _| fixture.tracker.as_ref().unwrap().is_ready());
    }

    fn status(&self) -> chr_common::methods::tracking::TrackerStatus {
        self.status.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.tracker.take());
        assert!(
            !self.dir.join("sockets").exists(),
            "the sockets go with the tracker"
        );
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn sequences(answers: &[Answer]) -> Vec<u64> {
    answers.iter().map(|answer| answer.sequence).collect()
}

#[test]
fn every_sent_frame_comes_back_with_the_recorded_ti_and_the_station_centre() {
    let mut fixture = Fixture::new("replay", &[], Duration::from_secs(2));
    fixture.tracker().track(0, &Fixture::frame(0));
    assert_eq!(fixture.status().not_sent, 1, "no worker is ready yet");
    fixture.ready();
    for sequence in 10..14 {
        fixture.tracker().track(sequence, &Fixture::frame(sequence));
    }
    let answers = fixture.until(|_, answers| answers.len() == 4);
    assert_eq!(sequences(&answers), [10, 11, 12, 13], "in order");
    let replay = TrackingReplay::new(TI, [0., 0., -10.], radial(-0.05), &WARPS, &CHECKS).unwrap();
    let ti = replay.ti();
    for answer in &answers {
        assert_eq!((answer.pose, answer.overdue), (PoseStatus::Fresh, false));
        let camera = answer.camera.as_ref().unwrap();
        assert_eq!(camera.sequence.get(), answer.sequence);
        assert_eq!(camera.ti.map(|v| v.get().to_bits()), ti.map(f64::to_bits));
        assert_eq!(
            camera.camera_center.map(|v| v.get().to_bits()),
            CENTRE.map(f64::to_bits),
            "the configured station centre, not the manifest's"
        );
        assert_eq!(camera.lens, Some(radial(-0.05)), "the manifest's lens");
        assert_eq!(camera.frame_warp, None, "the replay sends no warp");
    }
    let status = fixture.status();
    assert_eq!((status.requests, status.cameras), (4, 4));
    assert_eq!(status.state, WorkerState::Ready);
    assert_eq!(status.worker, "chr-tracking-sim-cli");
    assert_eq!(
        status.stream_id,
        Some(format!("chr-{}-1", std::process::id()))
    );
}

#[test]
fn unavailable_frames_are_counted_and_a_crashed_worker_comes_back_as_a_new_stream() {
    let mut fixture = Fixture::new(
        "crash",
        &["--unavailable-every", "2", "--exit-after", "4"],
        Duration::from_secs(2),
    );
    fixture.ready();
    for sequence in 0..4 {
        fixture.tracker().track(sequence, &Fixture::frame(sequence));
    }
    // Frames 0 and 2 get cameras, 1 is unavailable, and the 4th frame crashes the worker.
    let answers = fixture.until(|fixture, _| {
        fixture.status().restarts == 0 && fixture.status().state == WorkerState::Backoff
    });
    let status = fixture.status();
    assert_eq!(sequences(&answers), [0, 1, 2], "{status:?}");
    assert_eq!(answers[1].pose, PoseStatus::Unavailable);
    assert_eq!(answers[1].camera, None);
    assert_eq!((status.cameras, status.unavailable), (2, 1));
    assert!(status.last_error.unwrap().contains("exited"));
    fixture.ready();
    let status = fixture.status();
    assert_eq!(status.restarts, 1);
    assert_eq!(
        status.stream_id,
        Some(format!("chr-{}-2", std::process::id()))
    );
    // The new stream starts at frame_id 0; its first frame is not unavailable.
    fixture.tracker().track(8, &Fixture::frame(8));
    let answers = fixture.until(|_, answers| !answers.is_empty());
    assert_eq!(sequences(&answers), [8], "the new worker answers");
    assert_eq!(answers[0].pose, PoseStatus::Fresh);
}

#[test]
fn a_hung_worker_is_replaced() {
    let mut fixture = Fixture::new("hang", &["--hang-after", "2"], Duration::from_millis(150));
    fixture.ready();
    fixture.tracker().track(0, &Fixture::frame(0));
    fixture.tracker().track(1, &Fixture::frame(1));
    let answers = fixture.until(|fixture, _| fixture.status().state == WorkerState::Backoff);
    assert_eq!(sequences(&answers), [0], "frame 1 never came back");
    assert!(
        fixture
            .status()
            .last_error
            .unwrap()
            .contains("held a frame"),
        "{:?}",
        fixture.status()
    );
    fixture.ready();
    assert_eq!(fixture.status().restarts, 1);
}

#[test]
fn a_worker_holding_too_many_frames_is_replaced_not_skipped() {
    let mut fixture = Fixture::with(
        "full",
        None,
        &["--hang-after", "1"],
        Duration::from_secs(10),
        2,
    );
    fixture.ready();
    for sequence in 0..3 {
        fixture.tracker().track(sequence, &Fixture::frame(sequence));
    }
    let status = fixture.status();
    assert_eq!((status.requests, status.not_sent), (2, 1));
    assert_eq!(status.state, WorkerState::Backoff);
    assert!(
        status
            .last_error
            .as_ref()
            .unwrap()
            .contains("holds 2 frames"),
        "{status:?}"
    );
}

#[test]
fn delayed_frames_keep_flowing_and_forgotten_frames_are_never_answered() {
    let mut fixture = Fixture::new("forget", &["--delay-ms", "100"], Duration::from_secs(2));
    fixture.ready();
    let start = Instant::now();
    fixture.tracker().track(5, &Fixture::frame(5));
    fixture.tracker().forget();
    for sequence in 6..10 {
        fixture.tracker().track(sequence, &Fixture::frame(sequence));
    }
    let answers = fixture.until(|_, answers| answers.len() == 4);
    let elapsed = start.elapsed();
    std::thread::sleep(Duration::from_millis(50));
    assert!(fixture.tracker().poll().is_empty());
    assert_eq!(
        sequences(&answers),
        [6, 7, 8, 9],
        "frame 5 was forgotten after it was sent"
    );
    // Held 100 ms each but together, like a lookahead, not one after another.
    assert!(
        elapsed >= Duration::from_millis(100) && elapsed < Duration::from_millis(400),
        "{elapsed:?}"
    );
    assert_eq!(fixture.status().requests, 5);
}

#[test]
fn an_unreadable_manifest_exits_the_worker_and_backs_off() {
    let mut fixture = Fixture::new("fatal", &[], Duration::from_secs(2));
    std::fs::write(fixture.dir.join("scene.json"), "{}").unwrap();
    fixture.until(|fixture, _| fixture.status().last_error.is_some());
    let status = fixture.status();
    assert_eq!(status.state, WorkerState::Backoff);
    assert!(
        status.last_error.as_ref().unwrap().contains("exited"),
        "{status:?}"
    );
    assert!(!fixture.tracker().is_ready());
}

/// The test plays the worker: it returns frame 0 with a fresh, overdue pose,
/// changed by `corrupt`. Returns the answers and the status once the tracker
/// took the frame or failed the worker.
fn return_frame(
    name: &str,
    corrupt: impl FnOnce(&mut Received),
) -> (Vec<Answer>, chr_common::methods::tracking::TrackerStatus) {
    let mut corrupt = Some(corrupt);
    return_frames(name, 1, |_, frame| (corrupt.take().unwrap())(frame))
}

/// [`return_frame`] for `count` frames (capture sequences 3, 4, ...), each
/// changed by `corrupt(frame_id, frame)`.
fn return_frames(
    name: &str,
    count: u64,
    mut corrupt: impl FnMut(u64, &mut Received),
) -> (Vec<Answer>, chr_common::methods::tracking::TrackerStatus) {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("chr-tracking-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A worker process that does nothing: the test speaks for it.
    let idle = dir.join("idle.sh");
    std::fs::write(&idle, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&idle, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut fixture = Fixture::with(name, Some(idle), &[], Duration::from_secs(5), 8);
    fixture.tracker().poll();
    let sockets = fixture.dir.join("sockets");
    let output = Sender::listen_live(&sockets.join("output.sock"), None, 4).unwrap();
    let input = Receiver::connect(&sockets.join("input.sock")).unwrap();
    fixture.ready();
    for frame_id in 0..count {
        fixture
            .tracker()
            .track(3 + frame_id, &Fixture::frame(3 + frame_id));
        let mut frame = Received::decode(input.reader().recv_sample().unwrap().unwrap()).unwrap();
        assert_eq!(
            (frame.meta.frame_id, frame.meta.selection.clone()),
            (frame_id, None)
        );
        if frame_id == 0 {
            output.set_caps(frame.caps.as_ref().unwrap());
        }
        let t = [0.5, 0., 0.25, 0., 0.5, 1. / 3., 0., 0., 1.];
        frame.meta.selection = Some(Selection {
            selected_ns: frame.meta.source_ns + 1,
            overdue: true,
            pose: Pose::Fresh {
                t,
                left: Endpoint {
                    frame_id,
                    sequence: frame_id,
                    source_ns: frame.meta.source_ns,
                    observed_ns: frame.meta.source_ns,
                    t,
                },
                frame_warp: None,
            },
        });
        corrupt(frame_id, &mut frame);
        attach(frame.buffer.make_mut(), &frame.meta).unwrap();
        output.push(frame.buffer).unwrap();
    }
    let answers = fixture.until(|fixture, answers| {
        answers.len() as u64 == count || fixture.status().state == WorkerState::Backoff
    });
    let status = fixture.status();
    drop((input, output, fixture));
    (answers, status)
}

#[test]
fn a_well_formed_return_from_any_worker_becomes_a_camera() {
    let (answers, status) = return_frame("good", |_| {});
    assert_eq!(status.state, WorkerState::Ready, "{status:?}");
    assert_eq!(answers.len(), 1);
    let answer = &answers[0];
    assert_eq!(
        (answer.sequence, answer.pose, answer.overdue),
        (3, PoseStatus::Fresh, true)
    );
    let camera = answer.camera.as_ref().unwrap();
    assert_eq!(
        camera.ti.map(|v| v.get().to_bits()),
        [0.5, 0., 0.25, 0., 0.5, 1. / 3., 0., 0., 1.].map(f64::to_bits),
        "row-major, bit for bit"
    );
    assert_eq!(
        (&camera.lens, camera.frame_warp),
        (&None, None),
        "vchao: no lens"
    );
}

const WARP: [f64; 9] = [0.001, 0., -1., 0., 0.001, -0.5, 0., 0., 1.];

fn warped(frame: &mut Received, lens: Option<w::RadialLens>, warp: Option<[f64; 9]>) {
    frame.meta.lens = lens;
    if let Some(Selection {
        pose: Pose::Fresh { frame_warp, .. },
        ..
    }) = &mut frame.meta.selection
    {
        *frame_warp = warp;
    }
}

#[test]
fn a_session_lens_and_frame_warps_reach_the_camera() {
    let (answers, status) = return_frames("lens", 3, |_, frame| {
        warped(frame, Some(radial(-0.05)), Some(WARP))
    });
    assert_eq!(status.state, WorkerState::Ready, "{status:?}");
    assert_eq!(answers.len(), 3);
    for answer in &answers {
        let camera = answer.camera.as_ref().unwrap();
        assert_eq!(camera.lens, Some(radial(-0.05)));
        assert_eq!(
            camera.frame_warp.map(|warp| warp.map(w::Finite::get)),
            Some(WARP)
        );
    }
}

#[test]
fn a_lens_change_within_a_session_is_a_worker_failure() {
    for (name, first, later) in [
        ("lens-change", Some(radial(-0.05)), Some(radial(-0.04))),
        ("lens-appears", None, Some(radial(-0.05))),
        ("lens-vanishes", Some(radial(-0.05)), None),
    ] {
        let (answers, status) = return_frames(name, 2, |frame_id, frame| {
            warped(
                frame,
                if frame_id == 0 {
                    first.clone()
                } else {
                    later.clone()
                },
                None,
            )
        });
        assert_eq!(sequences(&answers), [3], "{name}: the first frame stands");
        assert_eq!(status.state, WorkerState::Backoff, "{name}");
        let error = status.last_error.unwrap();
        assert!(error.contains("changed its lens"), "{name}: {error}");
    }
}

#[test]
fn malformed_returns_are_worker_failures() {
    for (name, corrupt, expected) in [
        (
            "wrong-id",
            Box::new(|frame: &mut Received| {
                frame.meta.frame_id = 7;
                frame.buffer.make_mut().set_offset(7);
            }) as Box<dyn FnOnce(&mut Received)>,
            "frame 7 where frame 0 was due",
        ),
        (
            "no-selection",
            Box::new(|frame: &mut Received| frame.meta.selection = None),
            "without a selection",
        ),
        (
            "other-stream",
            Box::new(|frame: &mut Received| frame.meta.stream_id = "someone-else".into()),
            "of stream \"someone-else\"",
        ),
        (
            "bad-version",
            Box::new(|frame: &mut Received| frame.meta.version = 2),
            "version 2",
        ),
        (
            "bad-duration",
            Box::new(|frame: &mut Received| frame.meta.duration_ns += 1),
            "does not match its buffer",
        ),
        (
            "bad-lens",
            Box::new(|frame: &mut Received| {
                let mut lens = radial(-0.05);
                lens.scale_px = w::Finite::try_from(0.).unwrap();
                warped(frame, Some(lens), None)
            }),
            "invalid lens",
        ),
        (
            "warp-without-lens",
            Box::new(|frame: &mut Received| warped(frame, None, Some(WARP))),
            "a frame warp without a lens",
        ),
        (
            "singular-warp",
            Box::new(|frame: &mut Received| warped(frame, Some(radial(-0.05)), Some([0.; 9]))),
            "invalid frame warp",
        ),
    ] {
        let (answers, status) = return_frame(name, corrupt);
        assert!(answers.is_empty(), "{name}: {answers:?}");
        assert_eq!(status.state, WorkerState::Backoff, "{name}");
        let error = status.last_error.unwrap();
        assert!(error.contains(expected), "{name}: {error}");
    }
}
