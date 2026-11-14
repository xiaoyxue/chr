//! Headless render server.
//!
//! One process, one GPU context, one direction of video:
//!
//! ```text
//! args (JSON-RPC) -> control -> pipeline: render -> SDI frame (UYVY)
//!                                  -> sdi: program output port (own clock)
//!                                       -> program tap (shared memory) -> chr-monitor
//! ```
//!
//! A tracked program pairs each captured frame with its camera. With
//! `--tracking-worker` the server hands every frame to a supervised tracking
//! worker (`chr-tracking-sim-cli` until the real module replaces it) and takes
//! its cameras; otherwise cameras arrive as `tracking.frame` calls.
//!
//! Control arrives over a NativeSocket (Unix Domain Socket) and a loopback WebSocket.
//! The renderer's only product is the SDI program output. Viewers watch it through the
//! separate `chr-monitor` process, which reads the program tap and never feeds back.

use std::{error::Error, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use chr_common::server::{IpcServer, ServerConfig, WebSocketListener};
use chr_renderer::coordinates::ImageExtent;

use chr_runtime::calibration::tracking::{
    CapturedFrame, MAX_WAIT, TrackingFeed, TrackingJoin, capture_backlog,
};
use chr_runtime::control::ControlHandler;
use chr_runtime::input::ring::RingInput;
use chr_runtime::pipeline::{RenderConfig, RenderWorker};
use chr_runtime::program::spec::ProgramSpec;
use chr_runtime::sdi::{self, SdiOutput, Taps, VideoMode};
use chr_runtime::worker::tracking::supervisor::{Tracker, TrackerConfig};
use chr_runtime::worker::{
    protocol::{Output, WorkerProgram},
    supervisor::{WorkerAssets, WorkerConfig, status_handle},
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// Tracking samples held ahead of their frames.
const TRACKING_BACKLOG: usize = 64;

const USAGE: &str = "\
Usage: chr-server [options]

Options:
  --ws <addr>             Loopback WebSocket address [default: 127.0.0.1:8787]
  --socket <path>         NativeSocket path [default: $XDG_RUNTIME_DIR/chr/render.sock]
  --no-socket             Do not listen on a NativeSocket
  --size <WxH>            Render size; both even [default: 1280x720]
  --fps <n>               Frames per second, 1-120 [default: 50]
  --synthetic-program    Opt in to synthetic-program-v1 scene/image output (max 1280x720)
  --software-video <mp4> Loop a local MP4 through FFmpeg/GStreamer and simulated SDI
                          (Linux gstreamer-media feature; --fps must match source)
  --video-scene <json>   Local calibrated-video-v1 scene/lens manifest (requires --software-video)
  --tracked-scene <json> The same manifest as tracked-video-v1: frames come from the input
                          ring and each frame's Ti/C/warp from tracking.frame (chr-tracking-sim)
  --description <json>    A chr-render-description-v1 scene and lens, with the same tracked
                          input as --tracked-scene
  --asset-root <dir>      Folder that description.load may read assets from (repeatable;
                          requires --description)
  --input-ring <path>     Shared-memory input ring for --tracked-scene or --description
                          [default: $XDG_RUNTIME_DIR/chr/input.ring]
  --tracking-wait-ms <n>  How long a captured frame waits for its camera before it is
                          skipped, at most 500 [default: 3 frame periods]
  --tracking-worker <json> Take cameras from a supervised tracking worker that gets every
                          captured frame and this calibration file, instead of from
                          tracking.frame (requires --tracked-scene or --description)
  --tracking-cli <path>   Tracking worker executable, run as `<path> worker`
                          [default: chr-tracking-sim-cli next to chr-server, a replay]
  --tracking-worker-arg <arg> Pass one argument to the tracking worker (repeatable)
  --pitch-demo <json>     Opt in to owner-local pitch-demo-v1 manifest (max 1280x720 output)
  --pitch-controls <json> Write local harness metadata (requires --pitch-demo; owner-only)
  --monitor-tap <path>    Shared-memory program tap for chr-monitor; \"none\" disables it
                          [default: $XDG_RUNTIME_DIR/chr/program.tap]
  --renderer-cli <path>   chr-renderer-cli executable that runs the render worker
                          [default: next to chr-server]
  --render-deadline-ms <n> Worker time per frame before it is sent as clean feed
                          [default: 3/4 of the frame period]
  --worker-hang-ms <n>    Replace a worker whose oldest request is older [default: 2000]
  --allow-origin <origin> Extra browser origin for the WebSocket (repeatable); loopback
                          origins are always allowed
  -h, --help              Show this help";

#[derive(Debug)]
struct Args {
    ws: SocketAddr,
    socket: Option<PathBuf>,
    width: u32,
    height: u32,
    fps: u32,
    monitor_tap: Option<PathBuf>,
    renderer_cli: Option<PathBuf>,
    render_deadline_ms: Option<u64>,
    worker_hang_ms: u64,
    allowed_origins: Vec<String>,
    synthetic_program: bool,
    pitch_demo: Option<PathBuf>,
    pitch_controls: Option<PathBuf>,
    software_video: Option<PathBuf>,
    video_scene: Option<PathBuf>,
    tracked_scene: Option<PathBuf>,
    description: Option<PathBuf>,
    asset_roots: Vec<PathBuf>,
    input_ring: Option<PathBuf>,
    tracking_wait_ms: Option<u64>,
    tracking_worker: Option<PathBuf>,
    tracking_cli: Option<PathBuf>,
    tracking_worker_args: Vec<String>,
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Args>> {
    let mut args = Args {
        ws: "127.0.0.1:8787".parse()?,
        socket: None,
        width: 1280,
        height: 720,
        fps: 50,
        monitor_tap: None,
        renderer_cli: None,
        render_deadline_ms: None,
        worker_hang_ms: 2000,
        allowed_origins: Vec::new(),
        synthetic_program: false,
        pitch_demo: None,
        pitch_controls: None,
        software_video: None,
        video_scene: None,
        tracked_scene: None,
        description: None,
        asset_roots: Vec::new(),
        input_ring: None,
        tracking_wait_ms: None,
        tracking_worker: None,
        tracking_cli: None,
        tracking_worker_args: Vec::new(),
    };
    let mut use_socket = true;
    let mut use_tap = true;
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--ws" => args.ws = value()?.parse()?,
            "--socket" => args.socket = Some(value()?.into()),
            "--no-socket" => use_socket = false,
            "--size" => {
                let size = value()?;
                let (width, height) = size
                    .split_once('x')
                    .ok_or("--size must look like 1280x720")?;
                (args.width, args.height) = (width.parse()?, height.parse()?);
            }
            "--fps" => args.fps = value()?.parse()?,
            "--synthetic-program" => args.synthetic_program = true,
            "--software-video" => args.software_video = Some(value()?.into()),
            "--video-scene" => args.video_scene = Some(value()?.into()),
            "--tracked-scene" => args.tracked_scene = Some(value()?.into()),
            "--description" => args.description = Some(value()?.into()),
            "--asset-root" => args.asset_roots.push(value()?.into()),
            "--input-ring" => args.input_ring = Some(value()?.into()),
            "--tracking-wait-ms" => args.tracking_wait_ms = Some(value()?.parse()?),
            "--tracking-worker" => args.tracking_worker = Some(value()?.into()),
            "--tracking-cli" => args.tracking_cli = Some(value()?.into()),
            "--tracking-worker-arg" => args.tracking_worker_args.push(value()?),
            "--pitch-demo" => args.pitch_demo = Some(value()?.into()),
            "--pitch-controls" => args.pitch_controls = Some(value()?.into()),
            "--renderer-cli" => args.renderer_cli = Some(value()?.into()),
            "--render-deadline-ms" => args.render_deadline_ms = Some(value()?.parse()?),
            "--worker-hang-ms" => args.worker_hang_ms = value()?.parse()?,
            "--monitor-tap" => match value()?.as_str() {
                "none" => use_tap = false,
                path => (use_tap, args.monitor_tap) = (true, Some(path.into())),
            },
            "--allow-origin" => args.allowed_origins.push(value()?),
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
        }
    }
    if !args.width.is_multiple_of(2) || !args.height.is_multiple_of(2) {
        return Err("UYVY output and 4:2:0 monitoring need an even width and height".into());
    }
    if usize::from(args.synthetic_program)
        + usize::from(args.pitch_demo.is_some())
        + usize::from(args.software_video.is_some())
        + usize::from(args.tracked_scene.is_some())
        + usize::from(args.description.is_some())
        > 1
        || (args.pitch_controls.is_some() && args.pitch_demo.is_none())
    {
        return Err("select one program profile; --pitch-controls requires --pitch-demo".into());
    }
    if args.video_scene.is_some() && args.software_video.is_none() {
        return Err("--video-scene requires --software-video".into());
    }
    let tracked = args.tracked_scene.is_some() || args.description.is_some();
    if (args.input_ring.is_some()
        || args.tracking_wait_ms.is_some()
        || args.tracking_worker.is_some())
        && !tracked
    {
        return Err(
            "--input-ring, --tracking-wait-ms and --tracking-worker require --tracked-scene or --description"
                .into(),
        );
    }
    if (args.tracking_cli.is_some() || !args.tracking_worker_args.is_empty())
        && args.tracking_worker.is_none()
    {
        return Err("--tracking-cli and --tracking-worker-arg require --tracking-worker".into());
    }
    if !args.asset_roots.is_empty() && args.description.is_none() {
        return Err("--asset-root requires --description".into());
    }
    if args
        .tracking_wait_ms
        .is_some_and(|ms| Duration::from_millis(ms) > MAX_WAIT)
    {
        return Err(format!(
            "--tracking-wait-ms must be at most {} ms",
            MAX_WAIT.as_millis()
        )
        .into());
    }
    if tracked && args.input_ring.is_none() {
        args.input_ring = Some(
            chr_shm::input::default_path()
                .ok_or("XDG_RUNTIME_DIR is not set; pass --input-ring <path>")?,
        );
    }
    if args.render_deadline_ms == Some(0) || args.worker_hang_ms == 0 {
        return Err("--render-deadline-ms and --worker-hang-ms must be positive".into());
    }
    if !(1..=120).contains(&args.fps) {
        return Err("--fps must be between 1 and 120".into());
    }
    if use_socket && args.socket.is_none() {
        args.socket = Some(default_socket_path()?);
    }
    if !use_socket {
        args.socket = None;
    }
    if !use_tap {
        args.monitor_tap = None;
    } else if args.monitor_tap.is_none() {
        // The program never needs the monitor, so a missing default only disables it.
        args.monitor_tap = chr_shm::tap::default_path();
        if args.monitor_tap.is_none() {
            log::warn!(
                "XDG_RUNTIME_DIR is not set: no program tap for chr-monitor; pass --monitor-tap <path>"
            );
        }
    }
    Ok(Some(args))
}

/// A private directory for this server's worker rings.
fn worker_ring_dir() -> PathBuf {
    private_dir("worker")
}

/// A private runtime directory named after `kind` and this server's PID.
fn private_dir(kind: &str) -> PathBuf {
    let name = format!("{kind}-{}", std::process::id());
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) => PathBuf::from(runtime).join("chr").join(name),
        None => std::env::temp_dir().join(format!("chr-{name}")),
    }
}

