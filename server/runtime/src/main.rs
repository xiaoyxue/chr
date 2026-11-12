//! Headless render server.
//!
//! One process, one GPU context, one direction of video:
//!
//! ```text
//! args (JSON-RPC) -> control -> pipeline: render -> SDI frame (UYVY)
//!                                  -> sdi: program output port (own clock)
//!                                       -> monitor: SDI -> H.264 -> WebRTC -> clients
//! ```
//!
//! Control arrives over a NativeSocket (Unix Domain Socket) and a loopback WebSocket.
//! The renderer's only product is the SDI program output; clients receive that output
//! back as WebRTC.

use std::{error::Error, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use chr_common::server::{IpcServer, ServerConfig, WebSocketListener};
use chr_renderer::coordinates::ImageExtent;

use chr_server::control::ControlHandler;
use chr_server::monitor::{
    Monitor,
    encoder::VideoFeed,
    webrtc::{IceConfig, WebRtcHub},
};
use chr_server::pipeline::{RenderConfig, RenderWorker};
use chr_server::sdi::{self, SdiOutput, VideoMode};

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
  --webrtc-udp <addr>     Local UDP bind for WebRTC; a wildcard binds every non-loopback
                          interface; \"none\" disables UDP [default: 0.0.0.0:0]
  --webrtc-tcp <addr>     First ICE-TCP listen address; previews use 4 consecutive
                          ports from it. Use 127.0.0.1:8788 to reach the server
                          through an SSH port forward [default: off]
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
    ice: IceConfig,
    allowed_origins: Vec<String>,
    synthetic_program: bool,
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Args>> {
    let mut args = Args {
        ws: "127.0.0.1:8787".parse()?,
        socket: None,
        width: 1280,
        height: 720,
        fps: 50,
        ice: IceConfig {
            udp: Some("0.0.0.0:0".into()),
            tcp_base: None,
        },
        allowed_origins: Vec::new(),
        synthetic_program: false,
    };
    let mut use_socket = true;
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
            "--webrtc-udp" => {
                let addr = value()?;
                args.ice.udp = (addr != "none").then_some(addr);
            }
            "--webrtc-tcp" => args.ice.tcp_base = Some(value()?.parse()?),
            "--allow-origin" => args.allowed_origins.push(value()?),
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
        }
    }
    if !args.width.is_multiple_of(2) || !args.height.is_multiple_of(2) {
        return Err("H.264 4:2:0 needs an even width and height".into());
    }
    if args.ice.udp.is_none() && args.ice.tcp_base.is_none() {
        return Err("WebRTC needs UDP or TCP; do not disable both".into());
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
    Ok(Some(args))
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
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,webrtc=warn,rtc=warn"),
    )
    .init();
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
    let extent = ImageExtent::new(args.width, args.height)?;
    let feed = Arc::new(VideoFeed::new());
    let mode = VideoMode {
        width: args.width,
        height: args.height,
        fps: args.fps,
    };
    let installation = if args.synthetic_program {
        Some(chr_server::program::synthetic::Synthetic::install(
            mode,
            chr_server::program::synthetic::runtime_id()?,
        )?)
    } else {
        None
    };
    let (sdi, tap) = if let Some((synthetic, _)) = &installation {
        SdiOutput::spawn_program(
            mode,
            chr_server::program::synthetic::id(format!(
                "{}-program",
                synthetic.configuration.runtime_id.as_str()
            ))?,
        )?
    } else {
        SdiOutput::spawn(mode)?
    };
    let monitor = Monitor::spawn(mode, tap, Arc::clone(&feed))?;
    let render_config = RenderConfig {
        extent,
        fps: args.fps,
    };
    let worker = if let Some((synthetic, _)) = &installation {
        RenderWorker::spawn_program(
            render_config,
            sdi.input(),
            synthetic.configuration.clone(),
            synthetic.scheduler.clone(),
        )?
    } else {
        RenderWorker::spawn(render_config, sdi.input())?
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
                let (control, owner) = chr_server::retained::SceneControl::spawn(adapter, 64).map_err(|e| e.to_string())?;
                synthetic.create_scene(&control).await?;
                log::info!("synthetic-program-v1 runtime_id={} scene_id={} configuration_id={}",
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
        let hub = Arc::new(WebRtcHub::new(feed, args.fps, args.ice.clone()));
        let handler = ControlHandler::new(commands, hub, sdi.stats());
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
    // Dropping the runtime closes listeners (removing the socket file) and connections.
    runtime.shutdown_timeout(Duration::from_secs(2));
    let stopped = worker.shutdown();
    drop(sdi);
    drop(monitor);
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

    /// Parses with an explicit socket path first, so results never depend on
    /// `XDG_RUNTIME_DIR`; later arguments still override it.
    fn parse(args: &[&str]) -> Result<Option<Args>> {
        let explicit_socket = ["--socket", "/tmp/chr-test.sock"];
        parse_args(
            explicit_socket
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
        let ice = parse(&["--webrtc-udp", "none", "--webrtc-tcp", "127.0.0.1:8788"])
            .unwrap()
            .unwrap()
            .ice;
        assert_eq!(ice.udp, None);
        assert_eq!(ice.tcp_base, Some("127.0.0.1:8788".parse().unwrap()));
        assert!(parse(&["--help"]).unwrap().is_none());
        for bad in [
            &["--size", "641x360"][..],
            &["--fps", "0"],
            &["--fps"],
            &["--bogus"],
            &["--ws", "nonsense"],
            &["--webrtc-udp", "none"],
            &["--webrtc-tcp", "localhost"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
