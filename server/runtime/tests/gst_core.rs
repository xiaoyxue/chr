//! chr-server's GStreamer core (`--pipeline gst`, #154) end to end without a
//! GPU: an in-process capture (`chr-sdi-in`'s test pattern), the real
//! `chr-tracking-sim-cli` replay, and `chr-renderer-cli worker` answering with
//! solid frames (`CHR_WORKER_TEST_SOLID`). Every slot leaves at its capture
//! time plus the program delay, with CG or its own clean feed; a killed or
//! stalled worker costs CG only, and its restart is relinked into the running
//! pipeline.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use chr_common::methods::{render_worker::WorkerStatus, tracking::TrackerStatus};
use chr_runtime::{
    calibration::lens::CHECKS,
    gst_core::{
        Core, CoreConfig,
        driver::{Driver, Program},
        input::{Input, Source},
        output::Totals,
        select::Counters,
    },
    program::spec::ProgramSpec,
    retained::SceneControl,
    sdi::{SdiStats, VideoMode},
    worker::{
        protocol::{Output, WorkerProgram},
        serve::{DELAY_ENV, SOLID_ENV},
        supervisor::{Supervisor, WorkerConfig, status_handle},
        tracking::supervisor::{Tracker, TrackerConfig},
    },
};
use chr_tracker::{join::TrackingFeed, lens::LensSelection};
use serde_json::json;

