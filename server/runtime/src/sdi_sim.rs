//! `chr-sdi-sim`: a simulated SDI capture input for a tracked `chr-server`
//! program (`--tracked-scene` or `--description`).
//!
//! It plays a calibrated clip in a loop the way a camera reaches the server: each
//! decoded frame goes into the shared-memory input ring (standing in for SDI in).
//! The server's tracking worker supplies the cameras. With `--send-cameras` it
//! also sends each frame's recorded camera, the manifest's `Ti`/`C` and lens with
//! the clip's warp of that frame, to `chr-server` as `tracking.frame` over the
//! NativeSocket (the external tracking mode). The two channels are independent;
//! the server pairs them by capture sequence. No physical SDI or tracking system
//! is involved; a real capture process (#80) takes its place.

use std::{error::Error, path::PathBuf};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const USAGE: &str = "\
Usage: chr-sdi-sim --video <mp4> --scene <json> [options]

Plays a calibrated clip in a loop as a simulated SDI capture input: frames into the
input ring. With --send-cameras also each frame's recorded camera to chr-server as
tracking.frame. Needs the gstreamer-media feature.

Options:
  --video <mp4>              Clip with the manifest's extent and rate, one warp per frame
  --scene <json>             calibrated-video-v1 manifest (Ti, C, radial, frame_warps)
  --socket <path>            chr-server NativeSocket [default: $XDG_RUNTIME_DIR/chr/render.sock]
  --input-ring <path>        Input ring to create [default: $XDG_RUNTIME_DIR/chr/input.ring]
  --send-cameras             Also send the recorded cameras as tracking.frame (for a
                             server started without --tracking-worker)
  --tracking-delay-ms <n>    Send each camera this long after its frame [default: 0]
  --drop-tracking-every <n>  Withhold every n-th camera to exercise frames without one
  -h, --help                 Show this help";

#[derive(Debug, PartialEq)]
struct Args {
    video: PathBuf,
    scene: PathBuf,
    socket: PathBuf,
    input_ring: PathBuf,
    delay_ms: u64,
    drop_every: Option<u64>,
    send_cameras: bool,
}

fn runtime_dir(name: &str) -> Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| format!("XDG_RUNTIME_DIR is not set; pass --{name} <path>"))?;
    Ok(PathBuf::from(dir).join("chr"))
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Args>> {
    let (mut video, mut scene, mut socket, mut input_ring) = (None, None, None, None);
    let (mut delay_ms, mut drop_every, mut send_cameras) = (0, None, false);
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--video" => video = Some(PathBuf::from(value()?)),
            "--scene" => scene = Some(PathBuf::from(value()?)),
            "--socket" => socket = Some(PathBuf::from(value()?)),
            "--input-ring" => input_ring = Some(PathBuf::from(value()?)),
            "--tracking-delay-ms" => delay_ms = value()?.parse()?,
            "--drop-tracking-every" => {
                drop_every = Some(value()?.parse::<u64>()?).filter(|n| *n > 0);
                if drop_every.is_none() {
                    return Err("--drop-tracking-every must be positive".into());
                }
            }
            "--send-cameras" => send_cameras = true,
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
        }
    }
    if !send_cameras && (delay_ms > 0 || drop_every.is_some()) {
        return Err("--tracking-delay-ms and --drop-tracking-every need --send-cameras".into());
    }
    Ok(Some(Args {
        video: video.ok_or("--video <mp4> is required")?,
        scene: scene.ok_or("--scene <json> is required")?,
        socket: match socket {
            Some(path) => path,
            None => runtime_dir("socket")?.join("render.sock"),
        },
        input_ring: match input_ring {
            Some(path) => path,
            None => runtime_dir("input-ring")?.join("input.ring"),
        },
        delay_ms,
        drop_every,
        send_cameras,
    }))
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

#[cfg(not(all(unix, feature = "gstreamer-media")))]
fn run(_args: Args) -> Result<()> {
    Err("chr-sdi-sim needs Linux/Unix and the gstreamer-media feature".into())
}

