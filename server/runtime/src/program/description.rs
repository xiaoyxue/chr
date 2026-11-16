//! `chr-render-description-v1` program: a described scene seen by a tracked camera.
//!
//! Startup (`--description <json>`) fixes the native extent, rate and lens, imports
//! the scene's assets and creates the scene. At runtime `description.load` replaces
//! the scene: new assets are read from inside the `--asset-root` folders, imported
//! and registered (with the server and with every render worker it starts), then the
//! program scene is recreated from the description.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use chr_common::{
    description::{self as d, AssetSource, FilePath},
    jsonrpc::ErrorObject,
    scene_frame as w,
};
use chr_renderer::{
    glb::{GlbAsset, ImportedMaterial},
    scene::{MeshData, PbrMaterial},
};
use serde_json::{Value, json};

use super::{
    Error,
    assets::{self, Paths},
    synthetic::{Synthetic, id},
};
use crate::{
    calibration::{
        replay::Radial,
        tracking::{Tracked, TrackingReplay},
    },
    retained::{Registrations, SceneAdapter, SceneControl, incompatible, reject},
    sdi::VideoMode,
    worker::supervisor::WorkerAssets,
};

/// Asset and instance ID of the ground shadow catcher; descriptions cannot reuse it.
pub const GROUND: &str = "chr-ground";
/// Most instances a description may place, besides the ground.
pub const MAX_INSTANCES: usize = 63;
/// Most assets a running server registers over its lifetime.
pub const MAX_ASSETS: usize = 64;
/// Most bytes of runtime asset copies a server keeps for its render workers.
pub const MAX_STORED_BYTES: u64 = 1024 * 1024 * 1024;

