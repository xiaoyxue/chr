//! Startup-only calibrated clip replay. No private paths or lens data enter RPC.
use super::{
    Error,
    synthetic::{Synthetic, id},
};
use crate::{
    calibration::{
        replay::{Radial, VideoLens},
        tracking::{Tracked, TrackingReplay},
    },
    retained::{Registrations, SceneAdapter},
    sdi::VideoMode,
};
use chr_common::scene_frame as w;
use chr_renderer::{
    camera::Camera as RendererCamera,
    coordinates::{ImageExtent, Ti},
    glb::{GlbAsset, ImportedMaterial, import_glb},
    scene::{MeshData, ModelMatrix, PbrMaterial},
};
use serde::Deserialize;
use serde_json::json;
use std::{fs, path::Path, path::PathBuf, sync::Arc};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub ti: [[f64; 3]; 3],
    pub center: [f64; 3],
    pub radial: Radial,
    /// Presentation-order reference-native-pixel -> frame-SNC matrices. A tracked
    /// program receives them, and the lens, per frame instead and ignores this
    /// list; only `chr-sdi-sim`, the `chr-tracking-sim-cli` replay and still
    /// renders replay it.
    #[serde(default)]
    pub frame_warps: Vec<[[f64; 3]; 3]>,
    pub model: PathBuf,
    pub height_m: f64,
    /// Ideal reference native pixel, not a distorted frame pixel.
    pub anchor_pixel: [f64; 2],
    pub pitch_m: [f64; 2],
}

/// How a program built from the manifest obtains each frame's camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraSource {
    /// `calibrated-video-v1`: the manifest's fixed `Ti`/`C` and its warp of
    /// `sequence % frame_warps`.
    Replay,
    /// `tracked-video-v1`: every frame carries `Ti`, `C`, its lens and warp; the
    /// manifest's `Ti`/`C` only place the model and its lens is not applied.
    Tracked,
}

pub fn load(
    path: &Path,
    mode: VideoMode,
    runtime: w::Id,
) -> Result<(Synthetic, SceneAdapter), Error> {
    load_with(path, mode, runtime, CameraSource::Replay)
}

pub fn load_with(
    path: &Path,
    mode: VideoMode,
    runtime: w::Id,
    camera: CameraSource,
) -> Result<(Synthetic, SceneAdapter), Error> {
    if fs::metadata(path)?.len() > 16 * 1024 * 1024 {
        return Err("video scene manifest exceeds 16 MiB".into());
    }
    let manifest: Manifest = serde_json::from_slice(&fs::read(path)?)?;
    let model = path
        .parent()
        .unwrap_or(Path::new("."))
        .join(&manifest.model);
    if fs::metadata(&model)?.len() > 128 * 1024 * 1024 {
        return Err("video scene GLB exceeds 128 MiB".into());
    }
    let asset = import_glb(&fs::read(model)?)?;
    prepare_with(manifest, asset, mode, runtime, camera)
}

pub fn prepare(
    manifest: Manifest,
    asset: GlbAsset,
    mode: VideoMode,
    runtime: w::Id,
) -> Result<(Synthetic, SceneAdapter), Error> {
    prepare_with(manifest, asset, mode, runtime, CameraSource::Replay)
}

