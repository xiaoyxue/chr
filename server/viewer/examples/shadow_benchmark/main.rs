//! Bounded synthetic benchmark; run from the workspace root. No owner assets.
mod reference;
mod timing;

use std::{fs, time::Instant};

use chr_renderer::{
    Gpu, MeshRenderer, OffscreenTarget, PbrSettings, ShadowMode, ShadowSettings, ToneMap,
};
use reference::{Fixture, Kind, Quality};
use serde::Serialize;
use serde_json::json;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct Options {
    repetitions: usize,
    resolution: u32,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut result = Self {
            repetitions: 3,
            resolution: 512,
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--repetitions" => {
                    result.repetitions = args.next().ok_or("missing repetitions")?.parse()?
                }
                "--resolution" => {
                    result.resolution = args.next().ok_or("missing resolution")?.parse()?
                }
                _ => {
                    return Err(format!(
                        "unknown argument {arg}; use --repetitions 1..20 --resolution 256|512|1024"
                    )
                    .into());
                }
            }
        }
        if !(1..=20).contains(&result.repetitions) || ![256, 512, 1024].contains(&result.resolution)
        {
            return Err("benchmark bounds: repetitions 1..20; resolution 256, 512 or 1024".into());
        }
        Ok(result)
    }
}

#[derive(Clone, Copy)]
struct Configuration {
    mode: ShadowMode,
    samples: u32,
}

impl Configuration {
    fn settings(self, resolution: u32, kind: Kind) -> ShadowSettings {
        let mut settings = ShadowSettings {
            mode: self.mode,
            resolution,
            filter_radius: 2.5,
            pcf_samples: if self.mode == ShadowMode::Pcf {
                self.samples
            } else {
                9
            },
            depth_bias: 0.00005,
            normal_bias: 0.0,
            max_bytes: map_bytes(resolution, kind.layers()),
            max_samples_per_fragment: self.budget(kind),
            ..Default::default()
        };
        if self.mode == ShadowMode::Pcss {
            settings.pcss.blocker_samples = self.samples;
            settings.pcss.filter_samples = self.samples;
        }
        settings.cascades.count = kind.cascades();
        settings.cascades.max_distance = 12.0;
        settings
    }

    fn budget(self, kind: Kind) -> u64 {
        u64::from(self.samples)
            * if self.mode == ShadowMode::Pcss { 2 } else { 1 }
            * if kind.cascades() > 1 { 2 } else { 1 }
    }
}

fn configurations() -> Vec<Configuration> {
    std::iter::once(Configuration {
        mode: ShadowMode::Hard,
        samples: 1,
    })
    .chain(
        [1, 4, 9, 16, 25, 36, 49, 64]
            .into_iter()
            .map(|samples| Configuration {
                mode: ShadowMode::Pcf,
                samples,
            }),
    )
    .chain([8, 32, 64].into_iter().map(|samples| Configuration {
        mode: ShadowMode::Pcss,
        samples,
    }))
    .collect()
}

fn map_bytes(resolution: u32, layers: u32) -> u64 {
    u64::from(resolution).pow(2) * u64::from(layers) * 4
}

#[derive(Serialize)]
struct Measurements {
    gpu_ms: Option<Vec<f64>>,
    gpu_median_ms: Option<f64>,
    gpu_min_ms: Option<f64>,
    gpu_max_ms: Option<f64>,
    cpu_prepare_ms: Vec<f64>,
    depth_passes: Vec<u32>,
}

impl Measurements {
    fn new(supported: bool) -> Self {
        Self {
            gpu_ms: supported.then(Vec::new),
            gpu_median_ms: None,
            gpu_min_ms: None,
            gpu_max_ms: None,
            cpu_prepare_ms: Vec::new(),
            depth_passes: Vec::new(),
        }
    }

    fn summarize(&mut self) {
        if let Some(samples) = &self.gpu_ms {
            let mut sorted = samples.clone();
            sorted.sort_by(f64::total_cmp);
            self.gpu_min_ms = sorted.first().copied();
            self.gpu_max_ms = sorted.last().copied();
            self.gpu_median_ms = (!sorted.is_empty())
                .then(|| (sorted[(sorted.len() - 1) / 2] + sorted[sorted.len() / 2]) * 0.5);
        }
    }
}