pub fn load(
    path: &Path,
    mode: VideoMode,
    runtime: w::Id,
) -> Result<(Synthetic, SceneAdapter), Error> {
    if std::fs::metadata(path)?.len() > 16 * 1024 * 1024 {
        return Err("render description exceeds 16 MiB".into());
    }
    let description: d::RenderDescription = serde_json::from_slice(&std::fs::read(path)?)?;
    let base = path.parent().unwrap_or(Path::new("."));
    let assets = description
        .scene
        .assets
        .iter()
        .map(|asset| {
            Ok((
                asset.id.clone(),
                assets::load(asset, &Paths::Startup { base })?,
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    prepare(&description, assets, mode, runtime)
}

/// Installs a description whose assets are already imported, in its order.
pub fn prepare(
    description: &d::RenderDescription,
    imported: Vec<(w::Id, GlbAsset)>,
    mode: VideoMode,
    runtime: w::Id,
) -> Result<(Synthetic, SceneAdapter), Error> {
    if description.profile != d::PROFILE {
        return Err(format!("unsupported description profile {:?}", description.profile).into());
    }
    let camera = &description.camera;
    if !super::fits(camera.width, camera.height, super::MAX_EXTENT)
        || camera.fps != mode.fps
        || u64::from(camera.width) * u64::from(mode.height)
            != u64::from(camera.height) * u64::from(mode.width)
    {
        return Err(
            "the description's native extent and rate must match the output's aspect and rate"
                .into(),
        );
    }
    let extent = w::Extent {
        width: camera.width,
        height: camera.height,
    };
    // The description's lens is the reference camera's, for still renders; tracked
    // frames bring their own (`server/docs/tracking-worker.md#lens`).
    let lens = radial(&camera.radial).wire()?;
    let replay = camera
        .reference
        .as_ref()
        .map(|r| TrackingReplay::new(r.ti, r.center, lens, &r.frame_warps))
        .transpose()?;

    let mut config = Synthetic::configuration(mode, &runtime)?;
    config.configuration_id = id(format!("{}-render-description-v1", runtime.as_str()))?;
    config.native_extent = extent;
    config.sample_count = 4;
    config.limits.lights_per_scene = 8;
    config.limits.instances_per_scene = (MAX_INSTANCES + 1) as u32;
    config.shadows.resolution = 2048;
    config.shadows.max_bytes = (128_u64 * 1024 * 1024).into();
    config.max_intermediate_bytes = super::composition_budget(extent, config.output_extent).into();

    let world: w::Version =
        serde_json::from_value(json!({"id":"description-world","revision":"1"}))?;
    let mut registrations = Registrations::default();
    registrations
        .register_world(world.clone())
        .map_err(|e| e.to_string())?;
    registrations
        .register_model(id(GROUND)?, ground())
        .map_err(|e| e.to_string())?;
    let mut digests = HashMap::new();
    for ((asset_id, asset), described) in imported.into_iter().zip(&description.scene.assets) {
        log::info!(
            "description asset {}: vertices={} triangles={} parts={}",
            asset_id.as_str(),
            asset.mesh.positions().len(),
            asset.mesh.indices().len() / 3,
            asset.mesh.parts().len()
        );
        registrations
            .register_model(asset_id.clone(), asset)
            .map_err(|e| e.to_string())?;
        digests.insert(asset_id, assets::identity(described));
    }
    let state = scene_state(&config.configuration_id, &world, &description.scene)?;
    let (mut program, adapter) = Synthetic::install_registered(config, registrations, state)?;
    program.tracking = Some(Tracked {
        world,
        extent,
        replay,
    });
    program.described = Some(digests);
    Ok((program, adapter))
}

fn radial(radial: &d::Radial) -> Radial {
    Radial {
        centre_px: radial.centre_px,
        scale_px: radial.scale_px,
        coefficients: radial.coefficients,
        support: radial.support,
    }
}

/// The unit ground quad, scaled to the pitch by its instance: an invisible shadow catcher.
fn ground() -> GlbAsset {
    GlbAsset {
        mesh: MeshData::new(
            vec![
                [-0.5, -0.5, 0.],
                [0.5, -0.5, 0.],
                [0.5, 0.5, 0.],
                [-0.5, 0.5, 0.],
            ],
            vec![0, 2, 1, 0, 3, 2],
            std::iter::once(0..6).collect(),
        )
        .expect("static quad"),
        materials: vec![ImportedMaterial {
            factors: PbrMaterial {
                shadow_catcher: true,
                double_sided: true,
                ..Default::default()
            },
            maps: Default::default(),
        }],
        warnings: Vec::new(),
    }
}

/// The program scene of `scene`: its instances behind the ground catcher, with the
/// tracked camera and the described or default lighting.
fn scene_state(
    configuration_id: &w::Id,
    world: &w::Version,
    scene: &d::Scene,
) -> Result<w::SceneState, Error> {
    let [length, width] = scene.ground.pitch_m;
    if ![length, width]
        .iter()
        .all(|v| v.is_finite() && (1.0..=1000.0).contains(v))
    {
        return Err("ground pitch_m must be between 1 and 1000 metres".into());
    }
    if scene.instances.len() > MAX_INSTANCES {
        return Err(format!("a description places at most {MAX_INSTANCES} instances").into());
    }
    if scene
        .assets
        .iter()
        .map(|a| &a.id)
        .chain(scene.instances.iter().map(|i| &i.id))
        .any(|id| id.as_str() == GROUND)
    {
        return Err(format!("{GROUND} is reserved for the ground").into());
    }
    let lighting = match &scene.lighting {
        Some(lighting) => serde_json::to_value(lighting)?,
        None => default_lighting(scene.ground.pitch_m)?,
    };
    let mut instances = vec![json!({
        "instance_id": GROUND, "asset_ref": GROUND, "visible": true, "material_overrides": [],
        "model": [[length * 1.5, 0, 0, 0], [0, width * 1.5, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]],
    })];
    instances.extend(scene.instances.iter().map(|instance| {
        let mut native = json!({
            "instance_id": instance.id, "asset_ref": instance.asset, "model": instance.model,
            "visible": instance.visible, "material_overrides": [],
        });
        if let Some(flag) = &instance.flag {
            native["flag"] = json!(flag);
        }
        native
    }));
    Ok(serde_json::from_value(json!({
        "world": world, "configuration_id": configuration_id,
        "camera": {"mode": "tracked", "near_m": 0.1, "far_m": 10000.0},
        "instances": instances, "lighting": lighting,
        "shadow_mode": scene.shadow_mode.unwrap_or(w::ShadowMode::Pcss),
    }))?)
}

/// Four 35 m corner spots on the pitch centre and a sun.
fn default_lighting(pitch: [f64; 2]) -> Result<Value, Error> {
    let mut lighting = super::pitch::lighting(pitch, 35., "spots", true, [[0.; 2]; 4])?;
    for light in lighting["lights"].as_array_mut().expect("native lighting") {
        if light["kind"] == "spot" {
            light["intensity"] = json!([16000, 16800, 20000]);
        } else {
            light["direction"] = json!([0.5, 0.6, 1.0]);
        }
    }
    Ok(lighting)
}

/// Serves `description.load` for one running description program.
pub struct Loader {
    control: SceneControl,
    runtime_id: w::Id,
    scene_id: w::Id,
    configuration_id: w::Id,
    world: w::Version,
    roots: Vec<PathBuf>,
    worker: WorkerAssets,
    /// Private directory of verified asset copies for render workers.
    store: PathBuf,
    /// One load at a time.
    registered: tokio::sync::Mutex<Registered>,
}

struct Registered {
    /// [`assets::identity`] of every registered asset.
    identities: HashMap<w::Id, String>,
    /// Bytes of the runtime assets in the store.
    stored: u64,
}

impl Loader {
    /// `roots` are canonical folders runtime asset paths must lie in; `store` is a
    /// private directory (created with mode 0700) for the copies workers read.
    pub fn new(
        program: &Synthetic,
        control: SceneControl,
        roots: Vec<PathBuf>,
        worker: WorkerAssets,
        store: PathBuf,
    ) -> Result<Self, Error> {
        let tracked = program
            .tracking
            .as_ref()
            .ok_or("not a description program")?;
        let mut dir = std::fs::DirBuilder::new();
        dir.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
        dir.create(&store)?;
        Ok(Self {
            control,
            runtime_id: program.configuration.runtime_id.clone(),
            scene_id: program.scene_id.clone(),
            configuration_id: program.configuration.configuration_id.clone(),
            world: tracked.world.clone(),
            roots,
            worker,
            store,
            registered: tokio::sync::Mutex::new(Registered {
                identities: program
                    .described
                    .clone()
                    .ok_or("not a description program")?,
                stored: 0,
            }),
        })
    }

    pub async fn load(&self, params: d::LoadParams) -> Result<d::Loaded, ErrorObject> {
        if self.roots.is_empty() {
            return Err(reject(
                w::RejectionKind::UnsupportedProfile,
                "description.load needs chr-server --asset-root",
            ));
        }
        let scene = params.scene;
        let state =
            scene_state(&self.configuration_id, &self.world, &scene).map_err(incompatible)?;
        let mut guard = self.registered.lock().await;
        let Registered {
            identities: registered,
            stored,
        } = &mut *guard;
        let mut fresh = Vec::new();
        let mut reused = 0;
        for asset in &scene.assets {
            let digest = assets::identity(asset);
            match registered.get(&asset.id) {
                Some(known) if *known == digest => reused += 1,
                Some(_) => {
                    return Err(reject(
                        w::RejectionKind::Conflict,
                        format!(
                            "asset {} is registered with other content",
                            asset.id.as_str()
                        ),
                    ));
                }
                None if fresh.iter().any(|a: &d::Asset| a.id == asset.id) => {
                    return Err(reject(w::RejectionKind::Conflict, "duplicate asset ID"));
                }
                None => fresh.push(asset.clone()),
            }
        }
        if let Some(instance) = scene.instances.iter().find(|instance| {
            !registered.contains_key(&instance.asset)
                && !fresh.iter().any(|a| a.id == instance.asset)
        }) {
            return Err(reject(
                w::RejectionKind::NotFound,
                format!("instance {} uses an unknown asset", instance.id.as_str()),
            ));
        }
        if registered.len() + fresh.len() > MAX_ASSETS {
            return Err(reject(
                w::RejectionKind::Capacity,
                format!("a server registers at most {MAX_ASSETS} assets; restart it to clear them"),
            ));
        }
        // Imports read files and decode meshes: off the async workers, all or none.
        // Workers get the verified bytes as server-owned copies, never the caller's
        // files, which may change or disappear later.
        let roots = self.roots.clone();
        let store = self.store.clone();
        let imports = fresh.clone();
        let imported = tokio::task::spawn_blocking(move || {
            imports
                .into_iter()
                .map(|asset| {
                    let bytes = assets::read_verified(&asset, &Paths::Roots(&roots))?;
                    let glb = assets::import(&asset, &bytes)?;
                    Ok((keep(asset, &bytes, &store)?, glb, bytes.len() as u64))
                })
                .collect::<Result<Vec<_>, Error>>()
        })
        .await
        .map_err(|e| ErrorObject::internal(e.to_string()))?
        .map_err(|e| incompatible(format!("asset import failed: {e}")))?;
        let size: u64 = imported.iter().map(|(_, _, size)| size).sum();
        if *stored + size > MAX_STORED_BYTES {
            return Err(reject(
                w::RejectionKind::Capacity,
                format!(
                    "runtime assets exceed {MAX_STORED_BYTES} bytes; restart the server to clear them"
                ),
            ));
        }
        *stored += size;
        let imported_count = imported.len() as u32;
        for (asset, glb, _) in imported {
            self.control.register_model(asset.id.clone(), glb).await?;
            registered.insert(asset.id.clone(), assets::identity(&asset));
            self.worker.push(asset);
        }
        let applied = self.replace(state).await?;
        log::info!(
            "description loaded: {} instances, {imported_count} assets imported, {reused} reused",
            scene.instances.len()
        );
        Ok(d::Loaded {
            scene_id: applied.scene_id,
            scene_revision: applied.scene_revision,
            assets_imported: imported_count,
            assets_reused: reused,
        })
    }
}

impl Loader {
    /// Recreates the program scene from `state`; if that fails, the previous scene
    /// is recreated, so the output keeps its CG.
    async fn replace(&self, state: w::SceneState) -> Result<w::SceneApplied, ErrorObject> {
        let target = json!({"runtime_id": self.runtime_id, "scene_id": self.scene_id});
        let previous: w::SceneSnapshot = serde_json::from_value(
            self.control
                .call(w::SCENE_GET, Some(target.clone()))
                .await?,
        )
        .map_err(|e| ErrorObject::internal(e.to_string()))?;
        self.control.call(w::SCENE_RELEASE, Some(target)).await?;
        let create = |state: &w::SceneState| {
            self.control.call(
                w::SCENE_CREATE,
                Some(json!({"runtime_id": self.runtime_id, "scene_id": self.scene_id, "state": state})),
            )
        };
        match create(&state).await {
            Ok(applied) => {
                serde_json::from_value(applied).map_err(|e| ErrorObject::internal(e.to_string()))
            }
            Err(error) => {
                create(&previous.state).await?;
                Err(error)
            }
        }
    }
}

/// Writes the verified bytes of `asset` into the private `store` under their
/// SHA-256 and points the asset there; render workers read only these copies.
fn keep(mut asset: d::Asset, bytes: &[u8], store: &Path) -> Result<d::Asset, Error> {
    let path = store.join(asset.sha256.to_ascii_lowercase());
    if !path.exists() {
        let staging = store.join(format!(".{}.tmp", asset.sha256.to_ascii_lowercase()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        std::io::Write::write_all(&mut options.open(&staging)?, bytes)?;
        std::fs::rename(&staging, &path)?;
    }
    asset.source = AssetSource::File(FilePath { path });
    Ok(asset)
}

/// Imports the runtime assets a render worker must register after installing its
/// program; their paths point to the server's private, verified copies.
pub fn import_for_worker(assets: &[d::Asset]) -> Result<Vec<(w::Id, GlbAsset)>, Error> {
    assets
        .iter()
        .map(|asset| {
            Ok((
                asset.id.clone(),
                assets::load(
                    asset,
                    &Paths::Startup {
                        base: Path::new("/"),
                    },
                )?,
            ))
        })
        .collect()
}
