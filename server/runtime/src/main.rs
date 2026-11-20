//! Headless render server.
//!
//! A tracked program (`--tracked-scene`, `--description`) runs on the
//! GStreamer core (#154, `server/docs/gst-core.md`): one pipeline from the
//! frames `chr-sdi-in` sends over unixfd to the output, timed by the pipeline
//! clock, with every frame leaving the program delay after its capture, its CG
//! or its clean feed, and the program tap for `chr-monitor`. With
//! `--tracking-worker` the server hands every frame to a supervised tracking
//! worker over vchao's VchaoFrame contract (vchao's `pitch-analysis`, or the
//! `chr-tracking-sim-cli` replay stand-in) and takes its cameras; otherwise
//! cameras arrive as `tracking.frame` calls.
//!
//! A generated program has no capture input to follow: the triangle,
//! `--synthetic-program`, `--pitch-demo` and `--software-video` run on an
//! internal clock instead, a render loop paced by the monotonic clock and the
//! simulated SDI port:
//!
//! ```text
//! args (JSON-RPC) -> control -> pipeline: render -> SDI frame (UYVY)
//!                                  -> sdi: program output port (own clock)
//!                                       -> program tap (shared memory) -> chr-monitor
//! ```
//!
//! Control arrives over a NativeSocket (Unix Domain Socket) and a loopback WebSocket.
//! The renderer's only product is the SDI program output. Viewers watch it through the
//! separate `chr-monitor` process, which reads the program tap and never feeds back.

use std::{
    error::Error, net::SocketAddr, num::NonZeroU32, path::PathBuf, sync::Arc, time::Duration,
};

use chr_common::{
    rate::Rate,
    scene_frame as w,
    server::{IpcServer, ServerConfig, WebSocketListener},
};
use chr_renderer::coordinates::ImageExtent;

use chr_runtime::calibration::lens::CHECKS;
use chr_runtime::control::ControlHandler;
use chr_runtime::pipeline::{RenderConfig, RenderWorker};
use chr_runtime::program::spec::ProgramSpec;
use chr_runtime::sdi::{self, SdiOutput, Taps, VideoMode};
use chr_runtime::worker::tracking::supervisor::{Tracker, TrackerConfig, in_flight_limit};
use chr_runtime::worker::{
    protocol::{Output, WorkerProgram},
    supervisor::{WorkerAssets, WorkerConfig, status_handle},
};