const MODE: VideoMode = VideoMode {
    width: 192,
    height: 128,
    fps: 50,
};
const DELAY: Duration = Duration::from_millis(100);

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    /// A render description with one ground quad, the stand-in's manifest, and
    /// a render worker script that answers with solid frames after `delay_ms`.
    fn new(name: &str, delay_ms: u64) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("chr-gst-core-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = f64::from(MODE.width * MODE.height).sqrt();
        let (width, height) = (f64::from(MODE.width), f64::from(MODE.height));
        let radial = json!({"centre_px": [width / 2., height / 2.], "scale_px": width,
            "coefficients": [-0.05, 0, 0], "support": null});
        let ti = json!([[0.12, 0, 0], [0, 0.12, 0], [0, 0, 1]]);
        let warps = json!([[[2. / s, 0, -width / s], [0, 2. / s, -height / s], [0, 0, 1]]]);
        std::fs::write(
            dir.join("description.json"),
            json!({"profile": "chr-render-description-v1",
                "camera": {"width": MODE.width, "height": MODE.height, "fps": MODE.fps,
                    "radial": radial, "reference": {"ti": ti, "center": [0, 0, -8], "frame_warps": warps}},
                "scene": {"ground": {"pitch_m": [20, 12]}, "assets": [], "instances": []}})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            json!({"ti": ti, "center": [0, 0, -8], "radial": radial, "frame_warps": warps})
                .to_string(),
        )
        .unwrap();
        let worker = dir.join("render-worker.sh");
        std::fs::write(
            &worker,
            format!(
                "#!/bin/sh\n{SOLID_ENV}=200 {DELAY_ENV}={delay_ms} exec {} \"$@\"\n",
                env!("CARGO_BIN_EXE_chr-renderer-cli")
            ),
        )
        .unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Status of both workers, for their PIDs.
struct Workers {
    render: Arc<std::sync::Mutex<WorkerStatus>>,
    tracking: Arc<std::sync::Mutex<TrackerStatus>>,
}

impl Workers {
    fn render_pid(&self) -> Option<u32> {
        self.render.lock().unwrap().pid
    }

    fn tracking_pid(&self) -> Option<u32> {
        self.tracking.lock().unwrap().pid
    }
}

/// Runs the core on `fixture` while `scenario` runs, then returns what it
/// returns. The capture runs in this process.
async fn with_core<T>(fixture: &Fixture, scenario: impl AsyncFnOnce(&Core, &Workers) -> T) -> T {
    let input = Input::start(&Source::TestPattern(MODE), &fixture.path("capture.sock"), 4).unwrap();
    let installed = ProgramSpec::Description {
        path: fixture.path("description.json"),
    }
    .install(MODE, chr_runtime::program::synthetic::runtime_id().unwrap())
    .unwrap();
    let (synthetic, adapter) = (installed.program, installed.adapter);
    let (_, recorder) = SdiStats::recorded(MODE);
    let (core, frames) = Core::start(CoreConfig {
        capture: MODE,
        output: MODE,
        capture_socket: fixture.path("capture.sock"),
        program_delay: DELAY,
        tap: Some(fixture.path("program.tap")),
        recorder,
    })
    .unwrap();
    let workers = Workers {
        render: status_handle(),
        tracking: chr_runtime::worker::tracking::supervisor::status_handle(),
    };
    let tracker = Tracker::new(
        TrackerConfig {
            executable: env!("CARGO_BIN_EXE_chr-tracking-sim-cli").into(),
            args: vec![
                "--manifest".into(),
                fixture.path("manifest.json").display().to_string(),
            ],
            width: MODE.width,
            height: MODE.height,
            fps: MODE.fps,
            camera_center: [0., 0., -8.],
            socket_dir: fixture.path("tracking"),
            hang: Duration::from_millis(500),
            in_flight: 16,
            host: core.host(),
        },
        Arc::clone(&workers.tracking),
    )
    .unwrap();
    let render = Supervisor::new(
        WorkerConfig {
            executable: fixture.path("render-worker.sh"),
            program: WorkerProgram::Description {
                path: fixture.path("description.json"),
            },
            runtime_id: synthetic.configuration.runtime_id.clone(),
            output: Output {
                width: MODE.width,
                height: MODE.height,
                fps: MODE.fps,
            },
            socket_dir: fixture.path("render"),
            hang: Duration::from_millis(500),
            assets: Default::default(),
            host: core.host(),
        },
        Arc::clone(&workers.render),
    )
    .unwrap();
    let (control, owner) = SceneControl::spawn(adapter, 8).unwrap();
    synthetic.create_scene(&control).await.unwrap();
    let program = Program::new(&synthetic, control.clone()).unwrap();
    let driver = Driver::new(
        &core,
        tracker,
        render,
        TrackingFeed::new(64, CHECKS),
        LensSelection::auto(None),
        program,
        16,
    );
    let result = tokio::select! {
        result = driver.run(frames) => panic!("the driver stopped: {result:?}"),
        result = scenario(&core, &workers) => result,
    };
    control.shutdown().await;
    drop(control);
    owner.await.unwrap();
    drop((core, input));
    result
}

/// Waits until `done` holds, at most `limit`.
async fn until(limit: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The output and selector counts accumulated over `period`.
async fn over(core: &Core, period: Duration) -> (Totals, Counters) {
    let (output, selector) = (core.output_totals(), core.selector_counters());
    tokio::time::sleep(period).await;
    let (after, decided) = (core.output_totals(), core.selector_counters());
    let mut totals = after;
    (totals.slots, totals.cg, totals.clean, totals.repeats) = (
        after.slots - output.slots,
        after.cg - output.cg,
        after.clean - output.clean,
        after.repeats - output.repeats,
    );
    let mut counters = decided;
    (
        counters.cg,
        counters.clean,
        counters.empty,
        counters.late_cg,
    ) = (
        decided.cg - selector.cg,
        decided.clean - selector.clean,
        decided.empty - selector.empty,
        decided.late_cg - selector.late_cg,
    );
    (totals, counters)
}

fn kill(signal: &str, pid: u32) {
    let status = std::process::Command::new("kill")
        .args(["-s", signal, &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

/// (slot - frame) of the tap's slots read over `period`.
fn tap_deltas(path: &Path, period: Duration) -> Vec<i64> {
    let tap = chr_shm::Reader::open(path).unwrap();
    let (mut seen, mut deltas, mut payload) = (tap.write_count(), Vec::new(), Vec::new());
    let deadline = Instant::now() + period;
    while Instant::now() < deadline {
        if tap.write_count() != seen {
            seen = tap.write_count();
            if let Ok(chr_shm::Read::Frame(header)) =
                tap.read(tap.latest_slot().unwrap(), &mut payload)
            {
                let meta = chr_shm::tap::SlotMeta::from_meta(header.meta);
                deltas.push(header.index as i64 - meta.source_frame as i64);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    deltas
}

#[tokio::test(flavor = "multi_thread")]
async fn every_slot_leaves_at_the_program_delay_with_its_cg() {
    let fixture = Fixture::new("steady", 0);
    let tap = fixture.path("program.tap");
    with_core(&fixture, async |core, _| {
        until(Duration::from_secs(10), || core.selector_counters().cg > 10).await;
        let (totals, counters) = over(core, Duration::from_secs(2)).await;
        assert!((95..=105).contains(&totals.slots), "{totals:?}");
        assert_eq!(
            (totals.cg, totals.clean, totals.repeats),
            (totals.slots, 0, 0),
            "{counters:?}"
        );
        // Every frame, CG or clean, leaves the program delay after its capture.
        let delay = core.output_totals().delay;
        let ms = |ns: i64| ns as f64 / 1e6;
        assert!(
            ms(delay.min) >= 99.5 && ms(delay.max) < 110.,
            "delay {:?}..{:?} ms",
            ms(delay.min),
            ms(delay.max)
        );
        let interval = totals.interval;
        assert!(
            (ms(interval.mean() as i64) - 20.).abs() < 0.5,
            "{interval:?}"
        );
        let deltas = tokio::task::block_in_place(|| tap_deltas(&tap, Duration::from_secs(1)));
        assert!(deltas.len() > 30);
        assert!(
            deltas.iter().all(|delta| *delta == deltas[0]),
            "the tap's slot - frame is constant: {deltas:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_render_slower_than_the_delay_gives_every_slot_its_clean_feed() {
    let fixture = Fixture::new("slow", 150);
    with_core(&fixture, async |core, workers| {
        until(Duration::from_secs(10), || {
            workers.render_pid().is_some() && core.selector_counters().late_cg > 5
        })
        .await;
        let (totals, counters) = over(core, Duration::from_secs(1)).await;
        assert_eq!(
            (totals.cg, totals.clean, totals.repeats),
            (0, totals.slots, 0),
            "{counters:?}"
        );
        assert!(
            counters.late_cg > 0,
            "late CG frames are dropped: {counters:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_or_stopped_worker_gives_clean_feed_and_cg_resumes_after_relinking() {
    let fixture = Fixture::new("faults", 0);
    with_core(&fixture, async |core, workers| {
        until(Duration::from_secs(10), || core.selector_counters().cg > 10).await;
        let before = core.output_totals();
        for (worker, signal) in [("render", "KILL"), ("tracking", "KILL"), ("render", "STOP")] {
            let pid = match worker {
                "render" => workers.render_pid(),
                _ => workers.tracking_pid(),
            }
            .unwrap();
            let clean = core.selector_counters().clean;
            kill(signal, pid);
            until(Duration::from_secs(5), || {
                core.selector_counters().clean > clean + 5
            })
            .await;
            // Replaced (a stopped worker once it hangs) and relinked: CG again.
            until(Duration::from_secs(10), || {
                let replaced = match worker {
                    "render" => workers.render_pid(),
                    _ => workers.tracking_pid(),
                };
                replaced.is_some_and(|new| new != pid)
            })
            .await;
            if signal == "STOP" {
                let _ = std::process::Command::new("kill")
                    .args(["-s", "KILL", &pid.to_string()])
                    .status();
            }
            let cg = core.selector_counters().cg;
            until(Duration::from_secs(10), || {
                core.selector_counters().cg > cg + 10
            })
            .await;
        }
        let after = core.output_totals();
        assert_eq!(
            after.repeats - before.repeats,
            0,
            "the output never froze: {}",
            after.report()
        );
        let delay = after.delay;
        assert!(
            delay.max - delay.min < 10_000_000,
            "the delay stayed constant: {}",
            after.report()
        );
    })
    .await;
}
