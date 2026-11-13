//! `chr-monitor`: encodes the program output `chr-server` transmits for viewers.
//!
//! ```text
//! chr-server program tap -> chr-monitor: H.264 -> WebRTC (browsers) / SRT (callers)
//! ```
//!
//! Browsers negotiate WebRTC with `webrtc.offer` on this process's own loopback
//! WebSocket. Program control stays on `chr-server`; the monitor only reads the tap,
//! so stopping, killing or stalling it never affects the program output.

use std::{error::Error, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use chr_common::server::{IpcServer, ServerConfig, WebSocketListener};
use chr_monitor::{
    Counters, VideoMode,
    control::MonitorHandler,
    encoder::VideoFeed,
    tap::{EncoderKind, Monitor},
    webrtc::{IceConfig, MAX_PEERS, WebRtcHub},
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const USAGE: &str = "\
Usage: chr-monitor [options]

Options:
  --tap <path>            Program tap written by chr-server
                          [default: $XDG_RUNTIME_DIR/chr/program.tap]
  --size <WxH>            Program output size; must match chr-server [default: 1280x720]
  --fps <n>               Program output rate; must match chr-server [default: 50]
  --ws <addr>             Loopback WebSocket for webrtc.offer and monitor.get_status
                          [default: 127.0.0.1:8789]
  --encoder <name>        openh264, or gstreamer (x264; gstreamer-media feature)
                          [default: openh264]
  --srt <ip:port>         Direct MPEG-TS/SRT listener (gstreamer-media feature)
  --webrtc-udp <addr>     Local UDP bind for WebRTC; a wildcard binds every non-loopback
                          interface; \"none\" disables UDP [default: 0.0.0.0:0]
  --webrtc-tcp <addr>     First ICE-TCP listen address; previews use 4 consecutive
                          ports from it. Use 127.0.0.1:8790 to reach the monitor
                          through an SSH port forward [default: off]
  --allow-origin <origin> Extra browser origin for the WebSocket (repeatable); loopback
                          origins are always allowed
  -h, --help              Show this help";

#[derive(Debug)]
struct Args {
    tap: PathBuf,
    mode: VideoMode,
    ws: SocketAddr,
    encoder: EncoderKind,
    srt: Option<SocketAddr>,
    ice: IceConfig,
    allowed_origins: Vec<String>,
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Args>> {
    let mut tap = None;
    let mut args = Args {
        tap: PathBuf::new(),
        mode: VideoMode {
            width: 1280,
            height: 720,
            fps: 50,
        },
        ws: "127.0.0.1:8789".parse()?,
        encoder: EncoderKind::OpenH264,
        srt: None,
        ice: IceConfig {
            udp: Some("0.0.0.0:0".into()),
            tcp_base: None,
        },
        allowed_origins: Vec::new(),
    };
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--tap" => tap = Some(PathBuf::from(value()?)),
            "--size" => {
                let size = value()?;
                let (width, height) = size
                    .split_once('x')
                    .ok_or("--size must look like 1280x720")?;
                (args.mode.width, args.mode.height) = (width.parse()?, height.parse()?);
            }
            "--fps" => args.mode.fps = value()?.parse()?,
            "--ws" => args.ws = value()?.parse()?,
            "--encoder" => {
                args.encoder = match value()?.as_str() {
                    "openh264" => EncoderKind::OpenH264,
                    "gstreamer" => EncoderKind::GStreamer,
                    other => return Err(format!("unknown encoder {other:?}").into()),
                }
            }
            "--srt" => args.srt = Some(value()?.parse()?),
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
    let mode = args.mode;
    if mode.width == 0
        || mode.height == 0
        || !mode.width.is_multiple_of(2)
        || !mode.height.is_multiple_of(2)
    {
        return Err("--size needs a nonzero, even width and height".into());
    }
    if !(1..=120).contains(&mode.fps) {
        return Err("--fps must be between 1 and 120".into());
    }
    if args.ice.udp.is_none() && args.ice.tcp_base.is_none() {
        return Err("WebRTC needs UDP or TCP; do not disable both".into());
    }
    if let Some(base) = args.ice.tcp_base {
        let ports = u32::from(base.port())..u32::from(base.port()) + MAX_PEERS as u32;
        if ports.contains(&u32::from(args.ws.port())) {
            return Err(format!(
                "--webrtc-tcp uses ports {}-{}, which include the --ws port {}",
                ports.start,
                ports.end - 1,
                args.ws.port()
            )
            .into());
        }
    }
    if args.srt.is_some_and(|address| address.port() == 0) {
        return Err("--srt requires a nonzero listen port".into());
    }
    let media = cfg!(all(unix, feature = "gstreamer-media"));
    if !media && (args.srt.is_some() || args.encoder == EncoderKind::GStreamer) {
        return Err(
            "--srt and --encoder gstreamer need Linux/Unix and the gstreamer-media feature".into(),
        );
    }
    args.tap = match tap {
        Some(tap) => tap,
        None => {
            chr_shm::tap::default_path().ok_or("XDG_RUNTIME_DIR is not set; pass --tap <path>")?
        }
    };
    Ok(Some(args))
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
    let feed = Arc::new(VideoFeed::new());
    let counters = Arc::new(Counters::default());
    #[cfg(all(unix, feature = "gstreamer-media"))]
    let srt = args
        .srt
        .map(|bind| {
            chr_monitor::srt::SrtOutput::spawn(
                args.mode,
                bind,
                Arc::clone(&feed),
                Arc::clone(&counters),
            )
        })
        .transpose()?;
    let monitor = Monitor::spawn(
        args.tap.clone(),
        args.mode,
        args.encoder,
        Arc::clone(&feed),
        Arc::clone(&counters),
    )?;
    log::info!(
        "monitoring {} from {} with {:?}",
        args.mode.name(),
        args.tap.display(),
        args.encoder
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let served = runtime.block_on(async {
        let hub = Arc::new(WebRtcHub::new(feed, args.mode.fps, args.ice.clone()));
        let server = IpcServer::new(
            Arc::new(MonitorHandler::new(hub, counters)),
            ServerConfig {
                extra_allowed_origins: args.allowed_origins.clone(),
                ..ServerConfig::default()
            },
        );
        let websocket = WebSocketListener::bind(args.ws).await?;
        log::info!(
            "monitor websocket listening on ws://{}",
            websocket.local_addr()?
        );
        tokio::select! {
            result = server.serve_websocket(websocket) => result.map_err(Into::into),
            signal = tokio::signal::ctrl_c() => {
                signal?;
                log::info!("shutting down");
                Ok(())
            }
        }
    });
    // Dropping the runtime closes the listener, its connections and their previews.
    runtime.shutdown_timeout(Duration::from_secs(2));
    drop(monitor);
    #[cfg(all(unix, feature = "gstreamer-media"))]
    drop(srt);
    served
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Args>> {
        let explicit_tap = ["--tap", "/tmp/chr-test.tap"];
        parse_args(explicit_tap.iter().chain(args).map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn parses_options_and_rejects_bad_values() {
        let args = parse(&[
            "--size",
            "640x360",
            "--fps",
            "60",
            "--ws",
            "127.0.0.1:9000",
            "--webrtc-udp",
            "none",
            "--webrtc-tcp",
            "127.0.0.1:8790",
            "--allow-origin",
            "http://studio:1",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(args.tap, PathBuf::from("/tmp/chr-test.tap"));
        assert_eq!(
            args.mode,
            VideoMode {
                width: 640,
                height: 360,
                fps: 60
            }
        );
        assert_eq!(args.ws.port(), 9000);
        assert_eq!(args.encoder, EncoderKind::OpenH264);
        assert_eq!(args.ice.udp, None);
        assert_eq!(args.ice.tcp_base, Some("127.0.0.1:8790".parse().unwrap()));
        assert_eq!(args.allowed_origins, ["http://studio:1"]);
        assert!(parse(&["--help"]).unwrap().is_none());
        let media = cfg!(all(unix, feature = "gstreamer-media"));
        assert_eq!(parse(&["--srt", "127.0.0.1:9001"]).is_ok(), media);
        assert_eq!(parse(&["--encoder", "gstreamer"]).is_ok(), media);
        for bad in [
            &["--size", "641x360"][..],
            &["--size", "0x0"],
            &["--fps", "0"],
            &["--fps"],
            &["--bogus"],
            &["--encoder", "nvenc"],
            &["--srt", "127.0.0.1:0"],
            &["--webrtc-udp", "none"],
            &["--webrtc-tcp", "localhost"],
            // ICE-TCP ports 8788-8791 would include the default --ws port 8789.
            &["--webrtc-tcp", "127.0.0.1:8788"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