use chr_tracker::{
    join::{MAX_WAIT, TrackingFeed},
    lens::{self, LensSelection},
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// Tracking samples held ahead of their frames.
const TRACKING_BACKLOG: usize = 64;

const USAGE: &str = "\
Usage: chr-server [options]

A tracked program (--tracked-scene, --description) runs on the GStreamer core: frames
from chr-sdi-in over unixfd, each leaving --program-delay-ms after its capture with its
CG or its clean feed, the output timed by the pipeline clock. Without one, a generated
program (the RGB triangle, --synthetic-program, --pitch-demo, --software-video) runs on
an internal clock with the simulated SDI port.

Options:
  --ws <addr>             Loopback WebSocket address [default: 127.0.0.1:8787]
  --socket <path>         NativeSocket path [default: $XDG_RUNTIME_DIR/chr/render.sock]
  --no-socket             Do not listen on a NativeSocket
  --size <WxH>            Output size; both even, up to 4096x2160 and with the camera's
                          aspect for a camera program [default: 1280x720]
  --fps <rate>            Frames per second, 1-120: an integer, N/D (30000/1001) or one of
                          23.976, 29.97, 47.95, 59.94, 119.88 [default: 50]; frames, not
                          fields (25 for 1080i50). For a tracked program the nominal rate:
                          the input's caps set the rate
  --synthetic-program    Opt in to synthetic-program-v1 scene/image output (max 1280x720)
  --software-video <mp4> Loop a local MP4 through FFmpeg/GStreamer and simulated SDI
                          (Linux gstreamer-media feature; --fps must match source)
  --video-scene <json>   Local calibrated-video-v1 scene/lens manifest (requires --software-video)
  --tracked-scene <json> The same manifest as tracked-video-v1: frames come from chr-sdi-in
                          and each frame's camera from the tracking worker or tracking.frame
                          (chr-sdi-in --send-cameras)
  --description <json>    A chr-render-description-v1 scene and lens, with the same tracked
                          input as --tracked-scene
  --asset-root <dir>      Folder that description.load may read assets from (repeatable;
                          requires --description)
  --capture-socket <path> The unixfd socket of chr-sdi-in (tracked program)
                          [default: $XDG_RUNTIME_DIR/chr/capture.sock]
  --program-delay-ms <n>  The program delay D of a tracked program: capture to output,
                          every frame; a frame without CG by then is its clean feed; above
                          4 and at most 1500 [default: 3 frame periods]
  --tracking-worker       Take cameras from a supervised tracking worker that gets every
                          captured frame over vchao's VchaoFrame unixfd contract, instead
                          of from tracking.frame (requires --tracked-scene or --description)
  --tracking-cli <path>   Tracking worker executable, run as `<path> <worker args>...
                          --stream-id <id> --input-socket <path> --output-socket <path>`
                          [default: chr-tracking-sim-cli next to chr-server, a replay]
  --tracking-worker-arg <arg> Pass one argument to the tracking worker (repeatable);
                          {stream_id} in it is replaced by each worker session's stream ID
  --tracking-threads <n>  Set OMP_NUM_THREADS=<n> for the tracking worker alone, capping
                          the OpenMP/PyTorch CPU threads of a tracker such as vchao's
                          [default: inherited]
  --tracking-centre <x> <y> <z> Station optical centre of every tracked camera, world
                          metres with negative Z up [default: the reference camera of
                          the manifest or description]
  --tracking-lens <auto|pinhole> Camera model of tracked frames: auto takes the lens the
                          tracking data carries, else the --tracking-lens-k/-centre/-scale
                          lens, else none (pinhole); pinhole ignores every lens. The
                          description's or manifest's lens is never applied [default: auto]
  --tracking-lens-k <k1> <k2> <k3> radial-poly-native-px-v1 coefficients of the CLI lens
                          (with --tracking-lens-centre and --tracking-lens-scale)
  --tracking-lens-centre <x> <y> Its centre, native pixels
  --tracking-lens-scale <px> Its normalisation scale, native pixels, positive
  --tracking-lens-support <min_x> <min_y> <max_x> <max_y> Its validated ideal-pixel
                          rectangle [default: none]
  --output-rate <follow|fixed:<rate>> The program output's rate (tracked program): the
                          input's rate and scan, changing with them (a DeckLink output
                          opens in the input's format at start and keeps it), or a fixed
                          rate such as fixed:50 (progressive) or fixed:50i (1080i50: 25
                          interlaced frames), repeating or dropping input frames
                          [default: follow]
  --input-loss <hold|black|slate[:<png>]> What the output shows while the input delivers
                          no frames (tracked program): the last frame, black, colour bars
                          or a PNG scaled to the output [default: hold]
  --output <sim|decklink> The program output (tracked program): a simulated sink on the
                          system clock, or a DeckLink card, whose clock (genlocked to its
                          reference input) times the pipeline; HD and UHD modes. The card
                          opens in the input's format: chr-server waits for chr-sdi-in's
                          first frame (the input must have the --size extent), unless
                          --output-rate fixed:<rate> sets another
                          [default: sim]
  --device <n>            The DeckLink output device (--output decklink) [default: 0]
  --audio-channels <n>    Audio channels of the DeckLink output: 2, 8 or 16 [default: 2]
  --no-audio              Do not pass the input's audio through (tracked program)
  --record <dir>          Record the captured input (as chr-sdi-in delivered it) and the
                          program (as it goes to the output) into a new folder
                          <dir>/<UTC time>/ with the supervised chr-recorder-cli worker:
                          H.264 MPEG-TS segments with a JSONL sidecar of their frames, a
                          new segment for each format; never delays the output, drops
                          instead (tracked program)
  --record-streams <input|program|input,program> What --record records [default: both]
  --record-fps <rate>     Record at most this many frames per second, on a cadence of the
                          frames' own times (10, 25, 29.97, ...) [default: every frame]
  --recorder-cli <path>   chr-recorder-cli executable [default: next to chr-server]
  --pitch-demo <json>     Opt in to owner-local pitch-demo-v1 manifest
  --pitch-controls <json> Write local harness metadata (requires --pitch-demo; owner-only)
  --monitor-tap <path>    Shared-memory program tap for chr-monitor; \"none\" disables it
                          [default: $XDG_RUNTIME_DIR/chr/program.tap]
  --renderer-cli <path>   chr-renderer-cli executable that runs the render worker
                          [default: next to chr-server]
  --render-deadline-ms <n> Worker time per frame before it is sent as clean feed, for a
                          generated program [default: 3/4 of the frame period]
  --worker-hang-ms <n>    Replace a worker whose oldest request is older [default: 2000]
  --allow-origin <origin> Extra browser origin for the WebSocket (repeatable); loopback
                          origins are always allowed
  -h, --help              Show this help";

#[derive(Debug)]
struct Args {
    ws: SocketAddr,
    socket: Option<PathBuf>,
    width: u32,
    height: u32,
    fps: Rate,
    monitor_tap: Option<PathBuf>,
    renderer_cli: Option<PathBuf>,
    render_deadline_ms: Option<u64>,
    worker_hang_ms: u64,
    allowed_origins: Vec<String>,
    synthetic_program: bool,
    pitch_demo: Option<PathBuf>,
    pitch_controls: Option<PathBuf>,
    software_video: Option<PathBuf>,
    video_scene: Option<PathBuf>,
    tracked_scene: Option<PathBuf>,
    description: Option<PathBuf>,
    asset_roots: Vec<PathBuf>,
    tracking_worker: bool,
    tracking_cli: Option<PathBuf>,
    tracking_worker_args: Vec<String>,
    tracking_threads: Option<NonZeroU32>,
    tracking_centre: Option<[f64; 3]>,
    /// `--tracking-lens pinhole`.
    tracking_pinhole: bool,
    /// The validated `--tracking-lens-*` lens.
    tracking_lens: Option<w::RadialLens>,
    capture_socket: Option<PathBuf>,
    program_delay_ms: Option<u64>,
    output_rate: Option<chr_runtime::gst_core::OutputRate>,
    input_loss: Option<chr_runtime::gst_core::InputLoss>,
    /// `--output decklink --device <n>`.
    decklink: Option<u32>,
    output_decklink: bool,
    no_audio: bool,
    audio_channels: Option<u32>,
    record: Option<PathBuf>,
    record_streams: Option<Vec<chr_runtime::worker::recording::Stream>>,
    record_fps: Option<Rate>,
    recorder_cli: Option<PathBuf>,
}

/// Reads `N` finite numbers that follow `flag`.
fn numbers<const N: usize>(flag: &str, raw: &mut impl Iterator<Item = String>) -> Result<[f64; N]> {
    let mut values = [0.0_f64; N];
    for value in &mut values {
        *value = raw
            .next()
            .ok_or_else(|| format!("{flag} needs {N} numbers"))?
            .parse()?;
        if !value.is_finite() {
            return Err(format!("{flag} must be finite").into());
        }
    }
    Ok(values)
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Args>> {
    let mut args = Args {
        ws: "127.0.0.1:8787".parse()?,
        socket: None,
        width: 1280,
        height: 720,
        fps: Rate::fps(50),
        monitor_tap: None,
        renderer_cli: None,
        render_deadline_ms: None,
        worker_hang_ms: 2000,
        allowed_origins: Vec::new(),
        synthetic_program: false,
        pitch_demo: None,
        pitch_controls: None,
        software_video: None,
        video_scene: None,
        tracked_scene: None,
        description: None,
        asset_roots: Vec::new(),
        tracking_worker: false,
        tracking_cli: None,
        tracking_worker_args: Vec::new(),
        tracking_threads: None,
        tracking_centre: None,
        tracking_pinhole: false,
        tracking_lens: None,
        capture_socket: None,
        program_delay_ms: None,
        output_rate: None,
        input_loss: None,
        decklink: None,
        output_decklink: false,
        no_audio: false,
        audio_channels: None,
        record: None,
        record_streams: None,
        record_fps: None,
        recorder_cli: None,
    };
    let mut lens_given = false;
    let (mut lens_k, mut lens_centre, mut lens_scale, mut lens_support) = (None, None, None, None);
    let mut use_socket = true;
    let mut use_tap = true;
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
            "--software-video" => args.software_video = Some(value()?.into()),
            "--video-scene" => args.video_scene = Some(value()?.into()),
            "--tracked-scene" => args.tracked_scene = Some(value()?.into()),
            "--description" => args.description = Some(value()?.into()),
            "--asset-root" => args.asset_roots.push(value()?.into()),
            "--input-ring" => {
                return Err(
                    "--input-ring was removed with the capture input ring (#154): \
                            chr-sdi-in sends the frames to --capture-socket"
                        .into(),
                );
            }
            "--tracking-wait-ms" => {
                return Err(
                    "--tracking-wait-ms was removed (#154): --program-delay-ms is \
                            the delay of every tracked frame"
                        .into(),
                );
            }
            "--pipeline" => {
                return Err(
                    "--pipeline was removed (#154): a tracked program always runs on \
                            the GStreamer core, a generated one on the internal clock"
                        .into(),
                );
            }
            "--tracking-worker" => args.tracking_worker = true,
            "--tracking-cli" => args.tracking_cli = Some(value()?.into()),
            "--tracking-worker-arg" => args.tracking_worker_args.push(value()?),
            "--tracking-threads" => {
                args.tracking_threads = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--tracking-threads must be a positive integer")?,
                )
            }
            "--tracking-centre" => args.tracking_centre = Some(numbers(&flag, &mut raw)?),
            "--tracking-lens" => {
                lens_given = true;
                args.tracking_pinhole = match value()?.as_str() {
                    "auto" => false,
                    "pinhole" => true,
                    other => {
                        return Err(format!(
                            "--tracking-lens must be auto or pinhole, not {other:?}"
                        )
                        .into());
                    }
                }
            }
            "--tracking-lens-k" => lens_k = Some(numbers::<3>(&flag, &mut raw)?),
            "--tracking-lens-centre" => lens_centre = Some(numbers::<2>(&flag, &mut raw)?),
            "--tracking-lens-scale" => lens_scale = Some(numbers::<1>(&flag, &mut raw)?[0]),
            "--tracking-lens-support" => {
                let [x0, y0, x1, y1] = numbers(&flag, &mut raw)?;
                lens_support = Some([[x0, y0], [x1, y1]]);
            }
            "--capture-socket" => args.capture_socket = Some(value()?.into()),
            "--program-delay-ms" => args.program_delay_ms = Some(value()?.parse()?),
            "--output-rate" => args.output_rate = Some(value()?.parse()?),
            "--input-loss" => args.input_loss = Some(value()?.parse()?),
            "--output" => {
                args.output_decklink = match value()?.as_str() {
                    "sim" => false,
                    "decklink" => true,
                    other => {
                        return Err(
                            format!("--output must be sim or decklink, not {other:?}").into()
                        );
                    }
                }
            }
            "--device" => args.decklink = Some(value()?.parse()?),
            "--no-audio" => args.no_audio = true,
            "--audio-channels" => {
                let channels = value()?.parse()?;
                if ![2, 8, 16].contains(&channels) {
                    return Err("--audio-channels is 2, 8 or 16".into());
                }
                args.audio_channels = Some(channels);
            }
            "--record" => args.record = Some(value()?.into()),
            "--record-streams" => {
                args.record_streams =
                    Some(chr_runtime::worker::recording::parse_streams(&value()?)?)
            }
            "--record-fps" => args.record_fps = Some(value()?.parse()?),
            "--recorder-cli" => args.recorder_cli = Some(value()?.into()),
            "--pitch-demo" => args.pitch_demo = Some(value()?.into()),
            "--pitch-controls" => args.pitch_controls = Some(value()?.into()),
            "--renderer-cli" => args.renderer_cli = Some(value()?.into()),
            "--render-deadline-ms" => args.render_deadline_ms = Some(value()?.parse()?),
            "--worker-hang-ms" => args.worker_hang_ms = value()?.parse()?,
            "--monitor-tap" => match value()?.as_str() {
                "none" => use_tap = false,
                path => (use_tap, args.monitor_tap) = (true, Some(path.into())),
            },
            "--allow-origin" => args.allowed_origins.push(value()?),
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
        }
    }
    if !args.width.is_multiple_of(2) || !args.height.is_multiple_of(2) {
        return Err("UYVY output and 4:2:0 monitoring need an even width and height".into());
    }
    if usize::from(args.synthetic_program)
        + usize::from(args.pitch_demo.is_some())
        + usize::from(args.software_video.is_some())
        + usize::from(args.tracked_scene.is_some())
        + usize::from(args.description.is_some())
        > 1
        || (args.pitch_controls.is_some() && args.pitch_demo.is_none())
    {
        return Err("select one program profile; --pitch-controls requires --pitch-demo".into());
    }
    if args.video_scene.is_some() && args.software_video.is_none() {
        return Err("--video-scene requires --software-video".into());
    }
    let tracked = args.tracked_scene.is_some() || args.description.is_some();
    if args.tracking_worker && !tracked {
        return Err("--tracking-worker requires --tracked-scene or --description".into());
    }
    if (args.tracking_cli.is_some()
        || !args.tracking_worker_args.is_empty()
        || args.tracking_threads.is_some()
        || args.tracking_centre.is_some())
        && !args.tracking_worker
    {
        return Err(
            "--tracking-cli, --tracking-worker-arg, --tracking-threads and \
                    --tracking-centre require --tracking-worker"
                .into(),
        );
    }
    args.tracking_lens = match (lens_k, lens_centre, lens_scale) {
        (Some(k), Some(centre), Some(scale)) => Some(
            lens::wire(centre, scale, k, lens_support, &CHECKS)
                .map_err(|error| format!("invalid --tracking-lens-* lens: {error}"))?,
        ),
        (None, None, None) if lens_support.is_none() => None,
        _ => {
            return Err(
                "--tracking-lens-k, --tracking-lens-centre and --tracking-lens-scale \
                        go together; --tracking-lens-support needs them"
                    .into(),
            );
        }
    };
    if (lens_given || args.tracking_lens.is_some()) && !tracked {
        return Err(
            "--tracking-lens and --tracking-lens-* require --tracked-scene or --description".into(),
        );
    }
    if !args.asset_roots.is_empty() && args.description.is_none() {
        return Err("--asset-root requires --description".into());
    }
    if tracked {
        if args.render_deadline_ms.is_some() {
            return Err(
                "--render-deadline-ms is for a generated program; a tracked program's frames \
                 have the program delay (--program-delay-ms)"
                    .into(),
            );
        }
        if args.program_delay_ms.is_some_and(|ms| {
            let delay = Duration::from_millis(ms);
            delay > MAX_WAIT || delay <= chr_runtime::gst_core::DECISION_MARGIN
        }) {
            return Err(format!(
                "--program-delay-ms must be above {:?} and at most {} ms",
                chr_runtime::gst_core::DECISION_MARGIN,
                MAX_WAIT.as_millis()
            )
            .into());
        }
        if args.capture_socket.is_none() {
            args.capture_socket = Some(
                std::env::var_os("XDG_RUNTIME_DIR")
                    .map(|dir| PathBuf::from(dir).join("chr").join("capture.sock"))
                    .ok_or("XDG_RUNTIME_DIR is not set; pass --capture-socket <path>")?,
            );
        }
        match (args.output_decklink, args.decklink) {
            (true, None) => args.decklink = Some(0),
            (false, Some(_)) => return Err("--device requires --output decklink".into()),
            _ => {}
        }
        if args.audio_channels.is_some() && !args.output_decklink {
            return Err("--audio-channels requires --output decklink".into());
        }
    } else if args.capture_socket.is_some()
        || args.program_delay_ms.is_some()
        || args.output_rate.is_some()
        || args.input_loss.is_some()
        || args.output_decklink
        || args.decklink.is_some()
        || args.no_audio
        || args.audio_channels.is_some()
    {
        return Err(
            "--capture-socket, --program-delay-ms, --output-rate, --input-loss, --output, \
             --device, --audio-channels and --no-audio require --tracked-scene or --description"
                .into(),
        );
    }
    if (args.record_streams.is_some() || args.record_fps.is_some() || args.recorder_cli.is_some())
        && args.record.is_none()
    {
        return Err("--record-streams, --record-fps and --recorder-cli require --record".into());
    }
    if args.record.is_some() && !tracked {
        return Err(
            "--record records a tracked program's input and output: it requires \
             --tracked-scene or --description"
                .into(),
        );
    }
    if args.render_deadline_ms == Some(0) || args.worker_hang_ms == 0 {
        return Err("--render-deadline-ms and --worker-hang-ms must be positive".into());
    }
    if use_socket && args.socket.is_none() {
        args.socket = Some(default_socket_path()?);
    }
    if !use_socket {
        args.socket = None;
    }
    if !use_tap {
        args.monitor_tap = None;
    } else if args.monitor_tap.is_none() {
        // The program never needs the monitor, so a missing default only disables it.
        args.monitor_tap = chr_shm::tap::default_path();
        if args.monitor_tap.is_none() {
            log::warn!(
                "XDG_RUNTIME_DIR is not set: no program tap for chr-monitor; pass --monitor-tap <path>"
            );
        }
    }
    Ok(Some(args))
}

