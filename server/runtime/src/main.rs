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
//! Control arrives over a NativeSocket (Unix Domain Socket) and a loopback WebSocket.
//! The renderer's only product is the SDI program output. Viewers watch it through the
//! separate `chr-monitor` process, which reads the program tap and never feeds back.

use std::{error::Error, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use chr_common::server::{IpcServer, ServerConfig, WebSocketListener};
use chr_renderer::coordinates::ImageExtent;

use chr_runtime::control::ControlHandler;
use chr_runtime::pipeline::{RenderConfig, RenderWorker};
use chr_runtime::program::spec::ProgramSpec;
use chr_runtime::sdi::{self, SdiOutput, Taps, VideoMode};
use chr_runtime::worker::{
    protocol::{Output, WorkerProgram},
    supervisor::{WorkerConfig, status_handle},
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

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
        > 1
        || (args.pitch_controls.is_some() && args.pitch_demo.is_none())
    {
        return Err("select one program profile; --pitch-controls requires --pitch-demo".into());
    }
    if args.video_scene.is_some() && args.software_video.is_none() {
        return Err("--video-scene requires --software-video".into());
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
    let name = format!("worker-{}", std::process::id());
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
                let adapter = if args.software_video.is_some() { adapter.with_rotation() } else { adapter };
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
        let result = {
        let handler = ControlHandler::new(commands, sdi.stats()).with_worker(Arc::clone(&status));
        let handler = Arc::new(match &scenes { Some(control) => handler.with_scenes(control.clone()), None => handler });
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
            let mut progress = tokio::time::interval(Duration::from_millis(5));
            progress.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut frames = tokio::time::interval(mode.frame_duration());
            frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = progress.tick() => synthetic.scheduler.tick().map_err(|e| e.to_string())?,
                    _ = frames.tick() => {
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
            signal = tokio::signal::ctrl_c() => {
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
