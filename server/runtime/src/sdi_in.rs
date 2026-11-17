//! `chr-sdi-in`: the INPUT process of chr-server's GStreamer core
//! (`chr-server --pipeline gst`, #154, phase 1). It stands in for an SDI input
//! card: a video file at its own rate, looped, or a moving test pattern,
//! played in real time and handed to chr-server over a unixfd socket as
//! memfd frames stamped with their identity
//! ([`chr_runtime::gst_core::input`]). It writes no input ring; that is
//! `chr-sdi-sim`, the timer-paced loop's simulator.

use std::{error::Error, path::PathBuf, time::Duration};

use chr_runtime::{
    gst_core::input::{Input, Source},
    sdi::VideoMode,
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const USAGE: &str = "\
Usage: chr-sdi-in (--video <file> | --test-pattern <WxH> <fps>) [options]

The INPUT process of chr-server's GStreamer core (chr-server --pipeline gst):
plays a video file at the file's own rate, looped, or a moving test pattern,
in real time on the system clock, and hands every frame to chr-server over a
unixfd socket: RGBA8 in memfd memory, stamped with its frame_id and capture
time (VchaoFrame, stage source). A stand-in for an SDI input card; it writes
no input ring.

Options:
  --video <file>              Progressive video at an integer rate, decoded by
                              FFmpeg (nix develop .#media)
  --test-pattern <WxH> <fps>  GStreamer's moving test pattern instead
  --socket <path>             unixfd socket chr-server connects to
                              [default: $XDG_RUNTIME_DIR/chr/capture.sock]
  --queue <n>                 Frames that may wait for chr-server before the
                              oldest is dropped [default: 4]
  -h, --help                  Show this help";

#[derive(Debug, PartialEq)]
struct Args {
    source: Source,
    socket: PathBuf,
    queue: u32,
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Args>> {
    let (mut sources, mut socket, mut queue) = (Vec::new(), None, 4);
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--video" => sources.push(Source::File(value()?.into())),
            "--test-pattern" => {
                let size = value()?;
                let (width, height) = size
                    .split_once('x')
                    .ok_or("--test-pattern needs <WxH> <fps>")?;
                let mode = VideoMode {
                    width: width.parse()?,
                    height: height.parse()?,
                    fps: value()?.parse()?,
                };
                if mode.width == 0
                    || mode.height == 0
                    || !mode.width.is_multiple_of(2)
                    || !mode.height.is_multiple_of(2)
                    || !(1..=120).contains(&mode.fps)
                {
                    return Err("--test-pattern needs an even extent and 1-120 fps".into());
                }
                sources.push(Source::TestPattern(mode));
            }
            "--socket" => socket = Some(PathBuf::from(value()?)),
            "--queue" => {
                queue = value()?.parse()?;
                if queue == 0 {
                    return Err("--queue must be positive".into());
                }
            }
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
        }
    }
    let [source] = <[Source; 1]>::try_from(sources)
        .map_err(|_| "give one of --video <file> and --test-pattern <WxH> <fps>")?;
    Ok(Some(Args {
        source,
        socket: match socket {
            Some(socket) => socket,
            None => std::env::var_os("XDG_RUNTIME_DIR")
                .map(|dir| PathBuf::from(dir).join("chr").join("capture.sock"))
                .ok_or("XDG_RUNTIME_DIR is not set; pass --socket <path>")?,
        },
        queue,
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

fn run(args: Args) -> Result<()> {
    if let Some(parent) = args.socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut input = Input::start(&args.source, &args.socket, args.queue)?;
    log::info!(
        "simulated SDI input {}: {} into {} (stream {})",
        input.mode().name(),
        match &args.source {
            Source::File(path) => path.display().to_string(),
            Source::TestPattern(_) => "test pattern".to_owned(),
        },
        args.socket.display(),
        input.stream_id()
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut check = tokio::time::interval(Duration::from_millis(100));
        let mut report = tokio::time::interval(Duration::from_secs(10));
        let mut last = (0, 0);
        loop {
            tokio::select! {
                signal = tokio::signal::ctrl_c() => return signal.map_err(Into::into),
                _ = terminate.recv() => return Ok(()),
                _ = check.tick() => input.check()?,
                _ = report.tick() => {
                    let (stamped, sent) = input.counts();
                    log::info!(
                        "frames captured {stamped} (+{}), sent {sent} (+{})",
                        stamped - last.0,
                        sent - last.1
                    );
                    last = (stamped, sent);
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Args>> {
        parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn parses_a_source_and_rejects_bad_options() {
        let args = parse(&["--test-pattern", "64x36", "50", "--socket", "/x/c.sock"])
            .unwrap()
            .unwrap();
        assert_eq!(
            args,
            Args {
                source: Source::TestPattern(VideoMode {
                    width: 64,
                    height: 36,
                    fps: 50
                }),
                socket: "/x/c.sock".into(),
                queue: 4,
            }
        );
        let file = parse(&["--video", "a.mp4", "--socket", "/s", "--queue", "8"])
            .unwrap()
            .unwrap();
        assert_eq!((file.source, file.queue), (Source::File("a.mp4".into()), 8));
        assert!(parse(&["--help"]).unwrap().is_none());
        for bad in [
            &["--socket", "/s"][..],
            &[
                "--video",
                "a.mp4",
                "--test-pattern",
                "64x36",
                "50",
                "--socket",
                "/s",
            ],
            &["--test-pattern", "63x36", "50", "--socket", "/s"],
            &["--test-pattern", "64x36", "0", "--socket", "/s"],
            &["--test-pattern", "64", "50"],
            &["--video", "a.mp4", "--queue", "0", "--socket", "/s"],
            &["--bogus"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