/// A private directory for this server's render worker sockets and asset copies.
fn worker_dir() -> PathBuf {
    private_dir("worker")
}

/// A private runtime directory named after `kind` and this server's PID.
fn private_dir(kind: &str) -> PathBuf {
    let name = format!("{kind}-{}", std::process::id());
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) => PathBuf::from(runtime).join("chr").join(name),
        None => std::env::temp_dir().join(format!("chr-{name}")),
    }
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
    // Fail at once, not on every worker start, if GStreamer cannot carry the workers.
    chr_runtime::worker::transport::check()?;
    remove_stale_private_dirs();
    if args.tracked_scene.is_some() || args.description.is_some() {
        return run_core(args);
    }
    run_generated(args)
}

/// A generated program on the internal clock: the render loop and the
/// simulated SDI port pace themselves, as there is no capture input to follow.
fn run_generated(args: Args) -> Result<()> {
    if args.software_video.is_some() && !cfg!(all(unix, feature = "gstreamer-media")) {
        return Err("--software-video requires Linux/Unix and the gstreamer-media feature".into());
    }
    ImageExtent::new(args.width, args.height)?;
    let mode = VideoMode {
        width: args.width,
        height: args.height,
        fps: args.fps,
    };
    let spec = if let Some(path) = &args.pitch_demo {
        Some(ProgramSpec::PitchDemo { path: path.clone() })
    } else if let Some(path) = &args.video_scene {
        Some(ProgramSpec::VideoScene { path: path.clone() })
    } else if args.software_video.is_some() {
        Some(ProgramSpec::SoftwareVideo)
    } else if args.synthetic_program {
        Some(ProgramSpec::Synthetic)
    } else {
        None
    };
    let installation = match &spec {
        Some(spec) => {
            let installed = spec.install(mode, chr_runtime::program::synthetic::runtime_id()?)?;
            if let (Some(path), Some(controls)) = (&args.pitch_controls, &installed.controls) {
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                serde_json::to_writer_pretty(options.open(path)?, controls)?;
            }
            Some((installed.program, installed.adapter))
        }
        None => None,
    };
    #[cfg(all(unix, feature = "gstreamer-media"))]
    let mut video_source = args
        .software_video
        .as_ref()
        .map(|path| {
            let program = &installation
                .as_ref()
                .expect("software video installation")
                .0;
            let native = program.configuration.native_extent;
            chr_runtime::input::software::VideoInput::open(
                path,
                VideoMode {
                    width: native.width,
                    height: native.height,
                    fps: mode.fps,
                },
                program.video_lens.as_ref().map(|lens| lens.frame_count()),
            )
        })
        .transpose()?;
    let taps = match &args.monitor_tap {
        Some(path) => {
            let taps = Taps::ring(mode, path)?;
            log::info!("program tap for chr-monitor at {}", path.display());
            taps
        }
        None => Taps::default(),
    };
    let sdi = if let Some((synthetic, _)) = &installation {
        SdiOutput::spawn_program(
            mode,
            chr_runtime::program::synthetic::id(format!(
                "{}-program",
                synthetic.configuration.runtime_id.as_str()
            ))?,
            taps,
        )?
    } else {
        SdiOutput::spawn(mode, taps)?
    };
    let status = status_handle();
    let worker_config = WorkerConfig {
        executable: match &args.renderer_cli {
            Some(path) => path.clone(),
            None => std::env::current_exe()?
                .with_file_name(format!("chr-renderer-cli{}", std::env::consts::EXE_SUFFIX)),
        },
        program: WorkerProgram::new(spec.as_ref()),
        runtime_id: match &installation {
            Some((synthetic, _)) => synthetic.configuration.runtime_id.clone(),
            None => chr_runtime::program::synthetic::runtime_id()?,
        },
        output: Output {
            width: mode.width,
            height: mode.height,
            fps: mode.fps,
        },
        socket_dir: worker_dir(),
        hang: Duration::from_millis(args.worker_hang_ms),
        assets: WorkerAssets::default(),
        host: Default::default(),
    };
    log::info!("render worker: {}", worker_config.executable.display());
    let render_config = RenderConfig {
        mode,
        render_deadline: args
            .render_deadline_ms
            // Leave a quarter of the frame for clean-feed conversion and hand-off.
            .map_or(mode.frame_duration() * 3 / 4, Duration::from_millis),
    };
    let worker = if let Some((synthetic, _)) = &installation {
        RenderWorker::spawn_program(
            render_config,
            sdi.input(),
            worker_config,
            Arc::clone(&status),
            synthetic.scheduler.clone(),
        )?
    } else {
        RenderWorker::spawn(
            render_config,
            sdi.input(),
            worker_config,
            Arc::clone(&status),
        )?
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
                let adapter = if args.software_video.is_some() { adapter.with_rotation() } else { adapter };
                let (control, owner) = chr_runtime::retained::SceneControl::spawn(adapter, 64).map_err(|e| e.to_string())?;
                synthetic.create_scene(&control).await?;
                log::info!("local program runtime_id={} scene_id={} configuration_id={}",
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
        let handler = ControlHandler::new(commands, sdi.stats()).with_worker(Arc::clone(&status));
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
                    _ = progress.tick() => {
                        synthetic.scheduler.tick().map_err(|e| e.to_string())?;
                    }
                    _ = frames.tick() => {
                        #[cfg(all(unix, feature = "gstreamer-media"))]
                        if let Some(source) = &mut video_source {
                            control.advance_rotation(sdi.stats().status().frames_output).await.map_err(|e| e.to_string())?;
                            if let Some(frame) = source.try_frame()?
                                && let Err(error) = synthetic.inject_image(control, frame.sequence, &frame.rgba).await {
                                log::warn!("software input skipped: {error}");
                            }
                            continue;
                        }
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
            signal = shutdown_signal() => {
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
    #[cfg(all(unix, feature = "gstreamer-media"))]
    drop(video_source);
    // Dropping the runtime closes listeners (removing the socket file) and connections.
    runtime.shutdown_timeout(Duration::from_secs(2));
    let stopped = worker.shutdown();
    drop(sdi);
    if let Some(path) = &args.monitor_tap {
        // A monitor that still maps the tap keeps its pages until it lets go.
        let _ = std::fs::remove_file(path);
    }
    served?;
    stopped.map_err(Into::into)
}

/// The fixed output rate a DeckLink output opens in to go out in the input's
/// format `input`; the program renders at `width`x`height`, which the input
/// must have.
fn card_follows(
    input: sdi::Format,
    width: u32,
    height: u32,
) -> Result<chr_runtime::gst_core::OutputRate> {
    if (input.mode.width, input.mode.height) != (width, height) {
        return Err(format!(
            "the input is {} but --size is {width}x{height}: a DeckLink output goes out in \
             the input's format, so give --size {}x{} (or --output-rate fixed:<rate> to \
             convert)",
            input.name(),
            input.mode.width,
            input.mode.height
        )
        .into());
    }
    Ok(chr_runtime::gst_core::OutputRate::Fixed(
        input.mode.fps,
        input.scan,
    ))
}

/// A tracked program on chr-server's GStreamer core (#154): one GStreamer
/// pipeline ([`chr_runtime::gst_core`]) from chr-sdi-in's frames to the output
/// and the program tap, timed by the pipeline clock.
fn run_core(args: Args) -> Result<()> {
    use chr_runtime::gst_core::{
        Core, CoreConfig, OutputRate,
        driver::{Driver, Program},
    };
    ImageExtent::new(args.width, args.height)?;
    let mut output_rate = args.output_rate.unwrap_or(OutputRate::Follow);
    if args.decklink.is_some() && output_rate == OutputRate::Follow {
        // The card cannot change its mode while it plays: open it in the input's.
        // The program's nominal rate (--fps, the description's) stays: frames
        // carry their own rate, as with a following simulated output.
        chr_runtime::gst_core::init()?;
        let input = chr_runtime::gst_core::capture::probe(
            args.capture_socket
                .as_deref()
                .expect("defaulted when parsing"),
        )?;
        output_rate = card_follows(input, args.width, args.height)?;
        log::info!("DeckLink output in the input's format: {}", input.name());
    }
    let mode = VideoMode {
        width: args.width,
        height: args.height,
        fps: args.fps,
    };
    let spec = match (&args.tracked_scene, &args.description) {
        (Some(path), _) => ProgramSpec::TrackedScene { path: path.clone() },
        (_, Some(path)) => ProgramSpec::Description { path: path.clone() },
        _ => unreachable!("checked when parsing"),
    };
    let installed = spec.install(mode, chr_runtime::program::synthetic::runtime_id()?)?;
    let (synthetic, adapter) = (installed.program, installed.adapter);
    let native = synthetic.configuration.native_extent;
    let delay = args
        .program_delay_ms
        .map_or(mode.frame_duration() * 3, Duration::from_millis);
    let (output, scan) = match output_rate {
        OutputRate::Follow => (mode, chr_runtime::sdi::Scan::Progressive),
        OutputRate::Fixed(fps, scan) => (VideoMode { fps, ..mode }, scan),
    };
    let (sdi_stats, recorder) =
        chr_runtime::sdi::SdiStats::recorded(chr_runtime::sdi::Format { mode: output, scan });
    let recording = match &args.record {
        Some(root) => {
            use chr_runtime::worker::recording::{Recording, RecordingConfig, Stream, session_dir};
            Some(Recording::start(RecordingConfig {
                executable: match &args.recorder_cli {
                    Some(path) => path.clone(),
                    None => std::env::current_exe()?.with_file_name(format!(
                        "chr-recorder-cli{}",
                        std::env::consts::EXE_SUFFIX
                    )),
                },
                dir: session_dir(root),
                socket_dir: private_dir("recording"),
                streams: args
                    .record_streams
                    .clone()
                    .unwrap_or_else(|| Stream::ALL.to_vec()),
                fps: args.record_fps,
            })?)
        }
        None => None,
    };
    let record_tap = |stream| {
        recording
            .as_ref()
            .and_then(|recording| recording.tap(stream))
    };
    let (core, frames) = Core::start(CoreConfig {
        capture: VideoMode {
            width: native.width,
            height: native.height,
            fps: mode.fps,
        },
        output,
        output_rate,
        capture_socket: args.capture_socket.clone().expect("defaulted when parsing"),
        program_delay: delay,
        tap: args.monitor_tap.clone(),
        recorder,
        sink: match args.decklink {
            Some(device) => chr_runtime::gst_core::OutputSink::DeckLink { device },
            None => chr_runtime::gst_core::OutputSink::Simulated,
        },
        input_loss: args.input_loss.clone().unwrap_or_default(),
        audio: !args.no_audio,
        audio_channels: args.audio_channels.unwrap_or(2),
        record_input: record_tap(chr_runtime::worker::recording::Stream::Input),
        record_program: record_tap(chr_runtime::worker::recording::Stream::Program),
    })?;
    if let Some(path) = &args.monitor_tap {
        log::info!("program tap for chr-monitor at {}", path.display());
    }
    let lenses = if args.tracking_pinhole {
        LensSelection::pinhole()
    } else {
        LensSelection::auto(args.tracking_lens.clone())
    };
    let in_flight = in_flight_limit(delay, mode.fps);
    let feed = TrackingFeed::new(TRACKING_BACKLOG.max(in_flight), CHECKS);
    let tracker_status = chr_runtime::worker::tracking::supervisor::status_handle();
    let tracker = if args.tracking_worker {
        let camera_center = args
            .tracking_centre
            .or_else(|| {
                Some(
                    synthetic
                        .tracking
                        .as_ref()?
                        .replay
                        .as_ref()?
                        .camera_center(),
                )
            })
            .ok_or(
                "the tracking worker needs the station centre: pass --tracking-centre <x> <y> \
                 <z> or give the description a reference camera",
            )?;
        let executable = match &args.tracking_cli {
            Some(path) => path.clone(),
            None => std::env::current_exe()?.with_file_name(format!(
                "chr-tracking-sim-cli{}",
                std::env::consts::EXE_SUFFIX
            )),
        };
        log::info!(
            "tracking worker {} (station centre {camera_center:?}, at most {in_flight} frames \
             in flight)",
            executable.display()
        );
        Some(Tracker::new(
            TrackerConfig {
                executable,
                args: args.tracking_worker_args.clone(),
                threads: args.tracking_threads,
                width: native.width,
                height: native.height,
                fps: mode.fps,
                camera_center,
                socket_dir: private_dir("tracking"),
                hang: Duration::from_millis(args.worker_hang_ms),
                in_flight,
                host: core.host(),
            },
            Arc::clone(&tracker_status),
        )?)
    } else {
        log::info!("no tracking worker: cameras come from tracking.frame");
        None
    };
    let status = status_handle();
    let worker_assets = WorkerAssets::default();
    let asset_roots = args
        .asset_roots
        .iter()
        .map(|root| root.canonicalize())
        .collect::<std::io::Result<Vec<_>>>()?;
    let render = chr_runtime::worker::supervisor::Supervisor::new(
        WorkerConfig {
            executable: match &args.renderer_cli {
                Some(path) => path.clone(),
                None => std::env::current_exe()?
                    .with_file_name(format!("chr-renderer-cli{}", std::env::consts::EXE_SUFFIX)),
            },
            program: WorkerProgram::new(Some(&spec)),
            runtime_id: synthetic.configuration.runtime_id.clone(),
            output: Output {
                width: mode.width,
                height: mode.height,
                fps: mode.fps,
            },
            socket_dir: worker_dir(),
            hang: Duration::from_millis(args.worker_hang_ms),
            assets: worker_assets.clone(),
            host: core.host(),
        },
        Arc::clone(&status),
    )?;
    log::info!(
        "render worker {}; lens: {}",
        args.renderer_cli.as_deref().map_or_else(
            || "chr-renderer-cli".into(),
            |path| path.display().to_string()
        ),
        match (lenses.is_forced_pinhole(), lenses.cli()) {
            (true, _) => "pinhole".to_owned(),
            (false, Some(cli)) => format!("the tracking data's, else {}", lens::describe(cli)),
            (false, None) => "the tracking data's, else none (pinhole)".to_owned(),
        }
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let served = runtime.block_on(async {
        let (control, owner) =
            chr_runtime::retained::SceneControl::spawn(adapter.with_rotation(), 64)
                .map_err(|e| e.to_string())?;
        synthetic.create_scene(&control).await?;
        log::info!(
            "local program runtime_id={} scene_id={}",
            synthetic.configuration.runtime_id.as_str(),
            synthetic.scene_id.as_str()
        );
        let loader = match synthetic.described {
            Some(_) => Some(Arc::new(chr_runtime::program::description::Loader::new(
                &synthetic,
                control.clone(),
                asset_roots.clone(),
                worker_assets.clone(),
                worker_dir().join("assets"),
            )?)),
            None => None,
        };
        let (commands, receiver) =
            tokio::sync::mpsc::channel(chr_runtime::pipeline::COMMAND_QUEUE_CAPACITY);
        tokio::spawn(chr_runtime::pipeline::answer_commands(receiver, mode.fps));
        let result = {
            let handler = ControlHandler::new(commands, sdi_stats)
                .with_worker(Arc::clone(&status))
                .with_scenes(control.clone())
                .with_tracking(feed.clone());
            let handler = if args.tracking_worker {
                handler.with_tracking_worker(Arc::clone(&tracker_status))
            } else {
                handler
            };
            let handler = Arc::new(match loader {
                Some(loader) => handler.with_description(loader),
                None => handler,
            });
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
            let program = Program::new(&synthetic, control.clone())?;
            // Frames in a program delay at the highest rate the input may switch to.
            let slots = Rate::fps(chr_common::rate::MAX_FPS).frames_in(core.config().program_delay);
            let driver = Driver::new(
                &core,
                tracker,
                render,
                feed,
                lenses,
                program,
                usize::try_from(slots)
                    .unwrap_or(usize::MAX)
                    .saturating_add(8),
            );
            tokio::select! {
                result = server.serve_websocket(websocket) => result.map_err(Into::into),
                result = native => result,
                result = driver.run(frames) => result,
                signal = shutdown_signal() => {
                    signal?;
                    log::info!("shutting down");
                    Ok(())
                }
            }
        };
        control.shutdown().await;
        drop(control);
        owner.await?;
        result
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    drop(core);
    // After the core: the worker records the last frames, then the streams end.
    drop(recording);
    served
}

/// Resolves on Ctrl-C or SIGTERM (`stack.py stop`, service managers), so both shut
/// the server down cleanly and its workers' socket directories are removed.
async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            signal = tokio::signal::ctrl_c() => signal,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}

/// Removes the worker, tracking and recording socket directories of chr-servers that are gone:
/// a server that was killed (SIGKILL, a crash) cannot remove its own, and they
/// live in memory-backed `$XDG_RUNTIME_DIR`.
fn remove_stale_private_dirs() {
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return;
    };
    for path in stale_private_dirs(&PathBuf::from(runtime).join("chr"), server_running) {
        match std::fs::remove_dir_all(&path) {
            Ok(()) => log::info!("removed {} left by a stopped chr-server", path.display()),
            Err(error) => log::warn!("cannot remove {}: {error}", path.display()),
        }
    }
}

/// `<root>/worker-<pid>`, `<root>/tracking-<pid>` and `<root>/recording-<pid>`
/// directories whose server is not running.
fn stale_private_dirs(root: &std::path::Path, running: impl Fn(u32) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut stale: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let (kind, pid) = name.split_once('-')?;
            let pid = pid.parse().ok()?;
            (matches!(kind, "worker" | "tracking" | "recording")
                && entry.path().is_dir()
                && !running(pid))
            .then(|| entry.path())
        })
        .collect();
    stale.sort();
    stale
}

/// Whether `pid` is a running chr-server. Without `/proc` every PID counts as running,
/// so nothing is removed.
fn server_running(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/comm")) {
        Ok(comm) => comm.trim_end() == "chr-server",
        Err(_) => !std::path::Path::new("/proc/self").exists(),
    }
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

    /// Parses with explicit socket and tap paths first, so results never depend on
    /// `XDG_RUNTIME_DIR`; later arguments still override them.
    fn parse(args: &[&str]) -> Result<Option<Args>> {
        let explicit_paths = [
            "--socket",
            "/tmp/chr-test.sock",
            "--monitor-tap",
            "/tmp/chr-test.tap",
        ];
        let capture = ["--capture-socket", "/run/c.sock"];
        let tracked = args
            .iter()
            .any(|arg| matches!(*arg, "--tracked-scene" | "--description"));
        let capture: &[&str] = if tracked { &capture } else { &[] };
        parse_args(
            explicit_paths
                .iter()
                .chain(capture)
                .chain(args)
                .map(|arg| (*arg).to_owned()),
        )
    }

    #[test]
    fn only_worker_directories_of_stopped_servers_are_stale() {
        let root = std::env::temp_dir().join(format!("chr-stale-{}", std::process::id()));
        for dir in [
            "worker-1",
            "tracking-1",
            "recording-1",
            "worker-2",
            "tracking-x",
            "other-3",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("worker-4"), b"").unwrap();
        assert_eq!(
            stale_private_dirs(&root, |pid| pid == 2),
            [
                root.join("recording-1"),
                root.join("tracking-1"),
                root.join("worker-1")
            ]
        );
        assert!(stale_private_dirs(&root.join("missing"), |_| false).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn recording_needs_a_tracked_program_and_names_its_streams() {
        use chr_runtime::worker::recording::Stream;
        let tracked = ["--description", "/d.json", "--record", "/rec"];
        let args = parse(&tracked).unwrap().unwrap();
        assert_eq!(args.record, Some(PathBuf::from("/rec")));
        assert_eq!(args.record_streams, None, "both, by default");
        let args = parse(&[&tracked[..], &["--record-streams", "program"]].concat())
            .unwrap()
            .unwrap();
        assert_eq!(args.record_streams, Some(vec![Stream::Program]));
        assert!(parse(&[&tracked[..], &["--record-streams", "output"]].concat()).is_err());
        assert!(parse(&["--record", "/rec"]).is_err(), "a generated program");
        assert!(parse(&["--description", "/d.json", "--record-streams", "input"]).is_err());
        assert!(parse(&["--description", "/d.json", "--recorder-cli", "/r"]).is_err());
        let args = parse(&[&tracked[..], &["--record-fps", "10"]].concat())
            .unwrap()
            .unwrap();
        assert_eq!(args.record_fps, Some(Rate::fps(10)));
        assert!(parse(&[&tracked[..], &["--record-fps", "0"]].concat()).is_err());
        assert!(parse(&["--description", "/d.json", "--record-fps", "10"]).is_err());
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
        assert_eq!(
            (args.width, args.height, args.fps),
            (640, 360, Rate::fps(60))
        );
        let ntsc = parse(&["--fps", "30000/1001"]).unwrap().unwrap();
        assert_eq!(ntsc.fps, Rate::new(30000, 1001).unwrap());
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
        assert!(parse(&["--software-video", "video.mp4", "--synthetic-program"]).is_err());
        assert!(parse(&["--software-video", "video.mp4", "--pitch-demo", "demo.json"]).is_err());
        assert!(parse(&["--video-scene", "scene.json"]).is_err());
        let tracked = parse(&["--tracked-scene", "scene.json", "--program-delay-ms", "60"])
            .unwrap()
            .unwrap();
        assert_eq!(
            (tracked.program_delay_ms, tracked.capture_socket),
            (Some(60), Some(PathBuf::from("/run/c.sock")))
        );
        assert!(parse(&["--tracked-scene", "scene.json", "--synthetic-program"]).is_err());
        assert!(parse(&["--program-delay-ms", "60"]).is_err());
        let delayed = |ms| parse(&["--tracked-scene", "scene.json", "--program-delay-ms", ms]);
        assert!(delayed("1500").is_ok());
        assert!(delayed("1501").is_err());
        assert!(delayed("4").is_err(), "not above the decision margin");
        for (removed, hint) in [
            (
                &["--tracked-scene", "s.json", "--tracking-wait-ms", "60"][..],
                "--program-delay-ms",
            ),
            (&["--input-ring", "/tmp/in.ring"], "chr-sdi-in"),
            (&["--pipeline", "gst"], "GStreamer core"),
        ] {
            let error = parse(removed).unwrap_err().to_string();
            assert!(error.contains(hint), "{removed:?}: {error}");
        }
        let worker = parse(&[
            "--description",
            "start.json",
            "--tracking-worker",
            "--tracking-cli",
            "/opt/tracker",
            "--tracking-worker-arg",
            "--ledger",
            "--tracking-worker-arg",
            "/runs/{stream_id}.arrow",
            "--tracking-threads",
            "4",
            "--tracking-centre",
            "14.5",
            "60.6",
            "-9.9",
            "--tracking-lens",
            "pinhole",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            (
                worker.tracking_worker,
                worker.tracking_cli,
                worker.tracking_worker_args,
                worker.tracking_threads,
                worker.tracking_centre,
                worker.tracking_pinhole,
            ),
            (
                true,
                Some(PathBuf::from("/opt/tracker")),
                vec!["--ledger".to_owned(), "/runs/{stream_id}.arrow".to_owned()],
                NonZeroU32::new(4),
                Some([14.5, 60.6, -9.9]),
                true,
            )
        );
        let threads = |n| {
            parse(&[
                "--description",
                "d.json",
                "--tracking-worker",
                "--tracking-threads",
                n,
            ])
        };
        assert!(threads("0").is_err());
        assert!(threads("-1").is_err());
        assert!(parse(&["--description", "d.json", "--tracking-threads", "4"]).is_err());
        let tracked = ["--tracked-scene", "s.json"];
        let lens_args = [
            "--tracking-lens-k",
            "-0.05",
            "0",
            "0",
            "--tracking-lens-centre",
            "960",
            "540",
            "--tracking-lens-scale",
            "960",
        ];
        let cli = parse(&[&tracked[..], &lens_args].concat())
            .unwrap()
            .unwrap();
        assert!(!cli.tracking_pinhole, "auto is the default");
        assert_eq!(
            cli.tracking_lens,
            Some(lens::wire([960., 540.], 960., [-0.05, 0., 0.], None, &CHECKS).unwrap()),
            "the CLI lens needs no tracking worker"
        );
        let supported = [
            &tracked[..],
            &lens_args,
            &["--tracking-lens-support", "-160", "680", "2080", "1090"],
        ]
        .concat();
        assert!(
            parse(&supported)
                .unwrap()
                .unwrap()
                .tracking_lens
                .unwrap()
                .support
                .is_some()
        );
        for bad in [&lens_args[..], &["--tracking-lens", "auto"]] {
            assert!(parse(bad).is_err(), "{bad:?} without a tracked program");
        }
        for bad in [
            &lens_args[..5],
            &lens_args[..7],
            &lens_args[4..],
            &["--tracking-lens-support", "0", "0", "1", "1"],
            &[&lens_args[..8], &["0"]].concat(),
            &[&lens_args[..8], &["-1"]].concat(),
            &[&lens_args[..8], &["nan"]].concat(),
            &[
                &lens_args[..],
                &["--tracking-lens-support", "5", "0", "1", "1"],
            ]
            .concat(),
            &["--tracking-lens", "identity"],
            &["--tracking-lens", "program"],
            &["--tracking-lens-k", "1", "2"],
        ] {
            let args = [&tracked[..], bad].concat();
            assert!(parse(&args).is_err(), "{bad:?}");
        }
        assert!(
            parse(&["--tracking-worker"]).is_err(),
            "needs a tracked program"
        );
        for bad in [
            &["--tracking-cli", "/opt/tracker"][..],
            &["--tracking-centre", "1", "2", "3"],
        ] {
            let args = [&["--tracked-scene", "s.json"][..], bad].concat();
            assert!(parse(&args).is_err(), "{bad:?} without --tracking-worker");
        }
        for bad in [
            &["--tracking-centre", "1", "2"][..],
            &["--tracking-centre", "1", "2", "inf"],
            &["--tracking-lens", "radial"],
            &["--tracking-lens", "identity"],
        ] {
            let args = [&tracked[..], &["--tracking-worker"], bad].concat();
            assert!(parse(&args).is_err(), "{bad:?}");
        }
        assert_eq!(
            parse(&[]).unwrap().unwrap().monitor_tap,
            Some(PathBuf::from("/tmp/chr-test.tap"))
        );
        assert!(
            parse(&["--monitor-tap", "none"])
                .unwrap()
                .unwrap()
                .monitor_tap
                .is_none()
        );
        assert_eq!(
            parse(&[
                "--software-video",
                "video.mp4",
                "--video-scene",
                "scene.json"
            ])
            .unwrap()
            .unwrap()
            .video_scene,
            Some(PathBuf::from("scene.json"))
        );
        assert_eq!(
            parse(&["--software-video", "video.mp4"])
                .unwrap()
                .unwrap()
                .software_video,
            Some(PathBuf::from("video.mp4"))
        );
        assert!(parse(&["--help"]).unwrap().is_none());
        for bad in [
            &["--size", "641x360"][..],
            &["--fps", "0"],
            &["--fps"],
            &["--bogus"],
            &["--ws", "nonsense"],
            &["--srt", "127.0.0.1:9001"],
            &["--webrtc-udp", "none"],
            &["--render-deadline-ms", "0"],
            &["--worker-hang-ms", "0"],
            &["--worker-hang-ms", "soon"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        let worker = parse(&[
            "--renderer-cli",
            "/opt/chr/chr-renderer-cli",
            "--render-deadline-ms",
            "15",
            "--worker-hang-ms",
            "500",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            worker.renderer_cli.as_deref(),
            Some(std::path::Path::new("/opt/chr/chr-renderer-cli"))
        );
        assert_eq!(
            (worker.render_deadline_ms, worker.worker_hang_ms),
            (Some(15), 500)
        );
        let defaults = parse(&[]).unwrap().unwrap();
        assert_eq!(
            (defaults.render_deadline_ms, defaults.worker_hang_ms),
            (None, 2000)
        );
        let core = [
            "--description",
            "d.json",
            "--tracking-worker",
            "--capture-socket",
            "/run/x.sock",
        ];
        let fixed = parse(
            &[
                &core[..],
                &["--program-delay-ms", "80", "--output-rate", "fixed:59.94"],
            ]
            .concat(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (
                fixed.capture_socket,
                fixed.program_delay_ms,
                fixed.output_rate
            ),
            (
                Some(PathBuf::from("/run/x.sock")),
                Some(80),
                Some(chr_runtime::gst_core::OutputRate::Fixed(
                    Rate::new(60000, 1001).unwrap(),
                    chr_runtime::sdi::Scan::Progressive
                ))
            )
        );
        for bad in [
            &[&core[..], &["--render-deadline-ms", "15"]].concat()[..],
            &[&core[..], &["--output-rate", "fixed:0"]].concat(),
            &[&core[..], &["--output-rate", "50"]].concat(),
            &["--capture-socket", "/run/c.sock"],
            &["--output-rate", "follow"],
            &["--fps", "29.9"],
            &[&core[..], &["--input-loss", "freeze"]].concat(),
            &[&core[..], &["--output", "sdi"]].concat(),
            &[&core[..], &["--device", "1"]].concat(),
            &["--input-loss", "black"],
            &["--output", "decklink"],
            &["--no-audio"],
            &[&core[..], &["--audio-channels", "8"]].concat(),
            &[
                &core[..],
                &["--output", "decklink", "--audio-channels", "6"],
            ]
            .concat(),
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        let card = parse(
            &[
                &core[..],
                &[
                    "--output",
                    "decklink",
                    "--input-loss",
                    "slate:/s.png",
                    "--no-audio",
                ],
            ]
            .concat(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (card.decklink, card.input_loss, card.no_audio),
            (
                Some(0),
                Some(chr_runtime::gst_core::InputLoss::Slate(Some(
                    "/s.png".into()
                ))),
                true
            )
        );
        let sim = parse(&core).unwrap().unwrap();
        assert_eq!(
            (sim.decklink, sim.input_loss, sim.no_audio),
            (None, None, false)
        );
        let second = parse(&[&core[..], &["--output", "decklink", "--device", "1"]].concat())
            .unwrap()
            .unwrap();
        assert_eq!(second.decklink, Some(1));
        let external = parse(&["--description", "d.json"]).unwrap().unwrap();
        assert!(
            !external.tracking_worker,
            "cameras from tracking.frame on the core"
        );
    }

    #[test]
    fn a_card_opens_in_the_inputs_rate_and_scan_at_the_program_size() {
        use chr_runtime::{
            gst_core::OutputRate,
            sdi::{FieldOrder, Format, Scan},
        };
        let i50 = Format {
            mode: VideoMode {
                width: 1920,
                height: 1080,
                fps: Rate::fps(25),
            },
            scan: Scan::Interlaced(FieldOrder::TopFirst),
        };
        assert_eq!(
            card_follows(i50, 1920, 1080).unwrap(),
            OutputRate::Fixed(Rate::fps(25), i50.scan)
        );
        let error = card_follows(i50, 1280, 720).unwrap_err().to_string();
        assert!(
            error.contains("1920x1080i50") && error.contains("--size 1920x1080"),
            "{error}"
        );
    }
}