pub fn prepare_with(
    manifest: Manifest,
    asset: GlbAsset,
    mode: VideoMode,
    runtime: w::Id,
    kind: CameraSource,
) -> Result<(Synthetic, SceneAdapter), Error> {
    let e = ImageExtent::new(manifest.width, manifest.height)?;
    if !super::fits(manifest.width, manifest.height, super::MAX_EXTENT)
        || manifest.fps != mode.fps
        || u64::from(manifest.width) * u64::from(mode.height)
            != u64::from(manifest.height) * u64::from(mode.width)
        || (kind == CameraSource::Replay && manifest.frame_warps.is_empty())
        || manifest.frame_warps.len() > 36_000
        || !manifest.height_m.is_finite()
        || !(0.05..=40.0).contains(&manifest.height_m)
        || manifest
            .pitch_m
            .iter()
            .any(|v| !v.is_finite() || !(1.0..=1000.0).contains(v))
    {
        return Err("invalid calibrated video extent, rate, frames or scene dimensions".into());
    }
    let camera = RendererCamera::from_ti(e, Ti::new(manifest.ti)?, manifest.center, 0.1, 10000.0)?;
    let lens = manifest.radial.wire()?;
    let replay = TrackingReplay::new(manifest.ti, manifest.center, lens, &manifest.frame_warps)?;
    let models = match kind {
        CameraSource::Replay => manifest.radial.models(e, &manifest.frame_warps)?,
        CameraSource::Tracked => Vec::new(),
    };
    let ray = camera.ray(manifest.anchor_pixel)?;
    let distance = -ray.origin[2] / ray.direction[2];
    if !distance.is_finite() || distance <= 0.0 {
        return Err("video model anchor has no forward ground intersection".into());
    }
    let anchor = std::array::from_fn::<_, 2, _>(|i| ray.origin[i] + distance * ray.direction[i]);
    let bounds = asset.mesh.bounds();
    let scale = manifest.height_m / (bounds.max()[2] - bounds.min()[2]);
    let model = ModelMatrix::new([
        [scale, 0., 0., anchor[0]],
        [0., scale, 0., anchor[1]],
        [0., 0., scale, 0.],
        [0., 0., 0., 1.],
    ])?
    .grounded(bounds)?;
    let mut config = Synthetic::configuration(mode, &runtime)?;
    config.configuration_id = id(format!(
        "{}-{}",
        runtime.as_str(),
        match kind {
            CameraSource::Replay => "calibrated-video-v1",
            CameraSource::Tracked => "tracked-video-v1",
        }
    ))?;
    config.native_extent = w::Extent {
        width: manifest.width,
        height: manifest.height,
    };
    config.sample_count = 4;
    config.limits.lights_per_scene = 5;
    config.shadows.resolution = 2048;
    config.shadows.max_bytes = (128_u64 * 1024 * 1024).into();
    config.max_intermediate_bytes =
        super::composition_budget(config.native_extent, config.output_extent).into();
    let mut registrations = Registrations::default();
    let world: w::Version = serde_json::from_value(json!({"id":"video-world","revision":"1"}))?;
    registrations
        .register_world(world.clone())
        .map_err(|e| e.to_string())?;
    log::info!(
        "calibrated video model: vertices={} triangles={} parts={} import_warnings={}",
        asset.mesh.positions().len(),
        asset.mesh.indices().len() / 3,
        asset.mesh.parts().len(),
        asset.warnings.len()
    );
    registrations
        .register_model(id("video-model")?, asset)
        .map_err(|e| e.to_string())?;
    let [x, y] = manifest.pitch_m.map(|v| v * 1.5);
    registrations
        .register_model(
            id("video-receiver")?,
            GlbAsset {
                mesh: MeshData::new(
                    vec![[-x, -y, 0.], [x, -y, 0.], [x, y, 0.], [-x, y, 0.]]
                        .into_iter()
                        .map(|p| p.map(|v| v as f32))
                        .collect(),
                    vec![0, 2, 1, 0, 3, 2],
                    std::iter::once(0..6).collect(),
                )?,
                materials: vec![ImportedMaterial {
                    factors: PbrMaterial {
                        shadow_catcher: true,
                        double_sided: true,
                        ..Default::default()
                    },
                    maps: Default::default(),
                }],
                warnings: Vec::new(),
            },
        )
        .map_err(|e| e.to_string())?;
    let mut lighting = super::pitch::lighting(manifest.pitch_m, 35., "spots", true, [anchor; 4])?;
    for light in lighting["lights"].as_array_mut().expect("native lighting") {
        if light["kind"] == "spot" {
            light["intensity"] = json!([16000, 16800, 20000]);
            // Aim each cone at the model instead of the pitch: the shadow map covers
            // only the cone, so a tight cone puts its resolution on the model.
            let position: [f64; 3] = serde_json::from_value(light["position"].clone())?;
            let distance = ((position[0] - anchor[0]).powi(2)
                + (position[1] - anchor[1]).powi(2)
                + position[2].powi(2))
            .sqrt();
            light["inner_angle"] = json!((1.1 * manifest.height_m / distance).atan());
            light["outer_angle"] = json!((1.5 * manifest.height_m / distance).atan());
        } else {
            light["direction"] = json!([0.5, 0.6, 1.0]);
        }
    }
    let scene_camera = match kind {
        CameraSource::Replay => {
            json!({"mode":"manual","camera":{"kind":"ti_c","extent":config.native_extent,
            "near_m":0.1,"far_m":10000.0,"ti":manifest.ti.into_iter().flatten().collect::<Vec<_>>(),
            "camera_center":manifest.center}})
        }
        CameraSource::Tracked => json!({"mode":"tracked","near_m":0.1,"far_m":10000.0}),
    };
    let extent = config.native_extent;
    let state = serde_json::from_value(json!({
        "world":world,"configuration_id":config.configuration_id,
        "camera":scene_camera,
        "instances":[
            {"instance_id":"mesh","asset_ref":"video-model","model":model.rows(),"visible":true,"material_overrides":[]},
            {"instance_id":"ground","asset_ref":"video-receiver","model":ModelMatrix::IDENTITY.rows(),"visible":true,"material_overrides":[]}
        ],
        "lighting":lighting,"shadow_mode":"pcss"
    }))?;
    let (mut program, adapter) = Synthetic::install_registered(config, registrations, state)?;
    match kind {
        CameraSource::Replay => {
            program.video_lens = Some(Arc::new(VideoLens::new(program.stream_id.clone(), models)));
        }
        CameraSource::Tracked => {
            program.tracking = Some(Tracked {
                world,
                extent,
                replay: Some(replay),
            });
        }
    }
    Ok((program, adapter))
}
