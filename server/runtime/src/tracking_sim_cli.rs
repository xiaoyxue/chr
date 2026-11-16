//! `chr-tracking-sim-cli`: a stand-in tracking worker for `chr-server
//! --tracking-worker`, in place of vchao's `pitch-analysis`.
//!
//! It speaks the same VchaoFrame contract (`chr_tracker::protocol`): it receives
//! every frame on `--input-socket` and returns it, the same memory, in order, on
//! `--output-socket` with `selection.pose = fresh` and the recorded `Ti` of a
//! calibrated clip: it replays, it does not estimate. That `Ti` maps to the
//! ideal image, so every frame also carries the clip's lens in chr's optional
//! VchaoFrame `lens` extension, and no frame warp. The replay itself is
//! `chr_tracker::replay`; this binary is its process: arguments, sockets and
//! fault injection. Options inject latency, unavailable poses, crashes and
//! hangs to exercise chr-server's supervision and missing-camera handling.

use std::{
    collections::VecDeque,
    error::Error,
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

use chr_runtime::{
    calibration::lens::CHECKS,
    worker::{
        tracking::wire::{Received, attach, check_caps, now_ns},
        transport::{self, Inbound, Receiver, Sender},
    },
};
use chr_tracker::replay::{self, TrackingReplay};
use gstreamer as gst;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const USAGE: &str = "\
Usage: chr-tracking-sim-cli --manifest <json> --stream-id <id> --input-socket <path>
                            --output-socket <path> [options]

Runs as chr-server's tracking worker (started by chr-server --tracking-worker with
--tracking-worker-arg --manifest --tracking-worker-arg <json>). It speaks vchao's
VchaoFrame contract over the two GStreamer unixfd sockets, like pitch-analysis:
it listens on --output-socket for its one consumer, connects to --input-socket,
and returns every frame of stream <id>, in order, with the recorded Ti of the
calibrated-video-v1 manifest as a fresh pose: a replay, not an estimate. Every
frame also carries the manifest's radial lens (chr's lens extension); the
manifest's per-frame warps are not sent.

Options (fault injection):
  --delay-ms <n>           Hold every frame this long before returning it, as a
                           tracker's lookahead would (frames keep flowing)
  --unavailable-every <n>  Return every n-th frame (by frame_id) as unavailable
  --exit-after <n>         Exit with status 3 on the n-th frame, after the frames
                           already returned were sent
  --hang-after <n>         Stop returning frames from the n-th on
  -h, --help               Show this help";

