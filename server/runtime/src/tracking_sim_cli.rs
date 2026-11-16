//! `chr-tracking-sim-cli`: a stand-in tracking worker for `chr-server
//! --tracking-worker`, to be replaced by the real tracking module.
//!
//! It speaks the tracking worker protocol (`chr_runtime::worker::tracking::protocol`)
//! and maps every frame it receives like a real tracker would, but
//! answers with the recorded camera of a calibrated clip (`TrackingReplay`): it
//! replays, it does not estimate. Options inject delays, unavailable results, crashes and
//! hangs to exercise chr-server's supervision and missing-camera handling.

use std::{error::Error, path::PathBuf, process::ExitCode, time::Duration};

use chr_runtime::{
    calibration::tracking::TrackingReplay,
    worker::{
        tracking::protocol::{Reply, Request, VERSION},
        transport::{self, Incoming, Receiver, Sender},
    },
};
use serde::Deserialize;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const USAGE: &str = "\
Usage: chr-tracking-sim-cli worker --input-socket <path> --output-socket <path> [options]

Runs as chr-server's tracking worker (started by chr-server --tracking-worker;
the protocol travels over the two GStreamer unixfd sockets chr-server names).
Answers every frame with the recorded camera of the calibrated-video-v1
manifest named in hello: a replay, not an estimate.

Options (fault injection):
  --delay-ms <n>           Spend this long on every frame before answering
  --unavailable-every <n>  Answer every n-th frame (by sequence) as unavailable
  --exit-after <n>         Exit with status 3 on the n-th request
  --hang-after <n>         Stop answering from the n-th request on
  -h, --help               Show this help";

#[derive(Debug, PartialEq)]
struct Worker {
    input: PathBuf,
    output: PathBuf,
    faults: Faults,
}

#[derive(Debug, Default, PartialEq)]
struct Faults {
    delay: Duration,
    unavailable_every: Option<u64>,
    exit_after: Option<u64>,
    hang_after: Option<u64>,
}

fn positive(flag: &str, value: Option<String>) -> Result<u64> {
    let value: u64 = value
        .ok_or_else(|| format!("{flag} needs a value"))?
        .parse()?;
    if value == 0 {
        return Err(format!("{flag} must be positive").into());
    }
    Ok(value)
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Worker>> {
    match raw.next().as_deref() {
        Some("worker") => {}
        Some("-h" | "--help") | None => return Ok(None),
        Some(other) => return Err(format!("unknown command {other:?}\n\n{USAGE}").into()),
    }
    let raw: Vec<String> = raw.collect();
    if raw.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Ok(None);
    }
    let (input, output, rest) = transport::parse_worker_args(raw)?;
    let mut faults = Faults::default();
    let mut raw = rest.into_iter();
    while let Some(flag) = raw.next() {
        match flag.as_str() {
            "--delay-ms" => {
                let value = raw.next().ok_or("--delay-ms needs a value")?;
                faults.delay = Duration::from_millis(value.parse()?);
            }
            "--unavailable-every" => faults.unavailable_every = Some(positive(&flag, raw.next())?),
            "--exit-after" => faults.exit_after = Some(positive(&flag, raw.next())?),
            "--hang-after" => faults.hang_after = Some(positive(&flag, raw.next())?),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
        }
    }
    Ok(Some(Worker {
        input,
        output,
        faults,
    }))
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let worker = match parse_args(std::env::args().skip(1)) {
        Ok(Some(worker)) => worker,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };
    match serve(worker) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            log::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// The fields of a `calibrated-video-v1` manifest the replay needs.
#[derive(Deserialize)]
struct Calibration {
    ti: [[f64; 3]; 3],
    center: [f64; 3],
    #[serde(default)]
    frame_warps: Vec<[[f64; 3]; 3]>,
}

fn load(path: &std::path::Path) -> Result<TrackingReplay> {
    let calibration: Calibration = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    TrackingReplay::new(calibration.ti, calibration.center, &calibration.frame_warps)
}

/// How long an exiting worker waits for its last replies, such as `fatal`, to
/// reach the server.
const FINISH: Duration = Duration::from_secs(2);

fn serve(worker: Worker) -> Result<()> {
    let (requests, replies) = transport::worker_endpoints(&worker.input, &worker.output)?;
    let result = serve_on(&requests, &replies, &worker.faults);
    replies.finish(FINISH);
    result
}

fn serve_on(requests: &Receiver, replies: &Sender, faults: &Faults) -> Result<()> {
    let Some(Incoming {
        message:
            Request::Hello {
                version,
                width,
                height,
                calibration,
                ..
            },
        ..
    }) = requests.recv()?
    else {
        return Err("expected hello".into());
    };
    let started = (|| -> Result<TrackingReplay> {
        if version != VERSION {
            return Err(format!("protocol version {version}, expected {VERSION}").into());
        }
        load(&calibration)
    })();
    let replay = match started {
        Ok(replay) => replay,
        Err(error) => {
            replies.send(
                &Reply::Fatal {
                    message: error.to_string(),
                },
                None,
            )?;
            return Err(error);
        }
    };
    let name = format!(
        "chr-tracking-sim-cli replaying the recorded cameras of {} ({} warps; no estimation)",
        calibration.display(),
        replay.warp_count()
    );
    log::info!("{name}");
    replies.send(&Reply::Ready { name }, None)?;
    let frame_bytes = width as usize * height as usize * 4;
    let mut requests_seen = 0_u64;
    while let Some(request) = requests.recv::<Request>()? {
        let Request::Track { id, sequence } = request.message else {
            return Err("unexpected hello".into());
        };
        requests_seen += 1;
        if faults.exit_after == Some(requests_seen) {
            log::warn!("injected exit on request {requests_seen}");
            std::process::exit(3);
        }
        if faults.hang_after.is_some_and(|n| requests_seen >= n) {
            log::warn!("injected hang on request {requests_seen}");
            continue;
        }
        std::thread::sleep(faults.delay);
        // A real tracker estimates from these pixels; the replay only checks they arrived.
        let frame = request.payload()?;
        let reply = if frame.len() != frame_bytes {
            Reply::Unavailable {
                id,
                reason: format!("a frame of {} bytes, not {frame_bytes}", frame.len()),
            }
        } else if faults
            .unavailable_every
            .is_some_and(|n| (sequence + 1) % n == 0)
        {
            Reply::Unavailable {
                id,
                reason: "injected by --unavailable-every".into(),
            }
        } else {
            Reply::Camera {
                id,
                camera: replay.frame(sequence),
            }
        };
        drop(frame);
        drop(request);
        replies.send(&reply, None)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Worker>> {
        parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    const SOCKETS: [&str; 4] = ["--input-socket", "in", "--output-socket", "out"];

    #[test]
    fn parses_fault_options() {
        let args = [
            &["worker"][..],
            &SOCKETS,
            &["--delay-ms", "5", "--unavailable-every", "4"],
        ]
        .concat();
        assert_eq!(
            parse(&args).unwrap().unwrap(),
            Worker {
                input: "in".into(),
                output: "out".into(),
                faults: Faults {
                    delay: Duration::from_millis(5),
                    unavailable_every: Some(4),
                    ..Faults::default()
                },
            }
        );
        assert!(parse(&["--help"]).unwrap().is_none());
        for bad in [
            &["render"][..],
            &["worker"],
            &["worker", "--input-socket", "in"],
            &[&["worker"][..], &SOCKETS, &["--exit-after", "0"]].concat(),
            &[&["worker"][..], &SOCKETS, &["--hang-after"]].concat(),
            &[&["worker"][..], &SOCKETS, &["--bogus"]].concat(),
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
