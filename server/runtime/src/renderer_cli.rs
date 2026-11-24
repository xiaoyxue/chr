//! `chr-renderer-cli`: the headless renderer entry point.
//!
//! `render` renders one program frame through the same installation, scene control
//! and program bridge as `chr-server`, and writes the BT.709 RGBA composition as a
//! PNG; `bench` times that frame through the render worker's path. `worker` is the render worker that `chr-server` starts and supervises; it
//! speaks the protocol of `chr_runtime::worker` on the two GStreamer unixfd sockets
//! that chr-server names, and logs to stderr.

use std::{
    error::Error,
    fs::{File, OpenOptions},
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
};

use chr_common::rate::Rate;
use chr_renderer::Gpu;
use chr_runtime::{
    program::{
        spec::ProgramSpec,
        still::{self, Background, FrameTiming, Still},
    },
    sdi::VideoMode,
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const USAGE: &str = "\
Usage: chr-renderer-cli render <program> --output <png> [options]
       chr-renderer-cli bench <program> [--frames <n>] [options]
       chr-renderer-cli worker --input-socket <path> --output-socket <path>
                                    (started by chr-server; not for interactive use)

render: renders one frame of a chr-server program and writes the BT.709 RGBA program
image as PNG code values, without further colour conversion.
bench: renders the same frame repeatedly the way a render worker does (composition,
UYVY conversion, readback) and prints GPU and wall time per frame.

Program (exactly one):
  --synthetic-program     synthetic-program-v1 (max 1280x720)
  --pitch-demo <json>     Owner-local pitch-demo-v1 manifest
  --video-scene <json>    Local calibrated-video-v1 scene/lens manifest
  --tracked-scene <json>  The same manifest as tracked-video-v1; the frame carries the
                          manifest's Ti/C and the warp of --frame as its tracked camera
  --description <json>    A chr-render-description-v1 file; the frame carries its
                          camera.reference and the warp of --frame

Options:
  --output <png>          render: output file; refuses to replace it without --overwrite
  --overwrite             render: replace an existing output file
  --frames <n>            bench: frames to time after 10 warm-up frames [default: 300]
  --size <WxH>            Output size; both even, up to 4096x2160 [default: 1280x720]
  --fps <rate>            Output rate, 1-120 fps: an integer, N/D (30000/1001) or 29.97,
                          59.94 ...; a video scene needs its clip rate [default: 50]
  --state <json>          chr-common SceneState replacing the program's initial scene
                          (its configuration_id is replaced by the program's)
  --background <png>      Native input image, read as BT.709 RGBA code values
                          [default: the program's synthetic input]
  --frame <n>             Input frame sequence; selects the lens model of a video
                          scene or the replayed camera of a tracked scene [default: 0]
  -h, --help              Show this help";

#[derive(Debug, PartialEq)]
struct RenderArgs {
    spec: ProgramSpec,
    mode: VideoMode,
    output: PathBuf,
    overwrite: bool,
    state: Option<PathBuf>,
    background: Option<PathBuf>,
    frame: u64,
    /// `Some` for `bench`: frames to time.
    bench: Option<u32>,
}

#[derive(Debug, PartialEq)]
enum Subcommand {
    Render(RenderArgs),
    Worker { input: PathBuf, output: PathBuf },
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Subcommand>> {
    let bench = match raw.next().as_deref() {
        Some("render") => None,
        Some("bench") => Some(300),
        Some("worker") => {
            let (input, output, rest) = chr_runtime::worker::transport::parse_worker_args(raw)?;
            return match rest.first() {
                None => Ok(Some(Subcommand::Worker { input, output })),
                Some(other) => Err(format!("unknown worker argument {other:?}").into()),
            };
        }
        Some("-h" | "--help") | None => return Ok(None),
        Some(other) => return Err(format!("unknown subcommand {other:?}\n\n{USAGE}").into()),
    };
    let mut specs = Vec::new();
    let mut output = None;
    let mut args = RenderArgs {
        spec: ProgramSpec::Synthetic,
        mode: VideoMode {
            width: 1280,
            height: 720,
            fps: Rate::fps(50),
        },
        output: PathBuf::new(),
        overwrite: false,
        state: None,
        background: None,
        frame: 0,
        bench,
    };
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--synthetic-program" => specs.push(ProgramSpec::Synthetic),
            "--pitch-demo" => specs.push(ProgramSpec::PitchDemo {
                path: value()?.into(),
            }),
            "--video-scene" => specs.push(ProgramSpec::VideoScene {
                path: value()?.into(),
            }),
            "--tracked-scene" => specs.push(ProgramSpec::TrackedScene {
                path: value()?.into(),
            }),
            "--description" => specs.push(ProgramSpec::Description {
                path: value()?.into(),
            }),
            "--output" => output = Some(PathBuf::from(value()?)),
            "--overwrite" => args.overwrite = true,
            "--size" => {
                let size = value()?;
                let (width, height) = size
                    .split_once('x')
                    .ok_or("--size must look like 1280x720")?;
                (args.mode.width, args.mode.height) = (width.parse()?, height.parse()?);
            }
            "--fps" => args.mode.fps = value()?.parse()?,
            "--state" => args.state = Some(value()?.into()),
            "--background" => args.background = Some(value()?.into()),
            "--frame" => args.frame = value()?.parse()?,
            "--frames" if args.bench.is_some() => args.bench = Some(value()?.parse()?),
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
        }
    }
    args.spec = match <[ProgramSpec; 1]>::try_from(specs) {
        Ok([spec]) => spec,
        Err(_) => {
            return Err(
                "select exactly one of --synthetic-program, --pitch-demo, --video-scene, --tracked-scene, --description"
                    .into(),
            );
        }
    };
    match (args.bench, output) {
        (None, Some(output)) => args.output = output,
        (None, None) => return Err("--output <png> is required".into()),
        (Some(0), _) => return Err("--frames must be positive".into()),
        (Some(_), None) => {}
        (Some(_), Some(_)) => return Err("bench takes no --output".into()),
    }
    let mode = args.mode;
    if mode.width == 0
        || mode.height == 0
        || !mode.width.is_multiple_of(2)
        || !mode.height.is_multiple_of(2)
    {
        return Err("--size needs a nonzero, even width and height".into());
    }
    Ok(Some(Subcommand::Render(args)))
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Some(Subcommand::Worker { input, output })) => {
            let result = chr_runtime::worker::serve::serve(&input, &output);
            if let Err(error) = result {
                log::error!("render worker: {error}");
                std::process::exit(1);
            }
            return;
        }
        Ok(Some(Subcommand::Render(args))) => args,
        Ok(None) => {
            println!("{USAGE}");
            return;
        }
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    };
    if let Err(error) = render(args) {
        log::error!("{error}");
        std::process::exit(1);
    }
}

