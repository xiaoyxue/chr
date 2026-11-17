//! chr-server's GStreamer core (`--pipeline gst`, #154) end to end without a
//! GPU: an in-process capture (`chr-sdi-in`'s test pattern), the real
//! `chr-tracking-sim-cli` replay, and `chr-renderer-cli worker` answering with
//! solid frames (`CHR_WORKER_TEST_SOLID`). Every slot leaves at its capture
//! time plus the program delay, with CG or its own clean feed; a killed or
//! stalled worker costs CG only, and its restart is relinked into the running
//! pipeline. A change of the input's rate is followed (or converted to a
//! fixed output rate) without a restart, with a new tracking stream.
//! Interlaced input keeps its fields on the output and is deinterlaced for
//! tracking and rendering; a lost input is filled on the cadence; audio,
//! timecode and captions leave with their frame's delay. The tests that play a
//! clip need FFmpeg (`nix develop .#media`) and are skipped without it.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chr_common::{
    methods::{render_worker::WorkerStatus, tracking::TrackerStatus},
    rate::Rate,
};
use chr_runtime::{
    calibration::lens::CHECKS,
    gst_core::{
        Core, CoreConfig, InputLoss, OutputRate, OutputSink,
        driver::{Driver, Program},
        input::{Input, Options, Source},
        output::Totals,
        select::Counters,
    },
    program::spec::ProgramSpec,
    retained::SceneControl,
    sdi::{FieldOrder, Format, Scan, SdiStats, VideoMode},
    worker::{
        protocol::{Output, WorkerProgram},
        serve::{DELAY_ENV, SOLID_ENV},
        supervisor::{Supervisor, WorkerConfig, status_handle},
        tracking::supervisor::{Tracker, TrackerConfig, in_flight_limit},
        transport::Host,
    },
};
use chr_tracker::{join::TrackingFeed, lens::LensSelection};
use gstreamer::{self as gst, prelude::*};
use serde_json::json;

const MODE: VideoMode = VideoMode {
    width: 192,
    height: 128,
    fps: chr_common::rate::Rate::fps(50),
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

    fn tracking_stream(&self) -> Option<String> {
        self.tracking.lock().unwrap().stream_id.clone()
    }
}

/// What a scenario sees: the core, its workers, its capture and its status.
struct Run<'a> {
    core: &'a Core,
    workers: &'a Workers,
    input: std::cell::RefCell<Input>,
    sdi: SdiStats,
}

/// What a run plays and how its output behaves.
struct Setup {
    source: Source,
    options: Options,
    policy: OutputRate,
    input_loss: InputLoss,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            source: Source::TestPattern(MODE.into()),
            options: Options::default(),
            policy: OutputRate::Follow,
            input_loss: InputLoss::Hold,
        }
    }
}