#[derive(Debug, PartialEq)]
struct Worker {
    manifest: PathBuf,
    stream_id: String,
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

fn parse_args(raw: impl Iterator<Item = String>) -> Result<Option<Worker>> {
    let raw: Vec<String> = raw.collect();
    if raw.is_empty() || raw.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Ok(None);
    }
    let (input, output, rest) = transport::parse_worker_args(raw)?;
    let (mut manifest, mut stream_id) = (None, None);
    let mut faults = Faults::default();
    let mut raw = rest.into_iter();
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(value()?)),
            "--stream-id" => stream_id = Some(value()?),
            "--delay-ms" => faults.delay = Duration::from_millis(value()?.parse()?),
            "--unavailable-every" => {
                faults.unavailable_every = Some(positive(&flag, Some(value()?))?)
            }
            "--exit-after" => faults.exit_after = Some(positive(&flag, Some(value()?))?),
            "--hang-after" => faults.hang_after = Some(positive(&flag, Some(value()?))?),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
        }
    }
    Ok(Some(Worker {
        manifest: manifest.ok_or("--manifest <json> is required")?,
        stream_id: stream_id
            .filter(|id| !id.is_empty())
            .ok_or("--stream-id <id> is required")?,
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
    match serve(&worker) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            log::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// How long an exiting worker waits for its last frames to reach the server.
const FINISH: Duration = Duration::from_secs(2);
/// Frames waiting for the consumer before the worker fails instead of growing.
const OUTPUT_QUEUE: u64 = 64;
/// How often the worker checks its input while it holds no frame.
const IDLE: Duration = Duration::from_millis(100);

fn serve(worker: &Worker) -> Result<()> {
    // The reference loads before the output socket exists, as pitch-analysis does.
    let replay = replay::load(&worker.manifest, &CHECKS)?;
    log::info!(
        "chr-tracking-sim-cli returning the recorded Ti and lens of {} for stream {} (a \
         replay; no estimation, no frame warps)",
        worker.manifest.display(),
        worker.stream_id
    );
    // As pitch-analysis: frames are returned only to the connected consumer.
    let output = Sender::listen_live(&worker.output, None, OUTPUT_QUEUE)?;
    let input = Receiver::connect(&worker.input)?;
    let result = forward(&input, &output, worker, &replay);
    output.finish(FINISH);
    result
}

/// Returns every input frame with its pose, in order, until the input ends.
fn forward(
    input: &Receiver,
    output: &Sender,
    worker: &Worker,
    replay: &TrackingReplay,
) -> Result<()> {
    let reader = input.reader();
    let faults = &worker.faults;
    let mut held: VecDeque<(Instant, gst::Buffer)> = VecDeque::new();
    let mut caps: Option<gst::Caps> = None;
    let mut received = 0_u64;
    let mut last_source_ns = None;
    loop {
        let timeout = held.front().map_or(IDLE, |(due, _)| {
            due.saturating_duration_since(Instant::now())
        });
        match reader.recv_sample_timeout(timeout) {
            Some(sample) => {
                received += 1;
                if faults.exit_after == Some(received) {
                    log::warn!("injected exit on frame {received}");
                    // Frames already returned reach the server before the exit.
                    output.finish(FINISH);
                    std::process::exit(3);
                }
                let frame = Received::decode(sample)?;
                // pitch-analysis' input rules: one stream, no gaps, fixed caps.
                replay::check_input(&frame.meta, &worker.stream_id, received - 1, last_source_ns)?;
                last_source_ns = Some(frame.meta.source_ns);
                let frame_caps = frame.caps.clone().ok_or("an input frame without caps")?;
                check_caps(&frame_caps, &worker.stream_id)?;
                match &caps {
                    None => {
                        output.set_caps(&frame_caps);
                        caps = Some(frame_caps);
                    }
                    Some(caps) if *caps != frame_caps => {
                        return Err("the input caps changed mid-stream".into());
                    }
                    Some(_) => {}
                }
                if faults.hang_after.is_some_and(|n| received >= n) {
                    if faults.hang_after == Some(received) {
                        log::warn!("injected hang from frame {received} on");
                    }
                    continue;
                }
                held.push_back((
                    Instant::now() + faults.delay,
                    select(frame, replay, faults)?,
                ));
            }
            None if reader.is_eos() => break,
            None => {
                if let Some(error) = input.error() {
                    return Err(format!("input lost: {error}").into());
                }
            }
        }
        while held.front().is_some_and(|(due, _)| *due <= Instant::now()) {
            let (_, buffer) = held.pop_front().expect("front exists");
            output.push(buffer)?;
        }
    }
    // The input ended: every accepted frame still goes out.
    for (_, buffer) in held {
        output.push(buffer)?;
    }
    Ok(())
}

/// The frame's buffer, the same memory, with its selected pose and the clip's lens.
fn select(frame: Received, replay: &TrackingReplay, faults: &Faults) -> Result<gst::Buffer> {
    let Received {
        mut meta,
        mut buffer,
        ..
    } = frame;
    let unavailable = faults
        .unavailable_every
        .is_some_and(|n| (meta.frame_id + 1) % n == 0);
    replay.select(&mut meta, now_ns(), unavailable);
    attach(buffer.make_mut(), &meta)?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Worker>> {
        parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    const REQUIRED: [&str; 8] = [
        "--manifest",
        "m.json",
        "--stream-id",
        "s",
        "--input-socket",
        "in",
        "--output-socket",
        "out",
    ];

    #[test]
    fn parses_fault_options() {
        let args = [
            &REQUIRED[..],
            &["--delay-ms", "5", "--unavailable-every", "4"],
        ]
        .concat();
        assert_eq!(
            parse(&args).unwrap().unwrap(),
            Worker {
                manifest: "m.json".into(),
                stream_id: "s".into(),
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
        assert!(parse(&[]).unwrap().is_none());
        for bad in [
            &["worker"][..],
            &REQUIRED[2..],
            &REQUIRED[..6],
            &[&REQUIRED[..2], &REQUIRED[4..]].concat(),
            &[&REQUIRED[..], &["--exit-after", "0"]].concat(),
            &[&REQUIRED[..], &["--hang-after"]].concat(),
            &[&REQUIRED[..], &["--bogus"]].concat(),
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