fn render(args: RenderArgs) -> Result<()> {
    if args.bench.is_none() && !args.overwrite && args.output.exists() {
        return Err(format!(
            "{} exists; pass --overwrite to replace it",
            args.output.display()
        )
        .into());
    }
    let state = args
        .state
        .as_ref()
        .map(|path| -> Result<_> {
            let file = BufReader::new(File::open(path)?);
            serde_json::from_reader(file)
                .map_err(|error| format!("{}: {error}", path.display()).into())
        })
        .transpose()?;
    let background = args
        .background
        .as_deref()
        .map(read_background)
        .transpose()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let pixels = runtime.block_on(async {
        // Every input is validated before a GPU is requested.
        let prepared = still::prepare(Still {
            spec: args.spec.clone(),
            mode: args.mode,
            state,
            background,
            frame: args.frame,
        })
        .await?;
        let gpu = if args.bench.is_some() {
            timed_gpu().await?
        } else {
            Gpu::request(&Gpu::instance(), None)
                .await
                .map_err(|error| error.to_string())?
        };
        let info = gpu.adapter.get_info();
        log::info!(
            "GPU adapter: {} ({:?}, {:?})",
            info.name,
            info.device_type,
            info.backend
        );
        if info.device_type == wgpu::DeviceType::Cpu {
            return Err::<_, Box<dyn Error + Send + Sync>>(
                "a software GPU adapter is not accepted; use a hardware GPU".into(),
            );
        }
        let extent = prepared.extent();
        if let Some(frames) = args.bench {
            let timings = prepared.bench(&gpu, WARM_UP + frames).await?;
            print_timings(extent, &timings[WARM_UP as usize..]);
            return Ok(None);
        }
        Ok(Some((extent, prepared.render(&gpu).await?)))
    })?;
    let Some(((width, height), rgba)) = pixels else {
        return Ok(());
    };
    write_png(&args.output, args.overwrite, width, height, rgba)?;
    log::info!("wrote {width}x{height} {}", args.output.display());
    Ok(())
}

