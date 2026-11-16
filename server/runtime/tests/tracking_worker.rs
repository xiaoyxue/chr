//! The supervisor driving the real `chr-tracking-sim-cli` worker.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use chr_common::methods::{render_worker::WorkerState, tracking::TrackingFrame};
use chr_runtime::{
    calibration::tracking::TrackingReplay,
    worker::{
        tracking::supervisor::{StatusHandle, Tracker, TrackerConfig, status_handle},
        transport::SharedPixels,
    },
};

const WIDTH: u32 = 4;
const HEIGHT: u32 = 2;
const TI: [[f64; 3]; 3] = [[0.02, 0., 0.], [0., 0.02, 0.], [0., 0., 1.]];
const WARPS: [[[f64; 3]; 3]; 2] = [
    [[0.001, 0., -1.], [0., 0.001, -0.5], [0., 0., 1.]],
    [[0.001, 0., -0.9], [0., 0.001, -0.5], [0., 0., 1.]],
];

struct Fixture {
    dir: PathBuf,
    status: StatusHandle,
    tracker: Option<Tracker>,
}

impl Fixture {
    fn new(name: &str, args: &[&str], hang: Duration) -> Self {
        let dir = std::env::temp_dir().join(format!("chr-tracking-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let calibration = dir.join("scene.json");
        std::fs::write(
            &calibration,
            serde_json::json!({"ti": TI, "center": [0., 0., -10.], "frame_warps": WARPS})
                .to_string(),
        )
        .unwrap();
        let status = status_handle();
        let tracker = Tracker::new(
            TrackerConfig {
                executable: env!("CARGO_BIN_EXE_chr-tracking-sim-cli").into(),
                args: args.iter().map(|arg| (*arg).to_owned()).collect(),
                calibration,
                width: WIDTH,
                height: HEIGHT,
                fps: 50,
                socket_dir: dir.join("sockets"),
                hang,
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

    /// Polls until `done` holds, collecting cameras.
    fn until(&mut self, done: impl Fn(&Self, &[TrackingFrame]) -> bool) -> Vec<TrackingFrame> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut cameras = Vec::new();
        while !done(self, &cameras) {
            assert!(Instant::now() < deadline, "timed out: {:?}", self.status());
            cameras.extend(self.tracker().poll());
            std::thread::sleep(Duration::from_millis(2));
        }
        cameras
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

#[test]
fn every_sent_frame_gets_the_recorded_camera() {
    let mut fixture = Fixture::new("replay", &[], Duration::from_secs(2));
    fixture.tracker().track(0, &Fixture::frame(0));
    assert_eq!(fixture.status().not_sent, 1, "no worker is ready yet");
    fixture.ready();
    for sequence in 10..14 {
        fixture.tracker().track(sequence, &Fixture::frame(sequence));
    }
    let cameras = fixture.until(|_, cameras| cameras.len() == 4);
    let replay = TrackingReplay::new(TI, [0., 0., -10.], &WARPS).unwrap();
    for camera in &cameras {
        assert_eq!(*camera, replay.frame(camera.sequence.get()));
    }
    let status = fixture.status();
    assert_eq!((status.requests, status.cameras), (4, 4));
    assert_eq!(status.state, WorkerState::Ready);
}

#[test]
fn unavailable_frames_are_counted_and_a_crashed_worker_comes_back() {
    let mut fixture = Fixture::new(
        "crash",
        &["--unavailable-every", "2", "--exit-after", "4"],
        Duration::from_secs(2),
    );
    fixture.ready();
    for sequence in 0..4 {
        fixture.tracker().track(sequence, &Fixture::frame(sequence));
    }
    // Frames 0 and 2 get cameras, 1 is unavailable, and the 4th request crashes the worker.
    let cameras = fixture.until(|fixture, _| {
        fixture.status().restarts == 0 && fixture.status().state == WorkerState::Backoff
    });
    let status = fixture.status();
    assert_eq!((cameras.len(), status.unavailable), (2, 1), "{status:?}");
    assert!(status.last_error.unwrap().contains("exited"));
    fixture.ready();
    assert_eq!(fixture.status().restarts, 1);
    fixture.tracker().track(8, &Fixture::frame(8));
    let cameras = fixture.until(|_, cameras| !cameras.is_empty());
    assert_eq!(cameras[0].sequence.get(), 8, "the new worker answers");
}

#[test]
fn a_hung_worker_is_replaced_and_its_late_frames_forgotten() {
    let mut fixture = Fixture::new("hang", &["--hang-after", "2"], Duration::from_millis(150));
    fixture.ready();
    fixture.tracker().track(0, &Fixture::frame(0));
    fixture.tracker().track(1, &Fixture::frame(1));
    let cameras = fixture.until(|fixture, _| fixture.status().state == WorkerState::Backoff);
    assert_eq!(cameras.len(), 1, "frame 1 never got its camera");
    assert!(
        fixture.status().last_error.unwrap().contains("unanswered"),
        "{:?}",
        fixture.status()
    );
    fixture.ready();
    assert_eq!(fixture.status().restarts, 1);
}

#[test]
fn forgotten_frames_are_never_answered() {
    let mut fixture = Fixture::new("forget", &["--delay-ms", "50"], Duration::from_secs(2));
    fixture.ready();
    fixture.tracker().track(5, &Fixture::frame(5));
    fixture.tracker().forget();
    fixture.tracker().track(6, &Fixture::frame(6));
    let cameras = fixture.until(|_, cameras| !cameras.is_empty());
    std::thread::sleep(Duration::from_millis(100));
    let late: Vec<_> = fixture.tracker().poll();
    let sequences: Vec<_> = cameras
        .iter()
        .chain(&late)
        .map(|c| c.sequence.get())
        .collect();
    assert_eq!(sequences, [6], "frame 5 was forgotten after it was sent");
}

#[test]
fn an_unreadable_calibration_is_fatal_and_backs_off() {
    let mut fixture = Fixture::new("fatal", &[], Duration::from_secs(2));
    std::fs::write(fixture.dir.join("scene.json"), "{}").unwrap();
    fixture.until(|fixture, _| fixture.status().last_error.is_some());
    let status = fixture.status();
    assert_eq!(status.state, WorkerState::Backoff);
    assert!(
        status.last_error.as_ref().unwrap().contains("failed"),
        "{status:?}"
    );
    assert!(!fixture.tracker().is_ready());
}
