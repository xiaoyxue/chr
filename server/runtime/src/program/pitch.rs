//! Owner-local pitch-demo-v1 registration. Paths are startup-only, never RPC inputs.
use super::{
    Error,
    synthetic::{Synthetic, id},
};
use crate::{
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
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Local normalized source record. No inferred frame identity or measured K/R.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CameraFixture {
    pub width: u32,
    pub height: u32,
    pub ti: [[f64; 3]; 3],
    pub center: [f64; 3],
    pub pitch: [f64; 2],
    pub source_frame: String,
    pub center_semantics: CenterSemantics,
    pub ground_status: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CenterSemantics {
    Synthetic,
    ConditionalFixedTi,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFixture {
    /// Null explicitly selects the reproducible synthetic multipart block.
    #[serde(deserialize_with = "required_path")]
    pub path: Option<PathBuf>,
    pub model: [[f64; 4]; 4],
    /// Optional explicit native-pixel ground anchor, resolved by the renderer camera.
    pub anchor_pixel: Option<[f64; 2]>,
    /// Optional authored world-AABB height after the supplied linear transform.
    pub height_m: Option<f64>,
}

fn required_path<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<PathBuf>, D::Error> {
    Option::deserialize(deserializer)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub camera: PathBuf,
    pub models: Vec<ModelFixture>,
}

pub struct Prepared {
    pub program: Synthetic,
    pub adapter: SceneAdapter,
    /// Local harness metadata, not a wire extension or asset/path discovery API.
    pub controls: Value,
}

impl Prepared {
    pub fn load(path: &Path, mode: VideoMode, runtime: w::Id) -> Result<Self, Error> {
        let manifest: Manifest = serde_json::from_slice(&fs::read(path)?)?;
        let root = path.parent().unwrap_or(Path::new("."));
        let camera: CameraFixture = serde_json::from_slice(&fs::read(root.join(manifest.camera))?)?;
        let pinhole = Camera::from_ti(
            ImageExtent::new(camera.width, camera.height)?,
            Ti::new(camera.ti)?,
            camera.center,
            0.1,
            10000.0,
        )?;
        let mut models = Vec::new();
        if manifest.models.is_empty() || manifest.models.len() > 8 {
            return Err("pitch-demo-v1 requires 1..8 registered models".into());
        }
        for model in manifest.models {
            let asset = match model.path {
                Some(path) => {
                    let path = root.join(path);
                    if fs::metadata(&path)?.len() > 128 * 1024 * 1024 {
                        return Err("pitch-demo-v1 GLB exceeds 128 MiB".into());
                    }
                    import_glb(&fs::read(path)?)?
                }
                None => block()?,
            };
            log::info!(
                "pitch registered model: vertices={} triangles={} parts={} import_warnings={}",
                asset.mesh.positions().len(),
                asset.mesh.indices().len() / 3,
                asset.mesh.parts().len(),
                asset.warnings.len()
            );
            let mut rows = model.model;
            let initial = ModelMatrix::new(rows)?;
            if let Some(height) = model.height_m {
                if !height.is_finite() || !(0.05..=40.0).contains(&height) {
                    return Err("authored model height must be 0.05..40 metres".into());
                }
                let bounds = asset.mesh.bounds().transformed(initial)?;
                let span = bounds.max()[2] - bounds.min()[2];
                if span <= 0.0 {
                    return Err("model has no vertical extent".into());
                }
                for row in &mut rows[..3] {
                    for coefficient in &mut row[..3] {
                        *coefficient *= height / span;
                    }
                }
            }
            if let Some(pixel) = model.anchor_pixel {
                let ray = pinhole.ray(pixel)?;
                let distance = -ray.origin[2] / ray.direction[2];
                if !distance.is_finite() || distance <= 0.0 {
                    return Err("model anchor has no forward ground intersection".into());
                }
                for (axis, row) in rows.iter_mut().enumerate().take(2) {
                    row[3] = ray.origin[axis] + distance * ray.direction[axis];
                }
            }
            models.push((asset, ModelMatrix::new(rows)?));
        }
        Self::new(camera, models, mode, runtime)
    }

    pub fn new(
        fixture: CameraFixture,
        models: Vec<(GlbAsset, ModelMatrix)>,
        mode: VideoMode,
        runtime: w::Id,
    ) -> Result<Self, Error> {
        if fixture.source_frame.trim().is_empty()
            || fixture.ground_status.trim().is_empty()
            || fixture
                .pitch
                .iter()
                .any(|v| !v.is_finite() || *v <= 0.0 || *v > 1000.0)
            || fixture.width > 4096
            || fixture.height > 2160
            || models.is_empty()
            || models.len() > 8
        {
            return Err("invalid pitch fixture metadata or demo capacity".into());
        }
        let extent = ImageExtent::new(fixture.width, fixture.height)?;
        let camera = Camera::from_ti(extent, Ti::new(fixture.ti)?, fixture.center, 0.1, 10000.0)?;
        let mut config = Synthetic::configuration(mode, &runtime)?;
        config.configuration_id = id(format!("{}-pitch-demo-v1", runtime.as_str()))?;
        config.native_extent = w::Extent {
            width: fixture.width,
            height: fixture.height,
        };
        config.limits.instances_per_scene = 8;
        config.limits.lights_per_scene = 5;
        config.shadows.resolution = 512;
        config.shadows.max_bytes = (32_u64 * 1024 * 1024).into();
        // Native input is bounded separately; output composition remains <= 1280x720.
        config.max_intermediate_bytes = (128_u64 * 1024 * 1024).into();
        let mut registrations = Registrations::default();
        registrations
            .register_world(serde_json::from_value(
                json!({"id":"pitch-world","revision":"1"}),
            )?)
            .map_err(|e| e.to_string())?;
        registrations
            .register_model(id("pitch-receiver")?, receiver(fixture.pitch)?)
            .map_err(|e| e.to_string())?;
        registrations
            .register_model(id("pitch-tower")?, block()?)
            .map_err(|e| e.to_string())?;
        let mut choices = Vec::new();
        for (index, (asset, model)) in models.into_iter().enumerate() {
            let bounds = asset.mesh.bounds();
            let grounded = model.grounded(bounds)?;
            let asset_ref = format!("pitch-model-{index}");
            choices.push(json!({
                "instance":instance("model", &asset_ref, grounded),
                "bounds":{"min":bounds.min(),"max":bounds.max()},
            }));
            registrations
                .register_model(id(asset_ref)?, asset)
                .map_err(|e| e.to_string())?;
        }
        let original = json!({
            "kind":"ti_c","extent":config.native_extent,"near_m":0.1,"far_m":10000.0,
            "ti":fixture.ti.into_iter().flatten().collect::<Vec<_>>(),"camera_center":fixture.center,
        });
        let k = camera.intrinsics();
        let preview = json!({
            "kind":"intrinsics_pose","extent":config.native_extent,"near_m":0.1,"far_m":10000.0,
            "intrinsics":{"fx":k.fx,"fy":k.fy,"skew":k.skew,"cx":k.cx,"cy":k.cy},
            "camera_to_world":camera.camera_to_world(),
        });
        let mut instances = vec![
            instance("pitch", "pitch-receiver", ModelMatrix::IDENTITY),
            choices[0]["instance"].clone(),
        ];
        instances.extend(towers(fixture.pitch, 35.0)?);
        let lighting = lighting(fixture.pitch, 35.0, "spots", true, [[0.0; 2]; 4])?;
        let state = serde_json::from_value(json!({
            "world":{"id":"pitch-world","revision":"1"},"configuration_id":config.configuration_id,
            "camera":{"mode":"manual","camera":original},
            "instances":instances,"lighting":lighting,"shadow_mode":"pcf",
        }))?;
        let (program, adapter) = Synthetic::install_registered(config, registrations, state)?;
        let controls = json!({
            "profile":"pitch-demo-v1",
            "target":{"runtime_id":runtime,"scene_id":program.scene_id},
            "configuration_id":program.configuration.configuration_id,
            "source_frame":fixture.source_frame,"center_semantics":fixture.center_semantics,
            "ground_status":fixture.ground_status,"pitch":fixture.pitch,
            "original_camera":original,"preview_camera":preview,"models":choices,
        });
        Ok(Self {
            program,
            adapter,
            controls,
        })
    }
}

fn instance(name: &str, asset: &str, model: ModelMatrix) -> Value {
    json!({"instance_id":name,"asset_ref":asset,"model":model.rows(),"visible":true,"material_overrides":[]})
}

pub fn towers(pitch: [f64; 2], height: f64) -> Result<Vec<Value>, Error> {
    if !(30.0..=40.0).contains(&height) {
        return Err("tower height must be 30..40 metres".into());
    }
    corners(pitch, height)
        .into_iter()
        .enumerate()
        .map(|(index, p)| {
            Ok(instance(
                &format!("tower-{index}"),
                "pitch-tower",
                ModelMatrix::new([
                    [0.2, 0., 0., p[0]],
                    [0., 0.2, 0., p[1]],
                    // Stop the support below the emitter; a solid pole through
                    // the punctual source would shadow the entire rig.
                    [0., 0., (height - 1.) / 2., 0.],
                    [0., 0., 0., 1.],
                ])?,
            ))
        })
        .collect()
}

fn corners([length, width]: [f64; 2], height: f64) -> [[f64; 3]; 4] {
    [
        [-length / 2., -width / 2., -height],
        [-length / 2., width / 2., -height],
        [length / 2., -width / 2., -height],
        [length / 2., width / 2., -height],
    ]
}

/// Exactly one four-light rig; sun is independent. Targets are pitch XY metres.
pub fn lighting(
    pitch: [f64; 2],
    height: f64,
    rig: &str,
    sun: bool,
    targets: [[f64; 2]; 4],
) -> Result<Value, Error> {
    if !(30.0..=40.0).contains(&height)
        || !["none", "points", "spots"].contains(&rig)
        || targets.iter().flatten().any(|v| !v.is_finite())
    {
        return Err("invalid pitch lighting".into());
    }
    let mut lights = Vec::new();
    if rig != "none" {
        for (index, position) in corners(pitch, height).into_iter().enumerate() {
            let mut light = json!({
                "light_id":format!("tower-light-{index}"),"kind":"point","position":position,
                "intensity":[4000,4200,5000],"source_radius":0.5,"shadow_mode":null,
            });
            if rig == "spots" {
                light["kind"] = json!("spot");
                light["direction"] = json!([
                    targets[index][0] - position[0],
                    targets[index][1] - position[1],
                    height
                ]);
                light["inner_angle"] = json!(0.4);
                light["outer_angle"] = json!(0.9);
            }
            lights.push(light);
        }
    }
    if sun {
        lights.push(
            json!({"light_id":"sun","kind":"directional","direction":[0.2,-0.4,1],
            "irradiance":[2.5,2.35,2.1],"angular_radius":0.00465,"shadow_mode":null}),
        );
    }
    Ok(json!({"lights":lights,"environment":null}))
}

fn material(color: [f32; 4]) -> ImportedMaterial {
    ImportedMaterial {
        factors: PbrMaterial {
            base_color: color,
            roughness: 0.85,
            double_sided: true,
            ..Default::default()
        },
        maps: Default::default(),
    }
}

fn receiver([length, width]: [f64; 2]) -> Result<GlbAsset, Error> {
    let mut positions = Vec::new();
    let mut indices = Vec::new();
    let mut parts = Vec::new();
    let mut materials = Vec::new();
    let mut rectangle = |x: f64, y: f64, sx: f64, sy: f64, z: f64, color| {
        let start = positions.len() as u32;
        positions.extend(
            [
                [-sx / 2. + x, -sy / 2. + y, z],
                [sx / 2. + x, -sy / 2. + y, z],
                [sx / 2. + x, sy / 2. + y, z],
                [-sx / 2. + x, sy / 2. + y, z],
            ]
            .map(|p| p.map(|v| v as f32)),
        );
        let begin = indices.len() as u32;
        indices.extend([0, 2, 1, 0, 3, 2].map(|i| i + start));
        parts.push(begin..indices.len() as u32);
        materials.push(material(color));
    };
    for i in 0..14 {
        rectangle(
            -length / 2. + (i as f64 + 0.5) * length / 14.,
            0.,
            length / 14.,
            width,
            0.,
            if i % 2 == 0 {
                [0.035, 0.19, 0.045, 1.]
            } else {
                [0.045, 0.24, 0.06, 1.]
            },
        );
    }
    for (x, y, sx, sy) in [
        (0., -width / 2., length, 0.16),
        (0., width / 2., length, 0.16),
        (-length / 2., 0., 0.16, width),
        (length / 2., 0., 0.16, width),
        (0., 0., 0.16, width),
    ] {
        rectangle(x, y, sx, sy, -0.025, [0.8, 0.85, 0.75, 1.]);
    }
    Ok(GlbAsset {
        mesh: MeshData::new(positions, indices, parts)?,
        materials,
        warnings: Vec::new(),
    })
}

/// Two material parts, with the ground-facing face at max Z=0.
pub fn block() -> Result<GlbAsset, Error> {
    Ok(GlbAsset {
        mesh: MeshData::new(
            vec![
                [-1., -1., -2.],
                [1., -1., -2.],
                [1., 1., -2.],
                [-1., 1., -2.],
                [-1., -1., 0.],
                [1., -1., 0.],
                [1., 1., 0.],
                [-1., 1., 0.],
            ],
            vec![
                0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2,
                7, 6, 3, 0, 4, 3, 4, 7,
            ],
            vec![0..12, 12..36],
        )?,
        materials: vec![
            material([0.75, 0.25, 0.08, 1.]),
            material([0.7, 0.65, 0.45, 1.]),
        ],
        warnings: Vec::new(),
    })
}
