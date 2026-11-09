mod gpu;
mod media;
mod render;
mod scene;

use anyhow::{Context, Result, bail, ensure};
use gpu::Gpu;
use media::Media;
use render::{RenderFrame, Renderer};
use scene::{Scene, timecode};
use serde_json::json;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Arguments {
    scene: PathBuf,
    output: String,
    frames: u64,
    realtime: bool,
    overlay: bool,
    status: Option<PathBuf>,
    ledger: Option<PathBuf>,
    bypass: bool,
}

fn arguments() -> Result<Arguments> {
    let mut scene = None;
    let mut output = None;
    let mut frames = 0;
    let mut realtime = true;
    let mut overlay = false;
    let mut status = None;
    let mut ledger = None;
    let mut bypass = false;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--scene" => {
                scene = Some(PathBuf::from(
                    args.next().context("--scene requires a path")?,
                ))
            }
            "--output" => output = Some(args.next().context("--output requires a URL or TS path")?),
            "--frames" => frames = args.next().context("--frames requires a count")?.parse()?,
            "--unpaced" => realtime = false,
            "--pace" => realtime = true,
            "--mode" => {
                bypass = match args
                    .next()
                    .context("--mode requires compose or bypass")?
                    .as_str()
                {
                    "compose" => false,
                    "bypass" => true,
                    _ => bail!("Expected --mode compose or --mode bypass"),
                };
            }
            "--timecode-overlay" => overlay = true,
            "--status" => {
                status = Some(PathBuf::from(
                    args.next().context("--status requires a path")?,
                ))
            }
            "--ledger" => {
                ledger = Some(PathBuf::from(
                    args.next().context("--ledger requires a path")?,
                ))
            }
            "--help" | "-h" => {
                println!(
                    "chr --scene SCENE.json --output OUTPUT.ts|srt://HOST:PORT?mode=listener \
                     [--frames N] [--unpaced] [--timecode-overlay] [--status STATUS.json] \
                     [--mode compose|bypass] [--ledger LEDGER.jsonl]\n\
                     Frames remain on the GPU. The default is paced, looping 30 fps playback."
                );
                std::process::exit(0);
            }
            _ => bail!("Unknown argument: {argument}"),
        }
    }
    ensure!(
        ledger.is_none() || frames > 0,
        "A ledger requires a bounded --frames run"
    );
    Ok(Arguments {
        scene: scene.context("--scene is required; run tools/prepare_scene.py first")?,
        output: output.context("--output is required")?,
        frames,
        realtime,
        overlay,
        status,
        ledger,
        bypass,
    })
}

fn write_status(path: &Option<PathBuf>, value: serde_json::Value) -> Result<()> {
    if let Some(path) = path {
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
        std::fs::rename(temporary, path)?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = arguments()?;
    let scene = Scene::load(&args.scene)?;
    let gpu = Gpu::new()?;
    eprintln!(
        "Adapter: {}; backend: Vulkan; video pixel readback: disabled",
        gpu.name
    );
    let renderer = Renderer::new(&gpu, &scene)?;
    let media = Media::open(&gpu, &scene.video)?;
    let mut ledger = args
        .ledger
        .as_ref()
        .map(|path| std::fs::File::create(path).map(BufWriter::new))
        .transpose()?;
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    write_status(
        &args.status,
        json!({
            "state": "waiting_for_output", "adapter": gpu.name, "epoch": epoch,
            "backend": "wgpu-vulkan", "output": args.output,
            "hardware_decode": true, "hardware_encode": true, "host_video_frames": false,
        }),
    )?;
    media.output(&args.output)?;
    let clock = Instant::now();
    let mut sequence = 0u64;
    let mut expected_source = 0usize;
    let mut total_processing = Duration::ZERO;
    loop {
        if Media::stopped() || (args.frames != 0 && sequence >= args.frames) {
            break;
        }
        if args.realtime {
            let deadline = clock + Duration::from_secs_f64(sequence as f64 / 30.0);
            if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        let began = Instant::now();
        let Some(info) = media.next()? else {
            ensure!(
                expected_source == scene.frames.len(),
                "Unexpected early source EOF"
            );
            media.rewind()?;
            expected_source = 0;
            continue;
        };
        let ordinal = scene.frame_index(&info)?;
        ensure!(
            ordinal == expected_source,
            "Source discontinuity; refusing stale pose/mask"
        );
        media.copy_input(&renderer.y, &renderer.uv)?;
        renderer.render(
            &gpu,
            &media,
            &scene,
            RenderFrame {
                ordinal,
                sequence,
                color_mode: info.color_mode,
                overlay: args.overlay,
                bypass: args.bypass,
            },
        )?;
        media.encode(&renderer.packed, sequence)?;
        if let Some(ledger) = &mut ledger {
            writeln!(
                ledger,
                "{}",
                json!({
                    "sequence": sequence, "source_ordinal": ordinal, "source_pts": info.pts,
                    "time_base": [info.time_base_num, info.time_base_den],
                    "present_index": scene.frames[ordinal].present_index,
                    "ti_row_index": scene.frames[ordinal].ti_row_index,
                    "timecode": timecode(sequence), "mode": if args.bypass { "bypass" } else { "compose" },
                })
            )?;
        }
        total_processing += began.elapsed();
        sequence += 1;
        expected_source += 1;
        if sequence == 1 || sequence.is_multiple_of(30) {
            let value = json!({
                "state": "streaming", "backend": "wgpu-vulkan", "adapter": gpu.name,
                "epoch": epoch, "frames": sequence, "source_ordinal": ordinal,
                "source_pts": info.pts,
                "source_time_base": [info.time_base_num, info.time_base_den],
                "present_index": scene.frames[ordinal].present_index,
                "ti_row_index": scene.frames[ordinal].ti_row_index,
                "timecode": timecode(sequence - 1), "timecode_mode": "30fps NDF",
                "output_pts": sequence - 1, "output_time_base": [1, 30],
                "elapsed_seconds": clock.elapsed().as_secs_f64(),
                "average_processing_ms": total_processing.as_secs_f64() * 1000.0 / sequence as f64,
                "hardware_decode": true, "hardware_encode": true,
                "host_video_frames": false, "gpu_local_plane_copies_per_frame": 4,
                "timestamp_mismatches": 0, "timecode_overlay": args.overlay,
            });
            write_status(&args.status, value.clone())?;
            if sequence == 1 || sequence.is_multiple_of(300) {
                println!("{value}");
            }
        }
    }
    if !Media::stopped() {
        media.finish()?;
    }
    if let Some(ledger) = &mut ledger {
        ledger.flush()?;
    }
    let completed = json!({
        "state": "complete", "frames": sequence, "adapter": gpu.name,
        "elapsed_seconds": clock.elapsed().as_secs_f64(),
        "average_processing_ms": total_processing.as_secs_f64() * 1000.0 / sequence.max(1) as f64,
        "timestamp_mismatches": 0, "host_video_frames": false,
    });
    write_status(&args.status, completed.clone())?;
    println!("{completed}");
    Ok(())
}
