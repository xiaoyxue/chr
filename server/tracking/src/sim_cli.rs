//! `chr-tracking-sim-cli`: a stand-in tracking worker for `chr-server
//! --tracking-worker`, to be replaced by the real tracking module.
//!
//! It speaks the tracking worker protocol (`chr_tracking::protocol`) and reads
//! every frame from the shared ring like a real tracker would, but answers with
//! the recorded camera of a calibrated clip (`chr_tracking::replay`): it replays,
//! it does not estimate. Options inject delays, unavailable results, crashes and
//! hangs to exercise chr-server's supervision and missing-camera handling.

use std::{
    error::Error,
    io::{self, BufReader},
    process::ExitCode,
    time::Duration,
};

use chr_shm::{Read, Reader};
use chr_tracking::{
    protocol::{self, Reply, Request, VERSION},
    replay::Replay,
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const USAGE: &str = "\
Usage: chr-tracking-sim-cli worker [options]

Runs as chr-server's tracking worker (started by chr-server --tracking-worker;
the protocol is JSON lines on stdin/stdout). Answers every frame with the
recorded camera of the calibrated-video-v1 manifest named in hello: a replay,
not an estimate.

Options (fault injection):
  --delay-ms <n>           Spend this long on every frame before answering
  --unavailable-every <n>  Answer every n-th frame (by sequence) as unavailable
  --exit-after <n>         Exit with status 3 on the n-th request
  --hang-after <n>         Stop answering from the n-th request on
  -h, --help               Show this help";

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

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Faults>> {
    match raw.next().as_deref() {
        Some("worker") => {}
        Some("-h" | "--help") | None => return Ok(None),
        Some(other) => return Err(format!("unknown command {other:?}\n\n{USAGE}").into()),
    }
    let mut faults = Faults::default();
    while let Some(flag) = raw.next() {
        match flag.as_str() {
            "--delay-ms" => {
                let value = raw.next().ok_or("--delay-ms needs a value")?;
                faults.delay = Duration::from_millis(value.parse()?);
            }
            "--unavailable-every" => faults.unavailable_every = Some(positive(&flag, raw.next())?),
            "--exit-after" => faults.exit_after = Some(positive(&flag, raw.next())?),
            "--hang-after" => faults.hang_after = Some(positive(&flag, raw.next())?),
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
        }
    }
    Ok(Some(faults))
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let faults = match parse_args(std::env::args().skip(1)) {
        Ok(Some(faults)) => faults,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };
    match serve(faults) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            log::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn serve(faults: Faults) -> Result<()> {
    let mut input = BufReader::new(io::stdin().lock());
    let mut output = io::stdout().lock();
    let Some(Request::Hello {
        version,
        input_ring,
        calibration,
        ..
    }) = protocol::read_line(&mut input)?
    else {
        return Err("expected hello".into());
    };
    let started = (|| -> Result<(Replay, Reader)> {
        if version != VERSION {
            return Err(format!("protocol version {version}, expected {VERSION}").into());
        }
        Ok((Replay::load(&calibration)?, Reader::open(&input_ring)?))
    })();
    let (replay, ring) = match started {
        Ok(started) => started,
        Err(error) => {
            protocol::write_line(
                &mut output,
                &Reply::Fatal {
                    message: error.to_string(),
                },
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
    protocol::write_line(&mut output, &Reply::Ready { name })?;
    let mut frame = Vec::new();
    let mut requests = 0_u64;
    while let Some(request) = protocol::read_line::<Request>(&mut input)? {
        let Request::Track { id, sequence, slot } = request else {
            return Err("unexpected hello".into());
        };
        requests += 1;
        if faults.exit_after == Some(requests) {
            log::warn!("injected exit on request {requests}");
            std::process::exit(3);
        }
        if faults.hang_after.is_some_and(|n| requests >= n) {
            log::warn!("injected hang on request {requests}");
            continue;
        }
        std::thread::sleep(faults.delay);
        // A real tracker estimates from these pixels; the replay only proves they arrived.
        let reply = match ring.read(slot, &mut frame)? {
            Read::Frame(header) if header.index == id => {
                if faults
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
                }
            }
            other => Reply::Unavailable {
                id,
                reason: format!("slot {slot} does not hold the frame: {other:?}"),
            },
        };
        protocol::write_line(&mut output, &reply)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Faults>> {
        parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn parses_fault_options() {
        assert_eq!(
            parse(&["worker", "--delay-ms", "5", "--unavailable-every", "4"])
                .unwrap()
                .unwrap(),
            Faults {
                delay: Duration::from_millis(5),
                unavailable_every: Some(4),
                ..Faults::default()
            }
        );
        assert!(parse(&["--help"]).unwrap().is_none());
        for bad in [
            &["render"][..],
            &["worker", "--exit-after", "0"],
            &["worker", "--hang-after"],
            &["worker", "--bogus"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
