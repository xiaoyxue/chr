//! Startup-only calibrated clip replay. No private paths or lens data enter RPC.
use super::{
    Error,
    synthetic::{Synthetic, id},
};
use crate::{
    calibration::replay::{Radial, VideoLens},
    retained::{Registrations, SceneAdapter},
    sdi::VideoMode,
};
use chr_common::scene_frame as w;
use chr_renderer::{
    camera::Camera,
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
    /// Presentation-order reference-native-pixel -> frame-SNC matrices.
    pub frame_warps: Vec<[[f64; 3]; 3]>,
    pub model: PathBuf,
    pub height_m: f64,
    /// Ideal reference native pixel, not a distorted frame pixel.
    pub anchor_pixel: [f64; 2],
    pub pitch_m: [f64; 2],
}

pub fn load(
    path: &Path,
    mode: VideoMode,
    runtime: w::Id,
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
    prepare(manifest, asset, mode, runtime)
}

pub fn prepare(
    manifest: Manifest,
    asset: GlbAsset,
    mode: VideoMode,
    runtime: w::Id,
) -> Result<(Synthetic, SceneAdapter), Error> {
    let e = ImageExtent::new(manifest.width, manifest.height)?;
    if manifest.width > 4096
        || manifest.height > 2160
        || !manifest.width.is_multiple_of(2)
        || !manifest.height.is_multiple_of(2)
        || manifest.fps != mode.fps
        || u64::from(manifest.width) * u64::from(mode.height)
            != u64::from(manifest.height) * u64::from(mode.width)
        || manifest.frame_warps.is_empty()
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
    let camera = Camera::from_ti(e, Ti::new(manifest.ti)?, manifest.center, 0.1, 10000.0)?;
    let models = manifest.radial.models(e, &manifest.frame_warps)?;
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
    config.configuration_id = id(format!("{}-calibrated-video-v1", runtime.as_str()))?;
    config.native_extent = w::Extent {
        width: manifest.width,
        height: manifest.height,
    };
    config.sample_count = 4;
    config.limits.lights_per_scene = 5;
    config.shadows.resolution = 2048;
    config.shadows.max_bytes = (128_u64 * 1024 * 1024).into();
    config.max_intermediate_bytes = (256_u64 * 1024 * 1024).into();
    let mut registrations = Registrations::default();
    registrations
        .register_world(serde_json::from_value(
            json!({"id":"video-world","revision":"1"}),
        )?)
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
    let state = serde_json::from_value(json!({
        "world":{"id":"video-world","revision":"1"},"configuration_id":config.configuration_id,
        "camera":{"mode":"manual","camera":{"kind":"ti_c","extent":config.native_extent,
            "near_m":0.1,"far_m":10000.0,"ti":manifest.ti.into_iter().flatten().collect::<Vec<_>>(),
            "camera_center":manifest.center}},
        "instances":[
            {"instance_id":"mesh","asset_ref":"video-model","model":model.rows(),"visible":true,"material_overrides":[]},
            {"instance_id":"ground","asset_ref":"video-receiver","model":ModelMatrix::IDENTITY.rows(),"visible":true,"material_overrides":[]}
        ],
        "lighting":lighting,"shadow_mode":"pcss"
    }))?;
    let (mut program, adapter) = Synthetic::install_registered(config, registrations, state)?;
    program.video_lens = Some(Arc::new(VideoLens::new(program.stream_id.clone(), models)));
    Ok((program, adapter))
}