/// [`with_setup`] of the test pattern with the output at `policy`.
async fn with_core_at<T>(
    fixture: &Fixture,
    policy: OutputRate,
    scenario: impl AsyncFnOnce(&Run<'_>) -> T,
) -> T {
    with_setup(
        fixture,
        Setup {
            policy,
            ..Setup::default()
        },
        scenario,
    )
    .await
}

/// Runs the core on `fixture` as `setup` says while `scenario` runs, then
/// returns what it returns. The capture runs in this process.
async fn with_setup<T>(
    fixture: &Fixture,
    setup: Setup,
    scenario: impl AsyncFnOnce(&Run<'_>) -> T,
) -> T {
    let policy = setup.policy;
    let input = Input::start(&setup.source, &fixture.path("capture.sock"), &setup.options).unwrap();
    let installed = ProgramSpec::Description {
        path: fixture.path("description.json"),
    }
    .install(MODE, chr_runtime::program::synthetic::runtime_id().unwrap())
    .unwrap();
    let (synthetic, adapter) = (installed.program, installed.adapter);
    let output = match policy {
        OutputRate::Follow => MODE,
        OutputRate::Fixed(fps) => VideoMode { fps, ..MODE },
    };
    let (sdi, recorder) = SdiStats::recorded(output);
    let (core, frames) = Core::start(CoreConfig {
        capture: MODE,
        output,
        output_rate: policy,
        capture_socket: fixture.path("capture.sock"),
        program_delay: DELAY,
        tap: Some(fixture.path("program.tap")),
        recorder,
        sink: OutputSink::Simulated,
        input_loss: setup.input_loss,
        audio: setup.options.audio,
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
            in_flight: in_flight_limit(DELAY, MODE.fps),
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
    let run = Run {
        core: &core,
        workers: &workers,
        input: std::cell::RefCell::new(input),
        sdi,
    };
    let result = tokio::select! {
        result = driver.run(frames) => panic!("the driver stopped: {result:?}"),
        result = scenario(&run) => result,
    };
    control.shutdown().await;
    drop(control);
    owner.await.unwrap();
    let input = run.input;
    drop((core, input));
    result
}

/// [`with_core_at`] with the output following the input's rate.
async fn with_core<T>(fixture: &Fixture, scenario: impl AsyncFnOnce(&Core, &Workers) -> T) -> T {
    with_core_at(fixture, OutputRate::Follow, async move |run| {
        scenario(run.core, run.workers).await
    })
    .await
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
    let mut totals = after.clone();
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
        let interval = *totals.interval(MODE.fps).unwrap();
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

/// The core's statistics once `count` more CG slots came after a change of
/// the input to `mode`: the transition is over.
async fn switch(run: &Run<'_>, mode: VideoMode) {
    let transitions = run.core.selector_counters().transitions;
    run.input.borrow_mut().switch(mode.into()).unwrap();
    until(Duration::from_secs(5), || {
        run.core.selector_counters().transitions > transitions
    })
    .await;
    let cg = run.core.selector_counters().cg;
    until(Duration::from_secs(10), || {
        run.core.selector_counters().cg > cg + 25
    })
    .await;
}

fn assert_mean_period(totals: &Totals, rate: Rate) {
    let interval = totals.interval(rate).expect("slots at the rate");
    let period = rate.frame_ns(1) as f64;
    assert!(
        interval.count > 20 && (interval.mean() - period).abs() < period * 0.02,
        "at {rate} fps: {}",
        totals.report()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_output_follows_a_change_of_rate_with_a_new_tracking_stream() {
    let fixture = Fixture::new("follow", 0);
    with_core_at(&fixture, OutputRate::Follow, async |run| {
        let core = run.core;
        until(Duration::from_secs(10), || core.selector_counters().cg > 25).await;
        let stream = run.workers.tracking_stream().unwrap();
        let ntsc = Rate::new(30000, 1001).unwrap();
        for fps in [Rate::fps(25), ntsc] {
            switch(run, VideoMode { fps, ..MODE }).await;
            tokio::time::sleep(Duration::from_millis(600)).await;
            let status = run.sdi.status();
            assert_eq!(status.rate, fps, "the output follows: {status:?}");
        }
        let totals = core.output_totals();
        for fps in [MODE.fps, Rate::fps(25), ntsc] {
            assert_mean_period(&totals, fps);
        }
        assert_eq!(totals.repeats, 0, "{}", totals.report());
        assert!(
            totals.delay.max - totals.delay.min < 10_000_000,
            "D stays constant: {}",
            totals.report()
        );
        let counters = core.selector_counters();
        assert_eq!(counters.transitions, 2, "{counters:?}");
        let status = run.sdi.status();
        assert_eq!(
            (status.format_changes, status.transition_clean_slots),
            (2, counters.transition_clean)
        );
        assert!(
            run.workers
                .tracking_stream()
                .is_some_and(|now| now != stream),
            "a new format is a new tracking stream"
        );
        assert_eq!(
            run.workers.tracking.lock().unwrap().restarts,
            0,
            "a new stream is no restart"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fixed_output_rate_converts_every_input_rate() {
    let fixture = Fixture::new("fixed", 0);
    let fixed = Rate::fps(50);
    with_core_at(&fixture, OutputRate::Fixed(fixed), async |run| {
        let core = run.core;
        until(Duration::from_secs(10), || core.selector_counters().cg > 25).await;
        for fps in [Rate::fps(25), Rate::new(30000, 1001).unwrap()] {
            switch(run, VideoMode { fps, ..MODE }).await;
            tokio::time::sleep(Duration::from_millis(600)).await;
        }
        let totals = core.output_totals();
        assert_eq!(
            totals.intervals.len(),
            1,
            "one output rate: {}",
            totals.report()
        );
        assert_mean_period(&totals, fixed);
        assert_eq!(totals.irregular, 0, "{}", totals.report());
        assert!(
            totals.duplicates > 20,
            "slower input repeats: {}",
            totals.report()
        );
        assert_eq!(run.sdi.status().rate, fixed);
        let counters = core.selector_counters();
        assert_eq!(counters.transitions, 2, "{counters:?}");
        assert!(counters.rate_repeats > 20, "{counters:?}");
    })
    .await;
}

/// The core's pipeline, to watch its sinks.
fn pipeline(core: &Core) -> gst::Pipeline {
    match core.host() {
        Host::Bin(pipeline) => pipeline,
        Host::Standalone => unreachable!("the core hosts its workers' sockets"),
    }
}

/// An output slot as it reached the output sink.
#[derive(Clone, Debug)]
struct Seen {
    /// Its due time, ns of running time.
    pts: u64,
    interlace: String,
    field_order: Option<String>,
    cg: bool,
    fill: bool,
    timecode: Option<String>,
    captions: usize,
    /// Mean luma of the odd rows minus the even rows in columns 0-63.
    parity: f64,
    /// Mean luma of columns 64-127.
    flash: f64,
    /// The first pixel's luma.
    luma: u8,
}

/// Records every slot the output sink gets.
fn watch_output(core: &Core) -> Arc<Mutex<Vec<Seen>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let sink = pipeline(core).by_name("output").unwrap();
    sink.static_pad("sink")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            let (Some(buffer), Some(caps)) = (info.buffer(), pad.current_caps()) else {
                return gst::PadProbeReturn::Ok;
            };
            let structure = caps.structure(0).unwrap();
            let width = structure.get::<i32>("width").unwrap() as usize;
            let height = structure.get::<i32>("height").unwrap() as usize;
            let meta =
                gst::meta::CustomMeta::from_buffer(buffer, chr_runtime::gst_core::PROGRAM_META)
                    .unwrap();
            let map = buffer.map_readable().unwrap();
            let luma = |x: usize, y: usize| f64::from(map[(y * width + x) * 2 + 1]);
            let mean = |xs: std::ops::Range<usize>, rows: &mut dyn Iterator<Item = usize>| {
                let (mut sum, mut n) = (0.0, 0.0);
                for y in rows {
                    for x in xs.clone() {
                        sum += luma(x, y);
                        n += 1.0;
                    }
                }
                sum / n
            };
            let parity = mean(0..64.min(width), &mut (1..height).step_by(2))
                - mean(0..64.min(width), &mut (0..height).step_by(2));
            let flash = mean(64..128.min(width), &mut (0..height));
            record.lock().unwrap().push(Seen {
                pts: buffer.pts().unwrap().nseconds(),
                interlace: structure.get::<String>("interlace-mode").unwrap(),
                field_order: structure.get::<String>("field-order").ok(),
                cg: meta.structure().get("cg").unwrap(),
                fill: meta.structure().get("fill").unwrap(),
                timecode: buffer
                    .meta::<gstreamer_video::VideoTimeCodeMeta>()
                    .map(|meta| meta.tc().to_string()),
                captions: buffer
                    .iter_meta::<gstreamer_video::VideoCaptionMeta>()
                    .count(),
                parity,
                flash,
                luma: map[1],
            });
            gst::PadProbeReturn::Ok
        });
    seen
}

/// What the audio sink got: buffers, and the times (ns of running time) at
/// which a beep starts after at least 100 ms of silence.
#[derive(Debug, Default)]
struct Heard {
    buffers: u64,
    onsets: Vec<u64>,
    loud_until: Option<u64>,
}

fn watch_audio(core: &Core) -> Arc<Mutex<Heard>> {
    let heard = Arc::new(Mutex::new(Heard::default()));
    let record = Arc::clone(&heard);
    let sink = pipeline(core).by_name("audio-output").unwrap();
    sink.static_pad("sink")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            let (Some(buffer), Some(caps)) = (info.buffer(), pad.current_caps()) else {
                return gst::PadProbeReturn::Ok;
            };
            let structure = caps.structure(0).unwrap();
            let rate = structure.get::<i32>("rate").unwrap() as u64;
            let channels = structure.get::<i32>("channels").unwrap() as usize;
            let start = buffer.pts().unwrap().nseconds();
            let map = buffer.map_readable().unwrap();
            let mut heard = record.lock().unwrap();
            heard.buffers += 1;
            for (n, frame) in map.chunks_exact(4 * channels).enumerate() {
                let sample = i32::from_le_bytes(frame[..4].try_into().unwrap());
                if sample.unsigned_abs() > i32::MAX as u32 / 10 {
                    let at = start + n as u64 * 1_000_000_000 / rate;
                    if heard
                        .loud_until
                        .is_none_or(|until| at > until + 100_000_000)
                    {
                        heard.onsets.push(at);
                    }
                    heard.loud_until = Some(at);
                }
            }
            gst::PadProbeReturn::Ok
        });
    heard
}

/// The audio status after a report: lip sync is within a frame.
fn assert_lip_sync(core: &Core, run: &Run<'_>, period_ns: u64) {
    core.report();
    let audio = run.sdi.status().audio.expect("audio passes through");
    assert!(audio.samples_output > 0, "{audio:?}");
    assert!(
        audio.offset_us.unsigned_abs() * 1000 < period_ns,
        "audio - video {} µs: {audio:?}",
        audio.offset_us
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interlaced_input_is_rendered_progressive_and_leaves_interlaced() {
    let fixture = Fixture::new("interlaced", 0);
    let i50 = Format {
        mode: VideoMode {
            fps: Rate::fps(25),
            ..MODE
        },
        scan: Scan::Interlaced(FieldOrder::TopFirst),
    };
    let setup = Setup {
        source: Source::TestPattern(i50),
        options: Options {
            captions: true,
            ..Options::default()
        },
        ..Setup::default()
    };
    with_setup(&fixture, setup, async |run| {
        let core = run.core;
        let seen = watch_output(core);
        until(Duration::from_secs(10), || core.selector_counters().cg > 10).await;
        // Deinterlaced frames reached both workers through memfds: CG.
        let (totals, counters) = over(core, Duration::from_secs(2)).await;
        assert!((45..=55).contains(&totals.slots), "{}", totals.report());
        assert_eq!(
            (totals.cg, totals.repeats),
            (totals.slots, 0),
            "{counters:?}"
        );
        assert_mean_period(&core.output_totals(), Rate::fps(25));
        let status = run.sdi.status();
        assert_eq!(status.video_mode, "192x128i50");
        let seen = seen.lock().unwrap().clone();
        let odd: Vec<_> = seen
            .iter()
            .filter(|slot| {
                !(slot.interlace == "interleaved"
                    && slot.field_order.as_deref() == Some("top-field-first"))
            })
            .collect();
        assert!(
            odd.is_empty(),
            "{} of {}: {:?}",
            odd.len(),
            seen.len(),
            &odd[..odd.len().min(3)]
        );
        // Each frame's timecode and captions leave with it.
        let totals = core.output_totals();
        assert_eq!(totals.timecode_breaks, 0, "{}", totals.report());
        assert!(
            seen.iter()
                .filter(|slot| slot.cg)
                .all(|slot| slot.timecode.is_some() && slot.captions == 1)
        );
        assert!(
            status.last_timecode.is_some() && status.caption_frames > 0,
            "{status:?}"
        );
        assert_lip_sync(core, run, Rate::fps(25).period_ns());
        let delay = totals.delay;
        assert!(delay.max - delay.min < 10_000_000, "{}", totals.report());
    })
    .await;
}

/// Runs the core with `loss`, loses the input for a second and checks the
/// fill and the recovery; returns the fill slots' first luma.
async fn lose_the_input(name: &str, loss: InputLoss) -> Vec<u8> {
    let fixture = Fixture::new(name, 0);
    let setup = Setup {
        input_loss: loss,
        ..Setup::default()
    };
    with_setup(&fixture, setup, async |run| {
        let core = run.core;
        let seen = watch_output(core);
        let heard = watch_audio(core);
        until(Duration::from_secs(10), || core.selector_counters().cg > 25).await;
        let before = core.output_totals();
        run.input.borrow().set_lost(true);
        tokio::time::sleep(Duration::from_secs(1)).await;
        let lost = core.selector_counters();
        run.input.borrow().set_lost(false);
        let cg = core.selector_counters().cg;
        until(Duration::from_secs(5), || {
            core.selector_counters().cg > cg + 25
        })
        .await;
        let after = core.output_totals();
        let counters = core.selector_counters();
        assert_eq!(counters.losses, 1, "{counters:?}");
        assert!(
            (35..=55).contains(&counters.loss_slots),
            "a second at 50 fps, less the program delay: {counters:?} ({lost:?} then)"
        );
        let status = run.sdi.status();
        assert_eq!(
            (status.input_losses, status.input_loss_slots),
            (1, counters.loss_slots)
        );
        // The cadence went on through the loss and the recovery, and every
        // frame still left the program delay after its capture.
        assert_eq!(after.repeats - before.repeats, 0, "{}", after.report());
        assert_eq!(after.irregular, 0, "{}", after.report());
        assert_eq!(after.fills, counters.loss_slots, "{}", after.report());
        assert!(
            after.delay.max - after.delay.min < 10_000_000,
            "D unchanged: {}",
            after.report()
        );
        assert!(heard.lock().unwrap().buffers > 0, "audio came back");
        let seen = seen.lock().unwrap();
        seen.iter()
            .filter(|slot| slot.fill)
            .map(|slot| slot.luma)
            .collect()
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_input_holds_the_last_frame_on_the_cadence() {
    let lumas = lose_the_input("hold", InputLoss::Hold).await;
    assert!(
        lumas.iter().all(|luma| *luma == 200),
        "the last CG frame: {lumas:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_input_goes_black_on_the_cadence() {
    let lumas = lose_the_input("black", InputLoss::Black).await;
    assert!(lumas.iter().all(|luma| *luma == 16), "{lumas:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_input_shows_the_slate_on_the_cadence() {
    let lumas = lose_the_input("slate", InputLoss::Slate(None)).await;
    // Colour bars start with white.
    assert!(lumas.iter().all(|luma| *luma == 235), "{lumas:?}");
}

fn ffmpeg() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// A 4-period clip at `MODE`'s extent: a beep and a flash (columns 64-127)
/// together at the start of each period of `period` seconds, and in columns
/// 0-63 a luma that counts pictures, so an interlaced frame's bottom field
/// (from the later picture) is 4 brighter than its top field.
fn clip(dir: &Path, name: &str, pictures: &str, period: &str, interlaced: bool) -> PathBuf {
    let path = dir.join(format!("{name}.mov"));
    let per_period = pictures
        .parse::<Rate>()
        .unwrap()
        .frames_in(Duration::from_secs_f64(period.parse::<f64>().unwrap()));
    let flash = if interlaced { 2 } else { 1 };
    let mut video = format!(
        "color=c=gray:s={}x{}:r={pictures}:d={},format=yuv420p,\
         geq=lum='if(lt(X,64),40+4*mod(N,40),if(lt(mod(N,{per_period}),{flash})*lt(X,128),235,128))'\
         :cb=128:cr=128",
        MODE.width,
        MODE.height,
        4.0 * period.parse::<f64>().unwrap()
    );
    if interlaced {
        video.push_str(",tinterlace=interleave_top,setfield=tff");
    }
    let audio = format!(
        "aevalsrc='if(lt(mod(t,{period}),0.02),0.5*sin(2*PI*1000*t),0)':s=48000:c=stereo:d={}",
        4.0 * period.parse::<f64>().unwrap()
    );
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(&video)
        .args(["-f", "lavfi", "-i"])
        .arg(&audio)
        .args(["-c:v", "libx264", "-qp", "0", "-pix_fmt", "yuv420p"])
        .args(if interlaced {
            &["-flags", "+ilme+ildct", "-x264-params", "tff=1"][..]
        } else {
            &[][..]
        })
        .args(["-timecode", "10:00:00:00", "-c:a", "pcm_s16le"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    path
}

/// Plays `clip` with every slot clean (the render worker answers too late)
/// and returns what the output and audio sinks got over three periods.
async fn play(name: &str, clip: impl Fn(&Path) -> PathBuf) -> (Vec<Seen>, Heard, Played) {
    let fixture = Fixture::new(name, 150);
    let path = clip(&fixture.dir);
    let setup = Setup {
        source: Source::File(path),
        ..Setup::default()
    };
    with_setup(&fixture, setup, async |run| {
        let core = run.core;
        let seen = watch_output(core);
        let heard = watch_audio(core);
        until(Duration::from_secs(10), || core.output_totals().slots > 25).await;
        tokio::time::sleep(Duration::from_millis(3500)).await;
        eprintln!("{name}: {}", core.report());
        let totals = core.output_totals();
        let summary = Played {
            status: run.sdi.status(),
            totals,
        };
        let seen = seen.lock().unwrap().clone();
        let heard = std::mem::take(&mut *heard.lock().unwrap());
        (seen, heard, summary)
    })
    .await
}

struct Played {
    status: chr_common::methods::sdi::SdiStatus,
    totals: Totals,
}

/// The start of each flash on the output paired with the nearest beep:
/// audio minus video, ns.
fn lip_sync(seen: &[Seen], heard: &Heard) -> Vec<i64> {
    let flashes: Vec<u64> = seen
        .windows(2)
        .filter(|pair| pair[1].flash > 200. && pair[0].flash < 200.)
        .map(|pair| pair[1].pts)
        .collect();
    flashes
        .iter()
        .filter_map(|flash| {
            heard
                .onsets
                .iter()
                .map(|onset| *onset as i64 - *flash as i64)
                .min_by_key(|offset| offset.unsigned_abs())
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn interlaced_fields_and_lip_sync_survive_the_core() {
    if !ffmpeg() {
        eprintln!("skipped: no FFmpeg (nix develop .#media)");
        return;
    }
    for (name, pictures, period, interlaced, fps) in [
        ("av-p50", "50", "1", false, Rate::fps(50)),
        ("av-i50", "50", "1", true, Rate::fps(25)),
        (
            "av-p5994",
            "60000/1001",
            "1.001",
            false,
            Rate::new(60000, 1001).unwrap(),
        ),
        (
            "av-i5994",
            "60000/1001",
            "1.001",
            true,
            Rate::new(30000, 1001).unwrap(),
        ),
    ] {
        let (seen, heard, run) =
            play(name, |dir| clip(dir, name, pictures, period, interlaced)).await;
        let report = run.totals.report();
        assert!(
            run.totals.slots > 50 && run.totals.cg == 0,
            "{name}: {report}"
        );
        assert_eq!(run.totals.repeats, 0, "{name}: {report}");
        assert_eq!(run.totals.timecode_breaks, 0, "{name}: {report}");
        assert!(
            run.status
                .last_timecode
                .as_deref()
                .is_some_and(|tc| tc.starts_with("10:00:0")),
            "{name}: the file's timecode: {:?}",
            run.status.last_timecode
        );
        let offsets = lip_sync(&seen, &heard);
        eprintln!(
            "{name}: {} slots, audio - video at each flash {:?} µs, status offset {:?}",
            seen.len(),
            offsets.iter().map(|ns| ns / 1000).collect::<Vec<_>>(),
            run.status.audio.as_ref().map(|audio| audio.offset_us)
        );
        assert!(
            offsets.len() >= 2,
            "{name}: flashes and beeps heard: {offsets:?} {heard:?}"
        );
        let audio = run.status.audio.as_ref().expect("audio");
        assert!(
            audio.offset_us.unsigned_abs() * 1000 < fps.period_ns(),
            "{name}: {audio:?}"
        );
        assert!(
            offsets
                .iter()
                .all(|offset| offset.unsigned_abs() < fps.period_ns()),
            "{name}: lip sync within a frame: {offsets:?}"
        );
        if interlaced {
            assert!(seen.iter().all(|slot| slot.interlace == "interleaved"
                && slot.field_order.as_deref() == Some("top-field-first")));
            // Both fields as they came, the top one first: the bottom field
            // shows the later picture.
            let parities: Vec<f64> = seen
                .iter()
                .filter(|slot| slot.flash < 200.)
                .map(|slot| slot.parity)
                .collect();
            assert!(
                parities.iter().all(|parity| (3.0..=5.0).contains(parity)),
                "{name}: field parity {parities:?}"
            );
        } else {
            assert!(seen.iter().all(|slot| slot.interlace == "progressive"));
        }
    }
}

/// The development host has no DeckLink card: a DeckLink output fails to
/// start with a clear error (the card's code paths are not verified on
/// hardware).
#[test]
fn a_decklink_output_without_a_card_fails_clearly() {
    gst::init().unwrap();
    if gst::ElementFactory::find("decklinkvideosink").is_none()
        || chr_runtime::gst_core::decklink::devices() > 0
    {
        eprintln!("skipped: no decklink plugin, or a card is present");
        return;
    }
    let fixture = Fixture::new("decklink", 0);
    let output = VideoMode {
        width: 1920,
        height: 1080,
        fps: Rate::fps(25),
    };
    let (_, recorder) = SdiStats::recorded(output);
    let error = Core::start(CoreConfig {
        capture: output,
        output,
        output_rate: OutputRate::Follow,
        capture_socket: fixture.path("capture.sock"),
        program_delay: DELAY,
        tap: None,
        recorder,
        sink: OutputSink::DeckLink { device: 0 },
        input_loss: InputLoss::Hold,
        audio: true,
    })
    .err()
    .expect("no card")
    .to_string();
    assert!(error.contains("no DeckLink device 0"), "{error}");
}