/// Frames rendered before timing starts, so pipelines, caches and clocks settle.
const WARM_UP: u32 = 10;

/// The default high-performance adapter with timestamp queries when it supports them.
async fn timed_gpu() -> Result<Gpu> {
    let instance = Gpu::instance();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        })
        .await?;
    let timestamps =
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("chr-renderer-cli bench"),
            required_features: adapter.features() & timestamps,
            required_limits: Gpu::required_limits(&adapter),
            ..Default::default()
        })
        .await?;
    Ok(Gpu {
        adapter,
        device,
        queue,
    })
}

fn print_timings((width, height): (u32, u32), timings: &[FrameTiming]) {
    let summary = |mut values: Vec<f64>| {
        values.sort_by(f64::total_cmp);
        let at = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];
        format!(
            "mean {:7.3}  p50 {:7.3}  p95 {:7.3}  max {:7.3} ms",
            values.iter().sum::<f64>() / values.len() as f64,
            at(0.5),
            at(0.95),
            at(1.0)
        )
    };
    println!("{width}x{height}, {} frames", timings.len());
    match timings.iter().map(|t| t.gpu_ms).collect::<Option<Vec<_>>>() {
        Some(gpu) => println!("gpu   {}", summary(gpu)),
        None => println!("gpu   unavailable: the adapter has no timestamp queries inside encoders"),
    }
    println!(
        "wall  {}",
        summary(timings.iter().map(|t| t.wall_ms).collect())
    );
}

