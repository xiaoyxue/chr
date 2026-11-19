//! `chr-sdi-in`: the INPUT process of chr-server's GStreamer core (#154), the
//! one capture process of a tracked program. It plays a video file at its own
//! rate, looped, or a moving test pattern, in real time, or captures a DeckLink
//! card's input, and hands the frames to chr-server over a unixfd socket as
//! memfd frames stamped with their identity, timecode and captions, and the
//! audio over a second socket ([`chr_runtime::gst_core::input`]). The test
//! pattern can change its format on a schedule (`--then`), as a signal that
//! changes, and any input can lose its signal for a while (`--then <s> loss`).
//!
//! With `--send-cameras <manifest>` it is also the external tracking source of
//! a chr-server without a tracking worker: each frame's recorded camera (the
//! calibrated clip's `Ti`, `C`, lens and the warp of the clip frame it shows)
//! goes to chr-server as `tracking.frame`, keyed by the frame's `frame_id`.
//! Nothing estimates it.

use std::{
    collections::VecDeque,
    error::Error,
    path::PathBuf,
    time::{Duration, Instant},
};

use chr_common::{client::NativeSocketClient, methods::tracking};
use chr_runtime::{
    calibration::lens::CHECKS,
    gst_core::{
        decklink::{self, Connection},
        input::{Input, Options, Source, Stamped, audio_socket, decoded_frames},
    },
    sdi::{Format, VideoMode, scanned_rate},
};
use chr_tracker::replay::TrackingReplay;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const USAGE: &str = "\
Usage: chr-sdi-in (--video <file> | --test-pattern <WxH> <rate> | --source decklink) [options]

The INPUT process of chr-server's GStreamer core (a tracked chr-server program):
plays a video file at the file's own rate, looped, or a moving test pattern,
in real time on the system clock, or captures a DeckLink card's input, and
hands every frame to chr-server over a unixfd socket: RGBA8 in memfd memory,
stamped with its frame_id and capture time (VchaoFrame, stage source), its
timecode and captions; the audio goes over <socket>.audio. With --send-cameras
it also sends each frame's recorded camera to chr-server as tracking.frame.

Rates are frames per second: an integer, N/D (30000/1001) or 29.97, 59.94 ...;
a rate with an i suffix is interlaced, named by its field rate as broadcast
names it: 50i is 25 interlaced frames (50 fields) per second, top field first.