fn default_socket_path() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or("XDG_RUNTIME_DIR is not set; pass --socket <path> or --no-socket")?;
    Ok(PathBuf::from(runtime).join("chr").join("render.sock"))
}

/// Creates the socket's parent directory as owner-only if it does not exist yet.
#[cfg(unix)]
fn prepare_socket_dir(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if let Some(parent) = path.parent().filter(|parent| !parent.exists()) {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    Ok(())
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return;
        }
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    };
    if let Err(error) = run(args) {
        log::error!("{error}");
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<()> {
    remove_stale_private_dirs();
    if args.software_video.is_some() && !cfg!(all(unix, feature = "gstreamer-media")) {
        return Err("--software-video requires Linux/Unix and the gstreamer-media feature".into());
    }
    ImageExtent::new(args.width, args.height)?;
    let mode = VideoMode {
        width: args.width,
        height: args.height,
        fps: args.fps,
    };
    let spec = if let Some(path) = &args.pitch_demo {
        Some(ProgramSpec::PitchDemo { path: path.clone() })
    } else if let Some(path) = &args.video_scene {
        Some(ProgramSpec::VideoScene { path: path.clone() })
    } else if let Some(path) = &args.tracked_scene {
        Some(ProgramSpec::TrackedScene { path: path.clone() })
    } else if let Some(path) = &args.description {
        Some(ProgramSpec::Description { path: path.clone() })
    } else if args.software_video.is_some() {
        Some(ProgramSpec::SoftwareVideo)
    } else if args.synthetic_program {
        Some(ProgramSpec::Synthetic)
    } else {
        None
    };
    let installation = match &spec {
        Some(spec) => {
            let installed = spec.install(mode, chr_runtime::program::synthetic::runtime_id()?)?;
            if let (Some(path), Some(controls)) = (&args.pitch_controls, &installed.controls) {
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                serde_json::to_writer_pretty(options.open(path)?, controls)?;
            }
            Some((installed.program, installed.adapter))
        }
        None => None,
    };
    #[cfg(all(unix, feature = "gstreamer-media"))]
    let mut video_source = args
        .software_video
        .as_ref()
        .map(|path| {
            let program = &installation
                .as_ref()
                .expect("software video installation")
                .0;
            let native = program.configuration.native_extent;
            chr_runtime::input::software::VideoInput::open(
                path,
                VideoMode {
                    width: native.width,
                    height: native.height,
                    fps: mode.fps,
                },
                program.video_lens.as_ref().map(|lens| lens.frame_count()),
            )
        })
        .transpose()?;
    // The capture side of a tracked program: frames from the input ring wait in the
    // join for the camera that the tracking worker or tracking.frame delivers.
    let tracker_status = chr_runtime::worker::tracking::supervisor::status_handle();
    let tracking = match (&installation, &args.input_ring) {
        (Some((program, _)), Some(path)) => {
            let native = program.configuration.native_extent;
            let wait = args
                .tracking_wait_ms
                .map_or(mode.frame_duration() * 3, Duration::from_millis);
            let feed = TrackingFeed::new(TRACKING_BACKLOG);
            let join = TrackingJoin::new(
                feed.clone(),
                wait,
                capture_backlog(wait, mode.frame_duration()),
            );
            let input = RingInput::new(
                path.clone(),
                VideoMode {
                    width: native.width,
                    height: native.height,
                    fps: mode.fps,
                },
            );
            let tracker = match &args.tracking_worker {
                Some(calibration) => Some(Tracker::new(
                    TrackerConfig {
                        executable: match &args.tracking_cli {
                            Some(path) => path.clone(),
                            None => std::env::current_exe()?.with_file_name(format!(
                                "chr-tracking-sim-cli{}",
                                std::env::consts::EXE_SUFFIX
                            )),
                        },
                        args: args.tracking_worker_args.clone(),
                        calibration: calibration.clone(),
                        width: native.width,
                        height: native.height,
                        fps: mode.fps,
                        ring_dir: private_dir("tracking"),
                        hang: Duration::from_millis(args.worker_hang_ms),
                    },
                    Arc::clone(&tracker_status),
                )?),
                None => None,
            };
            log::info!(
                "tracked input: frames from {}, cameras from {}, wait {wait:?}",
                path.display(),
                match &args.tracking_worker {
                    Some(calibration) => format!("the tracking worker ({})", calibration.display()),
                    None => "tracking.frame".to_owned(),
                }
            );
            Some((feed, join, input, tracker))
        }
        _ => None,
    };
    let feed = tracking.as_ref().map(|(feed, ..)| feed.clone());
    let taps = match &args.monitor_tap {
        Some(path) => {
            let taps = Taps::ring(mode, path)?;
            log::info!("program tap for chr-monitor at {}", path.display());
            taps
        }
        None => Taps::default(),
    };
    let sdi = if let Some((synthetic, _)) = &installation {
        SdiOutput::spawn_program(
            mode,
            chr_runtime::program::synthetic::id(format!(
                "{}-program",
                synthetic.configuration.runtime_id.as_str()
            ))?,
            taps,
        )?
    } else {
        SdiOutput::spawn(mode, taps)?
    };
    let status = status_handle();
    let worker_assets = WorkerAssets::default();
    // Asset roots are compared as canonical paths, like the asset paths themselves.
    let asset_roots = args
        .asset_roots
        .iter()
        .map(|root| root.canonicalize())
        .collect::<std::io::Result<Vec<_>>>()?;
    let worker_config = WorkerConfig {
        executable: match &args.renderer_cli {
            Some(path) => path.clone(),
            None => std::env::current_exe()?
                .with_file_name(format!("chr-renderer-cli{}", std::env::consts::EXE_SUFFIX)),
        },
        program: WorkerProgram::new(spec.as_ref()),
        runtime_id: match &installation {
            Some((synthetic, _)) => synthetic.configuration.runtime_id.clone(),
            None => chr_runtime::program::synthetic::runtime_id()?,
        },
        output: Output {
            width: mode.width,
            height: mode.height,
            fps: mode.fps,
        },
        // Room for the widest input format, RGBA16F.
        input_bytes: installation.as_ref().map_or(0, |(synthetic, _)| {
            let native = synthetic.configuration.native_extent;
            u64::from(native.width) * u64::from(native.height) * 8
        }),
        ring_dir: worker_ring_dir(),
        hang: Duration::from_millis(args.worker_hang_ms),
        assets: worker_assets.clone(),
    };
    log::info!("render worker: {}", worker_config.executable.display());
    let render_config = RenderConfig {
        mode,
        render_deadline: args
            .render_deadline_ms
            // Leave a quarter of the frame for clean-feed conversion and hand-off.
            .map_or(mode.frame_duration() * 3 / 4, Duration::from_millis),
    };
    let worker = if let Some((synthetic, _)) = &installation {
        RenderWorker::spawn_program(
            render_config,
            sdi.input(),
            worker_config,
            Arc::clone(&status),
            synthetic.scheduler.clone(),
        )?
    } else {
        RenderWorker::spawn(
            render_config,
            sdi.input(),
            worker_config,
            Arc::clone(&status),
        )?
    };
    log::info!(
        "rendering {} with simulated SDI output ({})",
        mode.name(),
        sdi::PIXEL_FORMAT
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let commands = worker.commands.clone();
    let served = runtime.block_on(async {
        let installed = match installation {
            Some((synthetic, adapter)) => {
                let adapter = if args.software_video.is_some() || args.tracked_scene.is_some() || args.description.is_some() { adapter.with_rotation() } else { adapter };
                let (control, owner) = chr_runtime::retained::SceneControl::spawn(adapter, 64).map_err(|e| e.to_string())?;
                synthetic.create_scene(&control).await?;
                log::info!("local program runtime_id={} scene_id={} configuration_id={}",
                    synthetic.configuration.runtime_id.as_str(), synthetic.scene_id.as_str(), synthetic.configuration.configuration_id.as_str());
                Some((synthetic, control, owner))
            }
            None => None,
        };
        let (synthetic, scenes, owner) = match installed {
            Some((synthetic, control, owner)) => (Some(synthetic), Some(control), Some(owner)),
            None => (None, None, None),
        };
        let loader = match (&synthetic, &scenes) {
            (Some(program), Some(control)) if program.described.is_some() => Some(Arc::new(
                chr_runtime::program::description::Loader::new(program, control.clone(), asset_roots.clone(), worker_assets.clone(), worker_ring_dir().join("assets"))?,
            )),
            _ => None,
        };
        let result = {
        let handler = ControlHandler::new(commands, sdi.stats()).with_worker(Arc::clone(&status));
        let handler = match &scenes { Some(control) => handler.with_scenes(control.clone()), None => handler };
        let handler = match feed { Some(feed) => handler.with_tracking(feed), None => handler };
        let handler = if args.tracking_worker.is_some() { handler.with_tracking_worker(Arc::clone(&tracker_status)) } else { handler };
        let handler = Arc::new(match loader { Some(loader) => handler.with_description(loader), None => handler });
        let server = IpcServer::new(
            handler,
            ServerConfig {
                extra_allowed_origins: args.allowed_origins.clone(),
                ..ServerConfig::default()
            },
        );
        let websocket = WebSocketListener::bind(args.ws).await?;
        log::info!("websocket listening on ws://{}", websocket.local_addr()?);
        let native = serve_native(&server, args.socket.as_deref()).await?;
        let render_watch = async {
            while !worker.is_finished() {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        };
        let injection = async {
            let Some(mut synthetic) = synthetic else { return std::future::pending::<Result<()>>().await };
            let control = scenes.as_ref().expect("synthetic scene endpoint");
            let mut tracking = tracking;
            // Capture sequences restart with the capture process; input sequences may not.
            let mut capture_offset = 0;
            let mut progress = tokio::time::interval(Duration::from_millis(5));
            progress.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut frames = tokio::time::interval(mode.frame_duration());
            frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = progress.tick() => {
                        synthetic.scheduler.tick().map_err(|e| e.to_string())?;
                        if let Some((feed, _, _, Some(tracker))) = &mut tracking {
                            accept_cameras(feed, tracker.poll());
                        }
                    }
                    _ = frames.tick() => {
                        if let Some((feed, join, input, tracker)) = &mut tracking {
                            control.advance_rotation(sdi.stats().status().frames_output).await.map_err(|e| e.to_string())?;
                            let polled = input.poll();
                            if polled.restarted {
                                log::info!("input ring at {} was replaced; capture sequences restart", input.path().display());
                                join.reset();
                                if let Some(tracker) = tracker { tracker.forget(); }
                                capture_offset = synthetic.next_sequence();
                            }
                            feed.count_overwritten(polled.overwritten);
                            let arrived = std::time::Instant::now();
                            for (sequence, rgba) in polled.frames {
                                if let Some(tracker) = tracker { tracker.track(sequence, &rgba); }
                                join.frame(CapturedFrame { sequence, rgba, arrived });
                            }
                            if let Some(tracker) = tracker { accept_cameras(feed, tracker.poll()); }
                            if let Some((frame, sample)) = join.next(std::time::Instant::now()) {
                                let geometry = synthetic.tracking.as_ref().expect("tracked program").geometry(&sample);
                                if let Err(error) = synthetic.inject_with(control, capture_offset + frame.sequence, &frame.rgba, geometry).await {
                                    log::warn!("tracked frame {} skipped: {error}", frame.sequence);
                                }
                            }
                            continue;
                        }
                        #[cfg(all(unix, feature = "gstreamer-media"))]
                        if let Some(source) = &mut video_source {
                            control.advance_rotation(sdi.stats().status().frames_output).await.map_err(|e| e.to_string())?;
                            if let Some(frame) = source.try_frame()?
                                && let Err(error) = synthetic.inject_image(control, frame.sequence, &frame.rgba).await {
                                log::warn!("software input skipped: {error}");
                            }
                            continue;
                        }
                        if let Err(error) = synthetic.inject(control).await {
                            log::warn!("synthetic injection skipped: {error}");
                        }
                    }
                }
            }
        };
        tokio::select! {
            result = server.serve_websocket(websocket) => result.map_err(Into::into),
            result = native => result,
            () = render_watch => Err("render loop stopped".into()),
            result = injection => result,
            signal = shutdown_signal() => {
                signal?;
                log::info!("shutting down");
                Ok(())
            }
        }
        };
        if let Some(control) = &scenes { control.shutdown().await; }
        drop(scenes);
        if let Some(owner) = owner { owner.await?; }
        result
    });
    #[cfg(all(unix, feature = "gstreamer-media"))]
    drop(video_source);
    // Dropping the runtime closes listeners (removing the socket file) and connections.
    runtime.shutdown_timeout(Duration::from_secs(2));
    let stopped = worker.shutdown();
    drop(sdi);
    if let Some(path) = &args.monitor_tap {
        // A monitor that still maps the tap keeps its pages until it lets go.
        let _ = std::fs::remove_file(path);
    }
    served?;
    stopped.map_err(Into::into)
}

/// Resolves on Ctrl-C or SIGTERM (`stack.py stop`, service managers), so both shut
/// the server down cleanly and its workers' ring directories are removed.
async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            signal = tokio::signal::ctrl_c() => signal,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}