#[cfg(all(unix, feature = "gstreamer-media"))]
fn run(args: Args) -> Result<()> {
    use std::{
        collections::VecDeque,
        time::{Duration, Instant},
    };

    use chr_common::{client::NativeSocketClient, methods::tracking};
    use chr_runtime::{
        calibration::tracking::TrackingReplay, input::software::VideoInput,
        program::video::Manifest, sdi::VideoMode,
    };
    use chr_shm::{SlotHeader, Writer, input::SlotMeta};

    let manifest: Manifest = serde_json::from_slice(&std::fs::read(&args.scene)?)?;
    if manifest.frame_warps.is_empty() {
        return Err("the manifest has no frame_warps to replay".into());
    }
    let replay = TrackingReplay::new(
        manifest.ti,
        manifest.center,
        manifest.radial.wire()?,
        &manifest.frame_warps,
    )?;
    let mode = VideoMode {
        width: manifest.width,
        height: manifest.height,
        fps: manifest.fps,
    };
    let mut video = VideoInput::open(&args.video, mode, Some(replay.warp_count()))?;
    let mut ring = Writer::create(
        &args.input_ring,
        chr_shm::input::layout(mode.width, mode.height),
    )?;
    let meta = SlotMeta {
        width: mode.width,
        height: mode.height,
        fps: mode.fps,
    }
    .to_meta();
    if !args.send_cameras {
        log::info!(
            "simulating a {} SDI input: frames into {}; the server's tracking worker supplies cameras",
            mode.name(),
            args.input_ring.display()
        );
    } else {
        log::info!(
            "simulating a tracked {} camera: frames into {}, cameras to {} (+{} ms)",
            mode.name(),
            args.input_ring.display(),
            args.socket.display(),
            args.delay_ms
        );
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let delay = Duration::from_millis(args.delay_ms);
    let result = runtime.block_on(async {
        let mut client: Option<NativeSocketClient> = None;
        let mut next_connect = Instant::now();
        let mut due: VecDeque<(Instant, tracking::TrackingFrame)> = VecDeque::new();
        let (mut frames, mut sent, mut rejected, mut withheld) = (0_u64, 0_u64, 0_u64, 0_u64);
        let mut report = Instant::now();
        let mut poll = tokio::time::interval(Duration::from_millis(2));
        loop {
            tokio::select! {
                signal = tokio::signal::ctrl_c() => {
                    signal?;
                    log::info!("stopping");
                    return Ok::<(), Box<dyn Error + Send + Sync>>(());
                }
                _ = poll.tick() => {}
            }
            while let Some(frame) = video.try_frame()? {
                ring.push(
                    SlotHeader {
                        index: frame.sequence,
                        meta,
                    },
                    &frame.rgba,
                )?;
                frames += 1;
                if !args.send_cameras {
                    continue;
                }
                if args.drop_every.is_some_and(|n| (frame.sequence + 1) % n == 0) {
                    withheld += 1;
                } else {
                    due.push_back((Instant::now() + delay, replay.frame(frame.sequence)));
                }
            }
            // Cameras are bounded like a tracking link: stale ones are dropped, not queued.
            while due.len() > 64 {
                due.pop_front();
            }
            while due.front().is_some_and(|(at, _)| *at <= Instant::now()) {
                if client.is_none() && Instant::now() >= next_connect {
                    match NativeSocketClient::connect(&args.socket).await {
                        Ok(connected) => {
                            log::info!("connected to chr-server at {}", args.socket.display());
                            client = Some(connected);
                        }
                        Err(error) => {
                            log::warn!("waiting for chr-server at {}: {error}", args.socket.display());
                            next_connect = Instant::now() + Duration::from_secs(1);
                        }
                    }
                }
                let Some(connected) = client.as_mut() else {
                    due.pop_front();
                    continue;
                };
                let (_, camera) = due.pop_front().expect("front exists");
                match connected
                    .call(tracking::FRAME, Some(serde_json::to_value(&camera)?))
                    .await
                {
                    Ok(_) => sent += 1,
                    Err(chr_common::client::ClientError::Rpc(error)) => {
                        rejected += 1;
                        log::debug!("camera {} rejected: {}", camera.sequence.get(), error.message);
                    }
                    Err(error) => {
                        log::warn!("chr-server connection lost: {error}");
                        client = None;
                    }
                }
            }
            if report.elapsed() >= Duration::from_secs(5) {
                report = Instant::now();
                log::info!(
                    "frames written={frames} cameras sent={sent} rejected={rejected} withheld={withheld}"
                );
            }
        }
    });
    drop(video);
    let _ = std::fs::remove_file(&args.input_ring);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Args>> {
        parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn parses_options_and_rejects_bad_values() {
        let args = parse(&[
            "--video",
            "clip.mp4",
            "--scene",
            "scene.json",
            "--socket",
            "/tmp/s",
            "--input-ring",
            "/tmp/r",
            "--send-cameras",
            "--tracking-delay-ms",
            "40",
            "--drop-tracking-every",
            "10",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            args,
            Args {
                video: "clip.mp4".into(),
                scene: "scene.json".into(),
                socket: "/tmp/s".into(),
                input_ring: "/tmp/r".into(),
                delay_ms: 40,
                drop_every: Some(10),
                send_cameras: true,
            }
        );
        let camera_only = parse(&[
            "--video",
            "v.mp4",
            "--scene",
            "s.json",
            "--socket",
            "/tmp/s",
            "--input-ring",
            "/tmp/r",
        ])
        .unwrap()
        .unwrap();
        assert!(!camera_only.send_cameras, "frames only by default");
        assert!(
            parse(&[
                "--video",
                "v.mp4",
                "--scene",
                "s.json",
                "--tracking-delay-ms",
                "5"
            ])
            .is_err(),
            "a camera delay needs --send-cameras"
        );
        assert!(parse(&["--help"]).unwrap().is_none());
        for bad in [
            &["--scene", "scene.json"][..],
            &["--video", "clip.mp4"],
            &[
                "--video",
                "clip.mp4",
                "--scene",
                "s.json",
                "--send-cameras",
                "--drop-tracking-every",
                "0",
            ],
            &[
                "--video",
                "clip.mp4",
                "--scene",
                "s.json",
                "--send-cameras",
                "--tracking-delay-ms",
                "-1",
            ],
            &["--bogus"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
