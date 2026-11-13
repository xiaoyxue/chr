//! `chr-renderer-cli`: the headless renderer entry point.
//!
//! `render` renders one program frame through the same installation, scene control
//! and program bridge as `chr-server`, and writes the BT.709 RGBA composition as a
//! PNG. `worker` is the render worker that `chr-server` starts and supervises; it
//! speaks the protocol of `chr_runtime::worker` on stdin/stdout and logs to stderr.

use std::{
    error::Error,
    fs::{File, OpenOptions},
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
};

use chr_renderer::Gpu;
use chr_runtime::{
    program::{
        spec::ProgramSpec,
        still::{self, Background, Still},
    },
    sdi::VideoMode,
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const USAGE: &str = "\
Usage: chr-renderer-cli render <program> --output <png> [options]
       chr-renderer-cli worker      (started by chr-server; not for interactive use)

Renders one frame of a chr-server program and writes the BT.709 RGBA program image
as PNG code values, without further colour conversion.

Program (exactly one):
  --synthetic-program     synthetic-program-v1 (max 1280x720)
  --pitch-demo <json>     Owner-local pitch-demo-v1 manifest (max 1280x720)
  --video-scene <json>    Local calibrated-video-v1 scene/lens manifest
  --tracked-scene <json>  The same manifest as tracked-video-v1; the frame carries the
                          manifest's Ti/C and the warp of --frame as its tracked camera

Options:
  --output <png>          Output file; refuses to replace it without --overwrite
  --overwrite             Replace an existing output file
  --size <WxH>            Output size; both even [default: 1280x720]
  --fps <n>               Output rate, 1-120; a video scene needs its clip rate [default: 50]
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
}

#[derive(Debug, PartialEq)]
enum Subcommand {
    Render(RenderArgs),
    Worker,
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Subcommand>> {
    match raw.next().as_deref() {
        Some("render") => {}
        Some("worker") => {
            return match raw.next() {
                None => Ok(Some(Subcommand::Worker)),
                Some(other) => Err(format!("worker takes no arguments, got {other:?}").into()),
            };
        }
        Some("-h" | "--help") | None => return Ok(None),
        Some(other) => return Err(format!("unknown subcommand {other:?}\n\n{USAGE}").into()),
    }
    let mut specs = Vec::new();
    let mut output = None;
    let mut args = RenderArgs {
        spec: ProgramSpec::Synthetic,
        mode: VideoMode {
            width: 1280,
            height: 720,
            fps: 50,
        },
        output: PathBuf::new(),
        overwrite: false,
        state: None,
        background: None,
        frame: 0,
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
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
        }
    }
    args.spec = match <[ProgramSpec; 1]>::try_from(specs) {
        Ok([spec]) => spec,
        Err(_) => {
            return Err(
                "select exactly one of --synthetic-program, --pitch-demo, --video-scene, --tracked-scene"
                    .into(),
            );
        }
    };
    args.output = output.ok_or("--output <png> is required")?;
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
    Ok(Some(Subcommand::Render(args)))
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Some(Subcommand::Worker)) => {
            let result = chr_runtime::worker::serve::serve(
                std::io::stdin().lock(),
                std::io::stdout().lock(),
            );
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
    if !args.overwrite && args.output.exists() {
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
        let gpu = Gpu::request(&Gpu::instance(), None)
            .await
            .map_err(|error| error.to_string())?;
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
        Ok((extent, prepared.render(&gpu).await?))
    })?;
    let ((width, height), rgba) = pixels;
    write_png(&args.output, args.overwrite, width, height, rgba)?;
    log::info!("wrote {width}x{height} {}", args.output.display());
    Ok(())
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
                Subcommand::Worker => panic!("not a render command"),
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
            "25",
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
                    fps: 25
                },
                output: "out.png".into(),
                overwrite: true,
                state: Some("state.json".into()),
                background: Some("plate.png".into()),
                frame: 12,
            }
        );
        let defaults = parse(&["render", "--synthetic-program", "--output", "o.png"])
            .unwrap()
            .unwrap();
        assert_eq!(
            (defaults.mode.width, defaults.mode.height, defaults.mode.fps),
            (1280, 720, 50)
        );
        assert!(!defaults.overwrite);
        assert!(parse(&[]).unwrap().is_none());
        assert!(parse(&["render", "--help"]).unwrap().is_none());
        assert_eq!(
            parse_args(["worker".to_owned()].into_iter()).unwrap(),
            Some(Subcommand::Worker)
        );
        assert!(parse_args(["worker", "--x"].map(str::to_owned).into_iter()).is_err());
        for bad in [
            &["paint"][..],
            &["render", "--output", "o.png"],
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