/// Removes the worker and tracking ring directories of chr-servers that are gone:
/// a server that was killed (SIGKILL, a crash) cannot remove its own, and they
/// live in memory-backed `$XDG_RUNTIME_DIR`.
fn remove_stale_private_dirs() {
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return;
    };
    for path in stale_private_dirs(&PathBuf::from(runtime).join("chr"), server_running) {
        match std::fs::remove_dir_all(&path) {
            Ok(()) => log::info!("removed {} left by a stopped chr-server", path.display()),
            Err(error) => log::warn!("cannot remove {}: {error}", path.display()),
        }
    }
}

/// `<root>/worker-<pid>` and `<root>/tracking-<pid>` directories whose server is not running.
fn stale_private_dirs(root: &std::path::Path, running: impl Fn(u32) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut stale: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let (kind, pid) = name.split_once('-')?;
            let pid = pid.parse().ok()?;
            (matches!(kind, "worker" | "tracking") && entry.path().is_dir() && !running(pid))
                .then(|| entry.path())
        })
        .collect();
    stale.sort();
    stale
}

/// Whether `pid` is a running chr-server. Without `/proc` every PID counts as running,
/// so nothing is removed.
fn server_running(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/comm")) {
        Ok(comm) => comm.trim_end() == "chr-server",
        Err(_) => !std::path::Path::new("/proc/self").exists(),
    }
}

