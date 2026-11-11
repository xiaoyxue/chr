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

mod control;
mod monitor;
mod pipeline;
mod scene;
mod sdi;

use control::ControlHandler;
use monitor::{
    Monitor,
    encoder::VideoFeed,
    webrtc::{IceConfig, WebRtcHub},
};
use pipeline::{RenderConfig, RenderWorker};
use sdi::{SdiOutput, VideoMode};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const USAGE: &str = "\
Usage: chr-server [options]

Options:
  --ws <addr>             Loopback WebSocket address [default: 127.0.0.1:8787]
  --socket <path>         NativeSocket path [default: $XDG_RUNTIME_DIR/chr/render.sock]
  --no-socket             Do not listen on a NativeSocket
  --size <WxH>            Render size; both even [default: 1280x720]
  --fps <n>               Frames per second, 1-120 [default: 50]
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
    let (sdi, tap) = SdiOutput::spawn(mode)?;
    let monitor = Monitor::spawn(mode, tap, Arc::clone(&feed))?;
    let worker = RenderWorker::spawn(
        RenderConfig {
            extent,
            fps: args.fps,
        },
        sdi.input(),
    )?;
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
        let hub = Arc::new(WebRtcHub::new(feed, args.fps, args.ice.clone()));
        let handler = Arc::new(ControlHandler::new(commands, hub, sdi.stats()));
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
        tokio::select! {
            result = server.serve_websocket(websocket) => result.map_err(Into::into),
            result = native => result,
            () = render_watch => Err("render loop stopped".into()),
            signal = tokio::signal::ctrl_c() => {
                signal?;
                log::info!("shutting down");
                Ok(())
            }
        }
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