Options:
  --video <file>              Video at its own rate, 1-120 fps, progressive or interlaced,
                              integer or fractional, decoded by FFmpeg with its first audio
                              stream and its timecode tag (nix develop .#media)
  --test-pattern <WxH> <rate> GStreamer's moving test pattern instead, with a tick each
                              second as audio
  --source decklink           A DeckLink card's input and embedded audio: --device, --mode,
                              --connection
  --device <n>                DeckLink device [default: 0]
  --mode <auto|mode>          DeckLink mode: auto takes the format from the signal, or a
                              mode such as 1080i50, 1080p5994 [default: auto]
  --connection <c>            DeckLink input: sdi, hdmi, optical-sdi or auto [default: sdi]
  --then <s> <WxH> <rate>     After <s> seconds in the previous state, switch the test
                              pattern to this format: a new stream (repeatable)
  --then <s> loss             After <s> seconds, lose the signal: no frames, no audio
  --then <s> resume           After <s> seconds of loss, deliver again
  --cycle                     Start the --then list over after its last entry
  --socket <path>             unixfd socket chr-server connects to
                              [default: $XDG_RUNTIME_DIR/chr/capture.sock]
  --queue <n>                 Frames that may wait for chr-server before the
                              oldest is dropped [default: 4]
  --no-audio                  Send no audio
  --audio-channels <n>        Audio channels: 2, 8 or 16 [default: 2]
  --captions                  Give the test pattern's frames synthetic CEA-608 captions
  --send-cameras <json>       Also send each frame's recorded camera from this
                              calibrated-video-v1 manifest (Ti, C, radial, frame_warps; the
                              clip's) to chr-server as tracking.frame, keyed by frame_id: the
                              external tracking of a chr-server without --tracking-worker
  --server-socket <path>      chr-server's NativeSocket for --send-cameras
                              [default: $XDG_RUNTIME_DIR/chr/render.sock]
  --tracking-delay-ms <n>     Send each camera this long after its frame [default: 0]
  --drop-tracking-every <n>   Withhold every n-th camera to exercise frames without one
  -h, --help                  Show this help";

/// One step of the `--then` schedule.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Switch(Format),
    Loss,
    Resume,
}

#[derive(Debug, PartialEq)]
struct Args {
    source: Source,
    socket: PathBuf,
    options: Options,
    /// Steps, each after the previous state lasted this long.
    then: Vec<(Duration, Step)>,
    cycle: bool,
    cameras: Option<Cameras>,
}

/// `--send-cameras`: the recorded cameras of a calibrated clip.
#[derive(Debug, PartialEq)]
struct Cameras {
    manifest: PathBuf,
    server: PathBuf,
    delay: Duration,
    drop_every: Option<u64>,
}

/// `<WxH> <rate>` of `flag`.
fn format(flag: &str, size: String, raw: &mut impl Iterator<Item = String>) -> Result<Format> {
    let (width, height) = size
        .split_once('x')
        .ok_or_else(|| format!("{flag} needs <WxH> <rate>"))?;
    let (fps, scan) = scanned_rate(
        &raw.next()
            .ok_or_else(|| format!("{flag} needs <WxH> <rate>"))?,
    )?;
    let format = Format {
        mode: VideoMode {
            width: width.parse()?,
            height: height.parse()?,
            fps,
        },
        scan,
    };
    let mode = format.mode;
    if mode.width == 0
        || mode.height == 0
        || !mode.width.is_multiple_of(2)
        || !mode.height.is_multiple_of(2)
    {
        return Err(format!("{flag} needs an even extent").into());
    }
    Ok(format)
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Args>> {
    let (mut sources, mut socket, mut options) = (Vec::new(), None, Options::default());
    let (mut then, mut cycle) = (Vec::new(), false);
    let (mut device, mut mode, mut connection) = (None, None, None);
    let (mut manifest, mut server, mut delay_ms, mut drop_every) = (None, None, None, None);
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--video" => sources.push(Some(Source::File(value()?.into()))),
            "--test-pattern" => {
                let size = value()?;
                sources.push(Some(Source::TestPattern(format(&flag, size, &mut raw)?)));
            }
            "--source" => match value()?.as_str() {
                "decklink" => sources.push(None),
                other => return Err(format!("--source is decklink, not {other:?}").into()),
            },
            "--device" => device = Some(value()?.parse()?),
            "--mode" => {
                mode = match value()?.as_str() {
                    "auto" => Some(None),
                    nick => Some(Some(nick.parse::<decklink::Mode>()?)),
                }
            }
            "--connection" => connection = Some(value()?.parse::<Connection>()?),
            "--then" => {
                let seconds: f64 = value()?.parse()?;
                if !(seconds.is_finite() && seconds > 0.0) {
                    return Err("--then needs a positive number of seconds".into());
                }
                let step = match raw.next().as_deref() {
                    Some("loss") => Step::Loss,
                    Some("resume") => Step::Resume,
                    Some(size) => Step::Switch(format(&flag, size.to_owned(), &mut raw)?),
                    None => return Err("--then needs <WxH> <rate>, loss or resume".into()),
                };
                then.push((Duration::from_secs_f64(seconds), step));
            }
            "--cycle" => cycle = true,
            "--socket" => socket = Some(PathBuf::from(value()?)),
            "--queue" => {
                options.queue = value()?.parse()?;
                if options.queue == 0 {
                    return Err("--queue must be positive".into());
                }
            }
            "--no-audio" => options.audio = false,
            "--audio-channels" => {
                options.audio_channels = value()?.parse()?;
                if ![2, 8, 16].contains(&options.audio_channels) {
                    return Err("--audio-channels is 2, 8 or 16".into());
                }
            }
            "--captions" => options.captions = true,
            "--send-cameras" => manifest = Some(PathBuf::from(value()?)),
            "--server-socket" => server = Some(PathBuf::from(value()?)),
            "--tracking-delay-ms" => delay_ms = Some(value()?.parse::<u64>()?),
            "--drop-tracking-every" => {
                drop_every = Some(value()?.parse::<u64>()?).filter(|n| *n > 0);
                if drop_every.is_none() {
                    return Err("--drop-tracking-every must be positive".into());
                }
            }
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
        }
    }
    let [source] = <[Option<Source>; 1]>::try_from(sources).map_err(
        |_| "give one of --video <file>, --test-pattern <WxH> <rate> and --source decklink",
    )?;
    let source = match source {
        Some(source) => {
            if device.is_some() || mode.is_some() || connection.is_some() {
                return Err("--device, --mode and --connection are for --source decklink".into());
            }
            source
        }
        None => Source::DeckLink(decklink::Input {
            device: device.unwrap_or(0),
            mode: mode.flatten(),
            connection: connection.unwrap_or_default(),
        }),
    };
    if then.iter().any(|(_, step)| matches!(step, Step::Switch(_)))
        && !matches!(source, Source::TestPattern(_))
    {
        return Err("--then switches the format of the test pattern only".into());
    }
    if cycle && then.is_empty() {
        return Err("--cycle needs --then".into());
    }
    if options.captions && !matches!(source, Source::TestPattern(_)) {
        return Err("--captions is for the test pattern; files and cards carry their own".into());
    }
    let runtime_dir = |flag: &str| {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(|dir| PathBuf::from(dir).join("chr"))
            .ok_or_else(|| format!("XDG_RUNTIME_DIR is not set; pass {flag} <path>"))
    };
    let cameras = match manifest {
        Some(manifest) => Some(Cameras {
            manifest,
            server: match server {
                Some(server) => server,
                None => runtime_dir("--server-socket")?.join("render.sock"),
            },
            delay: Duration::from_millis(delay_ms.unwrap_or(0)),
            drop_every,
        }),
        None if server.is_some() || delay_ms.is_some() || drop_every.is_some() => {
            return Err(
                "--server-socket, --tracking-delay-ms and --drop-tracking-every need \
                 --send-cameras"
                    .into(),
            );
        }
        None => None,
    };
    Ok(Some(Args {
        source,
        then,
        cycle,
        cameras,
        socket: match socket {
            Some(socket) => socket,
            None => runtime_dir("--socket")?.join("capture.sock"),
        },
        options,
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
    let mut cameras = match (&args.cameras, &args.source) {
        (Some(cameras), Source::File(video)) => Some(CameraSender::new(cameras, video)?),
        (Some(_), _) => return Err("--send-cameras replays a calibrated clip: give --video".into()),
        (None, _) => None,
    };
    let mut input = Input::start(&args.source, &args.socket, &args.options)?;
    if cameras.is_some() {
        input.watch_frames();
    }
    log::info!(
        "SDI input: {} into {}{}",
        match &args.source {
            Source::File(path) => path.display().to_string(),
            Source::TestPattern(format) => format!("test pattern {}", format.name()),
            Source::DeckLink(card) => format!(
                "DeckLink device {} ({})",
                card.device,
                card.mode.map_or("mode auto", |mode| mode.nick)
            ),
        },
        args.socket.display(),
        if args.options.audio {
            format!(", audio into {}", audio_socket(&args.socket).display())
        } else {
            String::new()
        }
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut schedule = args.then.iter().copied().cycle().take(if args.cycle {
        usize::MAX
    } else {
        args.then.len()
    });
    runtime.block_on(async {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut check = tokio::time::interval(Duration::from_millis(100));
        let mut poll = tokio::time::interval(Duration::from_millis(2));
        let mut report = tokio::time::interval(Duration::from_secs(10));
        let mut last = (0, 0, 0);
        let mut next = schedule.next();
        let step = tokio::time::sleep(next.map_or(Duration::MAX / 4, |(after, _)| after));
        tokio::pin!(step);
        loop {
            tokio::select! {
                signal = tokio::signal::ctrl_c() => return signal.map_err(Into::into),
                _ = terminate.recv() => return Ok(()),
                _ = &mut step, if next.is_some() => {
                    match next.expect("guarded").1 {
                        Step::Switch(format) => {
                            log::info!(
                                "switching from {} to {}",
                                input.format().map_or_else(|| "-".to_owned(), |f| f.name()),
                                format.name()
                            );
                            input.switch(format)?;
                        }
                        Step::Loss => {
                            log::info!("signal lost");
                            input.set_lost(true);
                        }
                        Step::Resume => {
                            log::info!("signal back");
                            input.set_lost(false);
                        }
                    }
                    next = schedule.next();
                    if let Some((after, _)) = next {
                        step.as_mut().reset(tokio::time::Instant::now() + after);
                    }
                }
                _ = check.tick() => input.check()?,
                _ = poll.tick(), if cameras.is_some() => {
                    cameras.as_mut().expect("guarded").send(&input).await?;
                }
                _ = report.tick() => {
                    let (stamped, sent, audio, stale) = input.counts();
                    log::info!(
                        "frames captured {stamped} (+{}), sent {sent} (+{}), audio buffers sent \
                         {audio} (+{}), stale frames dropped {stale}; stream {}",
                        stamped - last.0,
                        sent - last.1,
                        audio - last.2,
                        input.stream_id().unwrap_or_default()
                    );
                    last = (stamped, sent, audio);
                    if let Some(cameras) = &cameras {
                        cameras.report();
                    }
                }
            }
        }
    })
}

/// The recorded camera of the clip frame `stamped` shows (its warp of
/// `source_index % warps`), keyed by its `frame_id`: frames the input dropped
/// (lost, stale) are not stamped but still count in the clip.
fn camera(replay: &TrackingReplay, stamped: Stamped) -> tracking::TrackingFrame {
    let mut camera = replay.frame(stamped.source_index);
    camera.sequence = stamped.frame_id.into();
    camera
}

/// `--send-cameras`: sends each stamped frame's recorded camera to chr-server
/// as `tracking.frame`, `delay` after its frame, keyed by its `frame_id`.
struct CameraSender {
    replay: TrackingReplay,
    server: PathBuf,
    delay: Duration,
    drop_every: Option<u64>,
    client: Option<NativeSocketClient>,
    next_connect: Instant,
    due: VecDeque<(Instant, tracking::TrackingFrame)>,
    sent: u64,
    rejected: u64,
    withheld: u64,
}

/// Cameras waiting to be sent: stale ones are dropped, like a tracking link's.
const DUE_CAMERAS: usize = 64;

impl CameraSender {
    /// The cameras of the calibrated clip `video`, which must decode to one
    /// frame per warp of the manifest, at its extent.
    fn new(cameras: &Cameras, video: &std::path::Path) -> Result<Self> {
        #[derive(serde::Deserialize)]
        struct Extent {
            width: u32,
            height: u32,
        }
        let replay = chr_tracker::replay::load(&cameras.manifest, &CHECKS)?;
        let extent: Extent = serde_json::from_slice(&std::fs::read(&cameras.manifest)?)?;
        let (width, height, frames) = decoded_frames(video)?;
        if replay.warp_count() == 0
            || frames != replay.warp_count() as u64
            || (width, height) != (extent.width, extent.height)
        {
            return Err(format!(
                "--send-cameras needs the manifest's clip: {} decodes to {frames} frames of \
                 {width}x{height}, the manifest has {} frame_warps at {}x{}",
                video.display(),
                replay.warp_count(),
                extent.width,
                extent.height
            )
            .into());
        }
        log::info!(
            "sending the recorded cameras of {} to {} (+{} ms)",
            cameras.manifest.display(),
            cameras.server.display(),
            cameras.delay.as_millis()
        );
        Ok(Self {
            replay,
            server: cameras.server.clone(),
            delay: cameras.delay,
            drop_every: cameras.drop_every,
            client: None,
            next_connect: Instant::now(),
            due: VecDeque::new(),
            sent: 0,
            rejected: 0,
            withheld: 0,
        })
    }

    async fn send(&mut self, input: &Input) -> Result<()> {
        for stamped in input.take_frames() {
            if self
                .drop_every
                .is_some_and(|n| (stamped.frame_id + 1) % n == 0)
            {
                self.withheld += 1;
            } else {
                self.due
                    .push_back((Instant::now() + self.delay, camera(&self.replay, stamped)));
            }
        }
        while self.due.len() > DUE_CAMERAS {
            self.due.pop_front();
        }
        while self
            .due
            .front()
            .is_some_and(|(at, _)| *at <= Instant::now())
        {
            if self.client.is_none() && Instant::now() >= self.next_connect {
                match NativeSocketClient::connect(&self.server).await {
                    Ok(client) => {
                        log::info!("connected to chr-server at {}", self.server.display());
                        self.client = Some(client);
                    }
                    Err(error) => {
                        log::warn!(
                            "waiting for chr-server at {}: {error}",
                            self.server.display()
                        );
                        self.next_connect = Instant::now() + Duration::from_secs(1);
                    }
                }
            }
            let (_, camera) = self.due.pop_front().expect("front exists");
            let Some(client) = self.client.as_mut() else {
                continue;
            };
            match client
                .call(tracking::FRAME, Some(serde_json::to_value(&camera)?))
                .await
            {
                Ok(_) => self.sent += 1,
                Err(chr_common::client::ClientError::Rpc(error)) => {
                    self.rejected += 1;
                    log::debug!(
                        "camera {} rejected: {}",
                        camera.sequence.get(),
                        error.message
                    );
                }
                Err(error) => {
                    log::warn!("chr-server connection lost: {error}");
                    self.client = None;
                }
            }
        }
        Ok(())
    }

    fn report(&self) {
        log::info!(
            "cameras sent {}, rejected {}, withheld {}",
            self.sent,
            self.rejected,
            self.withheld
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chr_common::rate::Rate;
    use chr_runtime::sdi::{FieldOrder, Scan};

    fn parse(args: &[&str]) -> Result<Option<Args>> {
        parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    fn format(width: u32, height: u32, fps: Rate, scan: Scan) -> Format {
        Format {
            mode: VideoMode { width, height, fps },
            scan,
        }
    }

    #[test]
    fn cameras_follow_the_clip_frame_after_dropped_frames_and_loops() {
        let warp = |x: f64| [[0.001, 0., x], [0., 0.001, -0.5], [0., 0., 1.]];
        let replay = TrackingReplay::new(
            [[0.02, 0., 0.], [0., 0.02, 0.], [0., 0., 1.]],
            [0., 0., -10.],
            chr_tracker::lens::wire([2., 1.], 2., [-0.05, 0., 0.], None, &CHECKS).unwrap(),
            &[warp(-1.), warp(-0.9), warp(-0.8), warp(-0.7)],
            &CHECKS,
        )
        .unwrap();
        // Clip frames 2 and 3 were dropped (input lost), 6 is frame 2 of the next loop.
        let shown = [0, 1, 4, 5, 6, 7];
        for (frame_id, source_index) in (0..).zip(shown) {
            let camera = camera(
                &replay,
                Stamped {
                    frame_id,
                    source_index,
                },
            );
            assert_eq!(camera.sequence.get(), frame_id, "keyed by frame_id");
            assert_eq!(
                camera.frame_warp,
                replay.warp(source_index % 4),
                "frame {frame_id} shows clip frame {}",
                source_index % 4
            );
        }
        assert_ne!(
            replay.warp(2),
            replay.warp(0),
            "frame_id 2 shows clip frame 0, not 2"
        );
    }

    #[test]
    fn parses_a_source_and_rejects_bad_options() {
        let args = parse(&["--test-pattern", "64x36", "50", "--socket", "/x/c.sock"])
            .unwrap()
            .unwrap();
        assert_eq!(
            args,
            Args {
                source: Source::TestPattern(format(64, 36, Rate::fps(50), Scan::Progressive)),
                socket: "/x/c.sock".into(),
                options: Options::default(),
                then: Vec::new(),
                cycle: false,
                cameras: None,
            }
        );
        let cameras = parse(&[
            "--video",
            "a.mp4",
            "--socket",
            "/s",
            "--send-cameras",
            "m.json",
            "--server-socket",
            "/r.sock",
            "--tracking-delay-ms",
            "40",
            "--drop-tracking-every",
            "25",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            cameras.cameras,
            Some(Cameras {
                manifest: "m.json".into(),
                server: "/r.sock".into(),
                delay: Duration::from_millis(40),
                drop_every: Some(25),
            })
        );
        let switching = parse(&[
            "--test-pattern",
            "64x36",
            "25",
            "--then",
            "30",
            "64x36",
            "50i",
            "--then",
            "0.5",
            "32x18",
            "29.97",
            "--then",
            "2",
            "loss",
            "--then",
            "1",
            "resume",
            "--cycle",
            "--socket",
            "/s",
            "--captions",
        ])
        .unwrap()
        .unwrap();
        let tff = Scan::Interlaced(FieldOrder::TopFirst);
        assert_eq!(
            switching.then,
            [
                (
                    Duration::from_secs(30),
                    Step::Switch(format(64, 36, Rate::fps(25), tff))
                ),
                (
                    Duration::from_millis(500),
                    Step::Switch(format(
                        32,
                        18,
                        Rate::new(30000, 1001).unwrap(),
                        Scan::Progressive
                    ))
                ),
                (Duration::from_secs(2), Step::Loss),
                (Duration::from_secs(1), Step::Resume),
            ]
        );
        assert!(switching.cycle && switching.options.captions);
        // 59.94i is 29.97 interlaced frames per second.
        let ntsc = parse(&["--test-pattern", "1920x1080", "59.94i", "--socket", "/s"])
            .unwrap()
            .unwrap();
        assert_eq!(
            ntsc.source,
            Source::TestPattern(format(1920, 1080, Rate::new(30000, 1001).unwrap(), tff))
        );
        let file = parse(&[
            "--video",
            "a.mp4",
            "--socket",
            "/s",
            "--queue",
            "8",
            "--then",
            "5",
            "loss",
            "--no-audio",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            (file.source, file.options.queue, file.options.audio),
            (Source::File("a.mp4".into()), 8, false)
        );
        let card = parse(&[
            "--source",
            "decklink",
            "--device",
            "1",
            "--mode",
            "1080i50",
            "--socket",
            "/s",
            "--audio-channels",
            "8",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            card.source,
            Source::DeckLink(decklink::Input {
                device: 1,
                mode: Some("1080i50".parse().unwrap()),
                connection: Connection::Sdi,
            })
        );
        assert_eq!(card.options.audio_channels, 8);
        let auto = parse(&["--source", "decklink", "--mode", "auto", "--socket", "/s"])
            .unwrap()
            .unwrap();
        assert!(matches!(
            auto.source,
            Source::DeckLink(decklink::Input {
                device: 0,
                mode: None,
                ..
            })
        ));
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
            &["--test-pattern", "64x36", "300i", "--socket", "/s"],
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
            &["--test-pattern", "64x36", "50", "--then", "1"],
            &["--source", "v4l2", "--socket", "/s"],
            &[
                "--source", "decklink", "--mode", "1080i49", "--socket", "/s",
            ],
            &[
                "--source",
                "decklink",
                "--connection",
                "dvi",
                "--socket",
                "/s",
            ],
            &["--video", "a.mp4", "--device", "1", "--socket", "/s"],
            &["--video", "a.mp4", "--captions", "--socket", "/s"],
            &[
                "--video",
                "a.mp4",
                "--tracking-delay-ms",
                "5",
                "--socket",
                "/s",
            ],
            &[
                "--video",
                "a.mp4",
                "--send-cameras",
                "m.json",
                "--drop-tracking-every",
                "0",
                "--socket",
                "/s",
            ],
            &[
                "--test-pattern",
                "64x36",
                "50",
                "--audio-channels",
                "6",
                "--socket",
                "/s",
            ],
            &["--bogus"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
