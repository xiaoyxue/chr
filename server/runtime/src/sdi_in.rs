//! `chr-sdi-in`: the INPUT process of chr-server's GStreamer core
//! (`chr-server --pipeline gst`, #154). It stands in for an SDI input card: a
//! video file at its own rate, looped, or a moving test pattern, played in real
//! time and handed to chr-server over a unixfd socket as memfd frames stamped
//! with their identity ([`chr_runtime::gst_core::input`]). The test pattern can
//! change its format on a schedule (`--then`), as a signal that changes. It
//! writes no input ring; that is `chr-sdi-sim`, the timer-paced loop's
//! simulator.

use std::{error::Error, path::PathBuf, time::Duration};

use chr_common::rate::Rate;
use chr_runtime::{
    gst_core::input::{Input, Source},
    sdi::VideoMode,
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const USAGE: &str = "\
Usage: chr-sdi-in (--video <file> | --test-pattern <WxH> <rate>) [options]

The INPUT process of chr-server's GStreamer core (chr-server --pipeline gst):
plays a video file at the file's own rate, looped, or a moving test pattern,
in real time on the system clock, and hands every frame to chr-server over a
unixfd socket: RGBA8 in memfd memory, stamped with its frame_id and capture
time (VchaoFrame, stage source). A stand-in for an SDI input card; it writes
no input ring.

Options:
  --video <file>              Progressive video at its own rate, 1-120 fps, integer
                              or fractional, decoded by FFmpeg (nix develop .#media)
  --test-pattern <WxH> <rate> GStreamer's moving test pattern instead; the rate is
                              an integer, N/D (30000/1001) or 29.97, 59.94 ...
  --then <s> <WxH> <rate>     After <s> seconds in the previous format, switch the
                              test pattern to this one: a new stream (repeatable)
  --cycle                     Start the --then formats over after the last one
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
    /// Formats to switch to, each after the previous one played this long.
    then: Vec<(Duration, VideoMode)>,
    cycle: bool,
}

/// `<WxH> <rate>` of `flag`.
fn mode(flag: &str, raw: &mut impl Iterator<Item = String>) -> Result<VideoMode> {
    let mut value = || {
        raw.next()
            .ok_or_else(|| format!("{flag} needs <WxH> <rate>"))
    };
    let size = value()?;
    let (width, height) = size
        .split_once('x')
        .ok_or_else(|| format!("{flag} needs <WxH> <rate>"))?;
    let mode = VideoMode {
        width: width.parse()?,
        height: height.parse()?,
        fps: value()?.parse::<Rate>()?,
    };
    if mode.width == 0
        || mode.height == 0
        || !mode.width.is_multiple_of(2)
        || !mode.height.is_multiple_of(2)
    {
        return Err(format!("{flag} needs an even extent").into());
    }
    Ok(mode)
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Args>> {
    let (mut sources, mut socket, mut queue) = (Vec::new(), None, 4);
    let (mut then, mut cycle) = (Vec::new(), false);
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--video" => sources.push(Source::File(value()?.into())),
            "--test-pattern" => sources.push(Source::TestPattern(mode(&flag, &mut raw)?)),
            "--then" => {
                let seconds: f64 = value()?.parse()?;
                if !(seconds.is_finite() && seconds > 0.0) {
                    return Err("--then needs a positive number of seconds".into());
                }
                then.push((Duration::from_secs_f64(seconds), mode(&flag, &mut raw)?));
            }
            "--cycle" => cycle = true,
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
        .map_err(|_| "give one of --video <file> and --test-pattern <WxH> <rate>")?;
    if !then.is_empty() && !matches!(source, Source::TestPattern(_)) {
        return Err("--then switches the test pattern only".into());
    }
    if cycle && then.is_empty() {
        return Err("--cycle needs --then".into());
    }
    Ok(Some(Args {
        source,
        then,
        cycle,
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
    // The formats to come, after the first one's.
    let first = input.mode();
    let mut schedule = args.then.iter().copied().cycle().take(if args.cycle {
        usize::MAX
    } else {
        args.then.len()
    });
    let mut previous = first;
    runtime.block_on(async {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut check = tokio::time::interval(Duration::from_millis(100));
        let mut report = tokio::time::interval(Duration::from_secs(10));
        let mut last = (0, 0);
        let mut next = schedule.next();
        let switch = tokio::time::sleep(next.map_or(Duration::MAX / 4, |(after, _)| after));
        tokio::pin!(switch);
        loop {
            tokio::select! {
                signal = tokio::signal::ctrl_c() => return signal.map_err(Into::into),
                _ = terminate.recv() => return Ok(()),
                _ = &mut switch, if next.is_some() => {
                    let (_, mode) = next.expect("guarded");
                    log::info!("switching from {} to {}", previous.name(), mode.name());
                    input.switch(mode)?;
                    previous = mode;
                    next = schedule.next();
                    if let Some((after, _)) = next {
                        switch.as_mut().reset(tokio::time::Instant::now() + after);
                    }
                }
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
                    fps: Rate::fps(50)
                }),
                socket: "/x/c.sock".into(),
                queue: 4,
                then: Vec::new(),
                cycle: false,
            }
        );
        let switching = parse(&[
            "--test-pattern",
            "64x36",
            "25",
            "--then",
            "30",
            "64x36",
            "50",
            "--then",
            "0.5",
            "32x18",
            "29.97",
            "--cycle",
            "--socket",
            "/s",
        ])
        .unwrap()
        .unwrap();
        let ntsc = VideoMode {
            width: 32,
            height: 18,
            fps: Rate::new(30000, 1001).unwrap(),
        };
        assert_eq!(
            switching.then,
            [
                (
                    Duration::from_secs(30),
                    VideoMode {
                        width: 64,
                        height: 36,
                        fps: Rate::fps(50)
                    }
                ),
                (Duration::from_millis(500), ntsc)
            ]
        );
        assert!(switching.cycle);
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
            &[
                "--video", "a.mp4", "--then", "1", "64x36", "50", "--socket", "/s",
            ],
            &["--test-pattern", "64x36", "50", "--cycle", "--socket", "/s"],
            &[
                "--test-pattern",
                "64x36",
                "50",
                "--then",
                "0",
                "64x36",
                "25",
            ],
            &[
                "--test-pattern",
                "64x36",
                "50",
                "--then",
                "1",
                "64x36",
                "29.9",
            ],
            &["--bogus"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