#[derive(Serialize)]
struct Row {
    light: String,
    scene: String,
    mode: String,
    samples: u32,
    logical_sample_budget_per_fragment: u64,
    shadow_map_bytes: u64,
    shadow_layers: u32,
    directional_cascades: u32,
    angular_radius_radians: Option<f64>,
    source_radius_metres: Option<f64>,
    caster_centres_metres: Vec<[f64; 3]>,
    camera_centre_metres: [f64; 3],
    cold: Measurements,
    warm: Measurements,
    quality: Quality,
    warm_pixels_identical: bool,
}

fn prepare(
    gpu: &Gpu,
    renderer: &mut MeshRenderer,
    fixture: &Fixture,
    shadows: ShadowSettings,
) -> Result<()> {
    renderer
        .prepare_pbr(
            gpu,
            &fixture.scene.snapshot(),
            &fixture.camera,
            PbrSettings {
                shadows,
                tone_map: ToneMap::Reinhard,
                ..Default::default()
            },
        )
        .map_err(|e| e as Box<dyn std::error::Error>)
}

fn pixels(gpu: &Gpu, renderer: &mut MeshRenderer, target: &OffscreenTarget) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    target
        .render_rgba(
            gpu,
            |encoder, view| renderer.encode(encoder, view),
            &mut result,
        )
        .map_err(|e| e as Box<dyn std::error::Error>)?;
    Ok(result)
}