/// Queues the tracking worker's cameras for the join; it counts late ones.
fn accept_cameras(feed: &TrackingFeed, cameras: Vec<chr_common::methods::tracking::TrackingFrame>) {
    for camera in cameras {
        let sequence = camera.sequence.get();
        if let Err(error) = feed.push(camera) {
            log::debug!("camera of frame {sequence} not used: {}", error.message);
        }
    }
}

#[cfg(unix)]
async fn serve_native(
    server: &IpcServer<ControlHandler>,
    path: Option<&std::path::Path>,
) -> Result<impl std::future::Future<Output = Result<()>> + use<>> {
    use chr_common::server::NativeSocketListener;
    let listener = match path {
        Some(path) => {
            prepare_socket_dir(path)?;
            let listener = NativeSocketListener::bind(path).await?;
            log::info!("native socket listening on {}", listener.path().display());
            Some(listener)
        }
        None => None,
    };
    let server = server.clone();
    Ok(async move {
        match listener {
            Some(listener) => server
                .serve_native_socket(listener)
                .await
                .map_err(Into::into),
            None => std::future::pending().await,
        }
    })
}

#[cfg(not(unix))]
async fn serve_native(
    _server: &IpcServer<ControlHandler>,
    path: Option<&std::path::Path>,
) -> Result<impl std::future::Future<Output = Result<()>> + use<>> {
    if path.is_some() {
        return Err("NativeSocket requires a Unix platform; pass --no-socket".into());
    }
    Ok(std::future::pending())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses with explicit socket and tap paths first, so results never depend on
    /// `XDG_RUNTIME_DIR`; later arguments still override them.
    fn parse(args: &[&str]) -> Result<Option<Args>> {
        let explicit_paths = [
            "--socket",
            "/tmp/chr-test.sock",
            "--monitor-tap",
            "/tmp/chr-test.tap",
        ];
        parse_args(
            explicit_paths
                .iter()
                .chain(args)
                .map(|arg| (*arg).to_owned()),
        )
    }

    #[test]
    fn only_ring_directories_of_stopped_servers_are_stale() {
        let root = std::env::temp_dir().join(format!("chr-stale-{}", std::process::id()));
        for dir in [
            "worker-1",
            "tracking-1",
            "worker-2",
            "tracking-x",
            "other-3",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("worker-4"), b"").unwrap();
        assert_eq!(
            stale_private_dirs(&root, |pid| pid == 2),
            [root.join("tracking-1"), root.join("worker-1")]
        );
        assert!(stale_private_dirs(&root.join("missing"), |_| false).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn parses_options_and_rejects_bad_values() {
        let args = parse(&[
            "--size",
            "640x360",
            "--fps",
            "60",
            "--socket",
            "/tmp/x.sock",
            "--allow-origin",
            "http://studio:1",
        ])
        .unwrap()
        .unwrap();
        assert_eq!((args.width, args.height, args.fps), (640, 360, 60));
        assert_eq!(
            args.socket.as_deref(),
            Some(std::path::Path::new("/tmp/x.sock"))
        );
        assert_eq!(args.allowed_origins, ["http://studio:1"]);
        assert!(
            !args.synthetic_program,
            "legacy triangle remains the default"
        );
        assert!(
            parse(&["--synthetic-program"])
                .unwrap()
                .unwrap()
                .synthetic_program
        );

        assert!(parse(&["--no-socket"]).unwrap().unwrap().socket.is_none());
        assert!(parse(&["--software-video", "video.mp4", "--synthetic-program"]).is_err());
        assert!(parse(&["--software-video", "video.mp4", "--pitch-demo", "demo.json"]).is_err());
        assert!(parse(&["--video-scene", "scene.json"]).is_err());
        let tracked = parse(&[
            "--tracked-scene",
            "scene.json",
            "--input-ring",
            "/tmp/in.ring",
            "--tracking-wait-ms",
            "60",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            (tracked.input_ring, tracked.tracking_wait_ms),
            (Some(PathBuf::from("/tmp/in.ring")), Some(60))
        );
        assert!(parse(&["--tracked-scene", "scene.json", "--synthetic-program"]).is_err());
        assert!(parse(&["--input-ring", "/tmp/in.ring"]).is_err());
        assert!(parse(&["--tracking-wait-ms", "60"]).is_err());
        assert!(parse(&["--tracked-scene", "scene.json", "--tracking-wait-ms", "501"]).is_err());
        let worker = parse(&[
            "--description",
            "start.json",
            "--input-ring",
            "/tmp/in.ring",
            "--tracking-worker",
            "scene.json",
            "--tracking-cli",
            "/opt/tracker",
            "--tracking-worker-arg",
            "--delay-ms",
            "--tracking-worker-arg",
            "5",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            (
                worker.tracking_worker,
                worker.tracking_cli,
                worker.tracking_worker_args
            ),
            (
                Some(PathBuf::from("scene.json")),
                Some(PathBuf::from("/opt/tracker")),
                vec!["--delay-ms".to_owned(), "5".to_owned()]
            )
        );
        assert!(parse(&["--tracking-worker", "scene.json"]).is_err());
        assert!(
            parse(&[
                "--tracked-scene",
                "s.json",
                "--tracking-cli",
                "/opt/tracker"
            ])
            .is_err()
        );
        assert_eq!(
            parse(&[]).unwrap().unwrap().monitor_tap,
            Some(PathBuf::from("/tmp/chr-test.tap"))
        );
        assert!(
            parse(&["--monitor-tap", "none"])
                .unwrap()
                .unwrap()
                .monitor_tap
                .is_none()
        );
        assert_eq!(
            parse(&[
                "--software-video",
                "video.mp4",
                "--video-scene",
                "scene.json"
            ])
            .unwrap()
            .unwrap()
            .video_scene,
            Some(PathBuf::from("scene.json"))
        );
        assert_eq!(
            parse(&["--software-video", "video.mp4"])
                .unwrap()
                .unwrap()
                .software_video,
            Some(PathBuf::from("video.mp4"))
        );
        assert!(parse(&["--help"]).unwrap().is_none());
        for bad in [
            &["--size", "641x360"][..],
            &["--fps", "0"],
            &["--fps"],
            &["--bogus"],
            &["--ws", "nonsense"],
            &["--srt", "127.0.0.1:9001"],
            &["--webrtc-udp", "none"],
            &["--render-deadline-ms", "0"],
            &["--worker-hang-ms", "0"],
            &["--worker-hang-ms", "soon"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        let worker = parse(&[
            "--renderer-cli",
            "/opt/chr/chr-renderer-cli",
            "--render-deadline-ms",
            "15",
            "--worker-hang-ms",
            "500",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            worker.renderer_cli.as_deref(),
            Some(std::path::Path::new("/opt/chr/chr-renderer-cli"))
        );
        assert_eq!(
            (worker.render_deadline_ms, worker.worker_hang_ms),
            (Some(15), 500)
        );
        let defaults = parse(&[]).unwrap().unwrap();
        assert_eq!(
            (defaults.render_deadline_ms, defaults.worker_hang_ms),
            (None, 2000)
        );
    }
}