fn read_background(path: &Path) -> Result<Background> {
    let image = image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()
        .map_err(|error| format!("{}: {error}", path.display()))?
        .into_rgba8();
    Ok(Background {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

fn write_png(path: &Path, overwrite: bool, width: u32, height: u32, rgba: Vec<u8>) -> Result<()> {
    let image = image::RgbaImage::from_raw(width, height, rgba).ok_or("rendered image size")?;
    let mut options = OpenOptions::new();
    options.write(true);
    if overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file = BufWriter::new(options.open(path)?);
    image.write_to(&mut file, image::ImageFormat::Png)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<RenderArgs>> {
        parse_args(args.iter().map(|arg| (*arg).to_owned())).map(|parsed| {
            parsed.map(|subcommand| match subcommand {
                Subcommand::Render(args) => args,
                Subcommand::Worker { .. } => panic!("not a render command"),
            })
        })
    }

    #[test]
    fn parses_render_options_and_rejects_bad_values() {
        let args = parse(&[
            "render",
            "--video-scene",
            "scene.json",
            "--output",
            "out.png",
            "--size",
            "640x360",
            "--fps",
            "29.97",
            "--state",
            "state.json",
            "--background",
            "plate.png",
            "--frame",
            "12",
            "--overwrite",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            args,
            RenderArgs {
                spec: ProgramSpec::VideoScene {
                    path: "scene.json".into()
                },
                mode: VideoMode {
                    width: 640,
                    height: 360,
                    fps: Rate::new(30000, 1001).unwrap()
                },
                output: "out.png".into(),
                overwrite: true,
                state: Some("state.json".into()),
                background: Some("plate.png".into()),
                frame: 12,
                bench: None,
            }
        );
        let bench = parse(&["bench", "--description", "d.json", "--frames", "50"])
            .unwrap()
            .unwrap();
        assert_eq!((bench.bench, bench.output), (Some(50), PathBuf::new()));
        assert_eq!(
            parse(&["bench", "--synthetic-program"])
                .unwrap()
                .unwrap()
                .bench,
            Some(300)
        );
        let defaults = parse(&["render", "--synthetic-program", "--output", "o.png"])
            .unwrap()
            .unwrap();
        assert_eq!(
            (defaults.mode.width, defaults.mode.height, defaults.mode.fps),
            (1280, 720, Rate::fps(50))
        );
        assert!(!defaults.overwrite);
        assert!(parse(&[]).unwrap().is_none());
        assert!(parse(&["render", "--help"]).unwrap().is_none());
        let worker = ["worker", "--input-socket", "in", "--output-socket", "out"];
        assert_eq!(
            parse_args(worker.map(str::to_owned).into_iter()).unwrap(),
            Some(Subcommand::Worker {
                input: "in".into(),
                output: "out".into()
            })
        );
        assert!(parse_args(["worker".to_owned()].into_iter()).is_err());
        assert!(
            parse_args(
                [
                    "worker",
                    "--input-socket",
                    "a",
                    "--output-socket",
                    "b",
                    "--x"
                ]
                .map(str::to_owned)
                .into_iter()
            )
            .is_err()
        );
        for bad in [
            &["paint"][..],
            &["render", "--output", "o.png"],
            &[
                "render",
                "--synthetic-program",
                "--output",
                "o.png",
                "--frames",
                "5",
            ],
            &["bench", "--synthetic-program", "--output", "o.png"],
            &["bench", "--synthetic-program", "--frames", "0"],
            &["render", "--synthetic-program"],
            &[
                "render",
                "--synthetic-program",
                "--pitch-demo",
                "p.json",
                "--output",
                "o.png",
            ],
            &[
                "render",
                "--synthetic-program",
                "--output",
                "o.png",
                "--size",
                "641x360",
            ],
            &[
                "render",
                "--synthetic-program",
                "--output",
                "o.png",
                "--fps",
                "0",
            ],
            &[
                "render",
                "--synthetic-program",
                "--output",
                "o.png",
                "--frame",
                "-1",
            ],
            &[
                "render",
                "--synthetic-program",
                "--output",
                "o.png",
                "--samples",
                "4",
            ],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rejects_bad_files_before_requesting_a_gpu() {
        let dir = std::env::temp_dir().join(format!("chr-renderer-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("existing.png");
        std::fs::write(&existing, b"keep").unwrap();
        let state = dir.join("state.json");
        std::fs::write(&state, b"{\"not\":\"a scene state\"}").unwrap();
        let render_with = |extra: &[&str]| {
            let mut args = vec!["render", "--synthetic-program", "--size", "64x32"];
            args.extend_from_slice(extra);
            render(parse(&args).unwrap().unwrap())
                .unwrap_err()
                .to_string()
        };
        let existing_arg = existing.to_str().unwrap();
        let output = dir.join("out.png");
        let output_arg = output.to_str().unwrap();
        assert!(render_with(&["--output", existing_arg]).contains("--overwrite"));
        assert_eq!(std::fs::read(&existing).unwrap(), b"keep");
        assert!(
            render_with(&["--output", output_arg, "--state", state.to_str().unwrap()])
                .contains("state.json")
        );
        assert!(
            render_with(&[
                "--output",
                output_arg,
                "--background",
                state.to_str().unwrap()
            ])
            .contains("state.json")
        );
        assert!(!output.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