fn run(options: Options) -> Result<()> {
    let (gpu, timer) = pollster::block_on(timing::request())?;
    let info = gpu.adapter.get_info();
    let hardware = matches!(
        info.device_type,
        wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
    );
    eprintln!(
        "adapter={} backend={:?} hardware={hardware} timestamps={}",
        info.name,
        info.backend,
        timer.is_some()
    );
    let target = OffscreenTarget::new(&gpu, chr_renderer::coordinates::ImageExtent::new(384, 384)?)
        .map_err(|e| e as Box<dyn std::error::Error>)?;
    let mut renderer = MeshRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for kind in Kind::ALL {
        for scene in [
            "contact",
            "separated",
            "large_penumbra",
            "multiple_casters",
            "grazing_view",
        ] {
            let mut fixture = Fixture::new(kind, scene)?;
            let probes = fixture.probes()?;
            prepare(&gpu, &mut renderer, &fixture, ShadowSettings::default())?;
            let off = pixels(&gpu, &mut renderer, &target)?;
            let mut pcf_single = Vec::new();
            let mut pcss_low = Vec::new();
            for config in configurations() {
                let settings = config.settings(options.resolution, kind);
                prepare(&gpu, &mut renderer, &fixture, settings)?;
                // Untimed priming includes first-use pipelines and depth-map allocation.
                let _ = pixels(&gpu, &mut renderer, &target)?;
                let mut cold = Measurements::new(timer.is_some());
                let mut warm = Measurements::new(timer.is_some());
                for _ in 0..options.repetitions {
                    fixture.invalidate()?;
                    for (is_cold, measurement) in [(true, &mut cold), (false, &mut warm)] {
                        let start = Instant::now();
                        prepare(&gpu, &mut renderer, &fixture, settings)?;
                        measurement
                            .cpu_prepare_ms
                            .push(start.elapsed().as_secs_f64() * 1000.0);
                        target
                            .render(&gpu, |encoder, view| {
                                if let Some(timer) = &timer {
                                    timer.begin(encoder);
                                }
                                renderer.encode(encoder, view);
                                if let Some(timer) = &timer {
                                    timer.end(encoder);
                                }
                            })
                            .map_err(|e| e as Box<dyn std::error::Error>)?;
                        if let (Some(timer), Some(samples)) = (&timer, &mut measurement.gpu_ms) {
                            samples.push(timer.read(&gpu)?);
                        }
                        let passes = renderer.shadow_passes_last_frame();
                        measurement.depth_passes.push(passes);
                        let expected = if is_cold { kind.layers() } else { 0 };
                        if passes != expected {
                            return Err(format!(
                                "depth cache invariant: expected {expected}, got {passes}"
                            )
                            .into());
                        }
                    }
                }
                cold.summarize();
                warm.summarize();
                fixture.invalidate()?;
                prepare(&gpu, &mut renderer, &fixture, settings)?;
                let image = pixels(&gpu, &mut renderer, &target)?;
                prepare(&gpu, &mut renderer, &fixture, settings)?;
                let repeated = pixels(&gpu, &mut renderer, &target)?;
                let identical = image == repeated && renderer.shadow_passes_last_frame() == 0;
                let quality = reference::quality(&probes, &off, &image)?;
                let label = format!("{kind:?}/{scene}/{:?}/{}", config.mode, config.samples);
                if scene == "separated" {
                    match (config.mode, config.samples) {
                        (ShadowMode::Pcf, 1) => pcf_single = image.clone(),
                        (ShadowMode::Pcss, 8) => pcss_low = image.clone(),
                        (ShadowMode::Pcf, 64) if image == pcf_single => {
                            failures
                                .push(format!("{label}: PCF sample count did not change pixels"));
                        }
                        (ShadowMode::Pcss, 64) if image == pcss_low => {
                            failures
                                .push(format!("{label}: PCSS sample count did not change pixels"));
                        }
                        _ => {}
                    }
                }
                if !identical
                    || quality.max_visibility > 1.05
                    || quality.reference_48_vs_96_mae > 0.01
                {
                    failures.push(format!(
                        "{label}: determinism/visibility/reference-convergence check failed"
                    ));
                }
                if config.mode == ShadowMode::Hard && quality.point_emitter_mae > 0.15 {
                    failures.push(format!("{label}: hard point-reference MAE exceeds 0.15"));
                }
                if config.mode == ShadowMode::Pcss
                    && config.samples >= 32
                    && quality.finite_emitter_mae > 0.15
                {
                    failures.push(format!("{label}: PCSS finite-reference MAE exceeds 0.15"));
                }
                rows.push(Row {
                    light: format!("{kind:?}"),
                    scene: scene.into(),
                    mode: format!("{:?}", config.mode),
                    samples: config.samples,
                    logical_sample_budget_per_fragment: config.budget(kind),
                    shadow_map_bytes: map_bytes(options.resolution, kind.layers()),
                    shadow_layers: kind.layers(),
                    directional_cascades: if kind.directional() {
                        kind.cascades()
                    } else {
                        0
                    },
                    angular_radius_radians: kind.directional().then_some(fixture.radius),
                    source_radius_metres: (!kind.directional()).then_some(fixture.radius),
                    caster_centres_metres: fixture.blockers.iter().map(|b| b.center).collect(),
                    camera_centre_metres: fixture.camera.center(),
                    cold,
                    warm,
                    quality,
                    warm_pixels_identical: identical,
                });
            }
            eprintln!("completed {kind:?}/{scene}");
        }
    }
    for kind in Kind::ALL {
        let light = format!("{kind:?}");
        let partial: Vec<usize> = ["contact", "separated", "large_penumbra"]
            .into_iter()
            .map(|scene| {
                rows.iter()
                    .find(|row| {
                        row.light == light
                            && row.scene == scene
                            && row.mode == "Pcss"
                            && row.samples == 64
                    })
                    .map(|row| row.quality.partial_pixels)
                    .ok_or("missing PCSS trend row")
            })
            .collect::<std::result::Result<_, _>>()?;
        if partial[0] >= partial[1] || partial[1] >= partial[2] {
            failures.push(format!(
                "{light}: PCSS contact/separation/source-size penumbra trend failed: {partial:?}"
            ));
        }
    }
    let report = json!({
        "schema_version": 1,
        "adapter": { "name": info.name, "backend": format!("{:?}", info.backend),
            "device_type": format!("{:?}", info.device_type), "driver": info.driver,
            "driver_info": info.driver_info, "hardware": hardware },
        "timestamps": { "status": if timer.is_some() { "supported" } else { "unsupported: TIMESTAMP_QUERY + TIMESTAMP_QUERY_INSIDE_ENCODERS required" },
            "period_ns": timer.as_ref().map(|_| gpu.queue.get_timestamp_period()),
            "scope": "MeshRenderer::encode: shadow depth (cold only), main HDR mesh, tone map; excludes prepare/uploads, allocation, readback, query resolve" },
        "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        "extent": [384, 384], "shadow_resolution": options.resolution,
        "geometry": { "ground_width_metres": 8, "caster_width_metres": 1, "camera_vertical_fov_radians": 1 },
        "repetitions": options.repetitions, "untimed_priming_frames_per_row": 1,
        "settings": { "filter_radius_texels": 2.5, "depth_bias": 0.00005, "normal_bias_metres": 0.0,
            "pcss_max_search_radius_texels": 64, "pcss_max_filter_radius_texels": 64,
            "cascade_max_distance_metres": 12, "cascade_split_lambda": 0.5, "cascade_blend_fraction": 0.1 },
        "cold_definition": "new scene revision with identical caster model; resident map allocation/pipelines retained",
        "memory_definition": "Depth32Float texel payload only; excludes allocation padding, main targets, meshes, query buffers and driver overhead",
        "sample_definition": "worst-case logical comparisons + PCSS blocker fetches for one light; doubled for blended cascades, not physical texture fetches or measured sample count",
        "quality_definition": "receiver-strip red-channel visibility ratio after inverse sRGB/Reinhard; independent uniform disk visibility quadrature, not BRDF-weighted area-light transport",
        "limitations": ["synthetic quads, one light, modest fixed image/scene scale",
            "timings may include concurrent GPU contention, clocks and query overhead; not a realtime guarantee",
            "no dedicated offscreen caster, cubemap seam sweep, temporal camera sequence, LTC, atmosphere or sky evaluation",
            "point reference uses a facing emitter disk, not sphere radiometric integration"],
        "self_checks_passed": failures.is_empty(), "failures": failures, "rows": rows,
    });
    fs::create_dir_all("output/renderer")?;
    fs::write(
        "output/renderer/shadow-benchmark.json",
        serde_json::to_vec_pretty(&report)?,
    )?;
    eprintln!(
        "wrote {} rows to output/renderer/shadow-benchmark.json; failures={}",
        rows.len(),
        failures.len()
    );
    if !failures.is_empty() {
        return Err(failures.join("; ").into());
    }
    if !hardware {
        return Err(
            "software/unknown adapter results do not qualify as hardware acceptance".into(),
        );
    }
    if timer.is_none() {
        return Err(
            "quality results saved, but GPU timing is unsupported (no CPU fallback)".into(),
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    run(Options::parse(std::env::args().skip(1))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_arguments_and_quality_budgets() {
        for args in [
            vec!["--repetitions", "0"],
            vec!["--repetitions", "21"],
            vec!["--resolution", "0"],
            vec!["--resolution", "4096"],
            vec!["--repetitions"],
            vec!["--unknown"],
        ] {
            assert!(Options::parse(args.into_iter().map(str::to_string)).is_err());
        }
        let options =
            Options::parse(["--repetitions", "5", "--resolution", "1024"].map(str::to_string))
                .unwrap();
        assert_eq!(options.repetitions, 5);
        for config in configurations() {
            let settings = config.settings(512, Kind::Point);
            assert_eq!(settings.max_bytes, 6 * 512 * 512 * 4);
            let expected = match config.mode {
                ShadowMode::Hard => 1,
                ShadowMode::Pcf => u64::from(settings.pcf_samples),
                ShadowMode::Pcss => {
                    u64::from(settings.pcss.blocker_samples + settings.pcss.filter_samples)
                }
                ShadowMode::Off => unreachable!(),
            };
            assert_eq!(settings.max_samples_per_fragment, expected);
            let cascaded = config.settings(512, Kind::DirectionalCascaded);
            assert_eq!(cascaded.max_bytes, 4 * 512 * 512 * 4);
            assert_eq!(cascaded.max_samples_per_fragment, expected * 2);
        }
    }

    #[test]
    fn timing_summary_preserves_samples_and_even_median() {
        let mut measurement = Measurements::new(true);
        measurement.gpu_ms = Some(vec![4.0, 1.0, 2.0, 3.0]);
        measurement.summarize();
        assert_eq!(measurement.gpu_median_ms, Some(2.5));
        assert_eq!(measurement.gpu_min_ms, Some(1.0));
        assert_eq!(measurement.gpu_max_ms, Some(4.0));
        assert_eq!(measurement.gpu_ms, Some(vec![4.0, 1.0, 2.0, 3.0]));
        let mut unsupported = Measurements::new(false);
        unsupported.summarize();
        assert_eq!(unsupported.gpu_median_ms, None);
    }
}
