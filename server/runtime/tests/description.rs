//! Render descriptions: startup installation, runtime `description.load` and a GPU frame.
use chr_common::{description as d, scene_frame as w};
use chr_renderer::Gpu;
use chr_runtime::{
    program::{
        ProgramRenderer,
        description::{self, Loader},
        synthetic::{Synthetic, id, runtime_id},
    },
    retained::SceneControl,
    sdi::{SdiOutput, Taps, VideoMode},
    worker::supervisor::WorkerAssets,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{Cursor, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const MODE: VideoMode = VideoMode {
    width: 192,
    height: 128,
    fps: 50,
};

fn png(rgba: [u8; 4]) -> Vec<u8> {
    let mut png = Cursor::new(Vec::new());
    image::RgbaImage::from_raw(2, 2, rgba.repeat(4))
        .unwrap()
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    png.into_inner()
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    /// `ad.png` (red) as a file, `match.zip` holding `assets/ad2.png` (blue).
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("chr-description-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ad.png"), png([255, 0, 0, 255])).unwrap();
        let mut zip = zip::ZipWriter::new(std::fs::File::create(dir.join("match.zip")).unwrap());
        zip.start_file("assets/ad2.png", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&png([0, 0, 255, 255])).unwrap();
        zip.finish().unwrap();
        Self { dir }
    }

    /// A quad of `size` metres at pitch XY, lying on the ground.
    fn instance(id: &str, asset: &str, x: f64, size: f64) -> Value {
        json!({"id": id, "asset": asset,
            "model": [[size, 0, 0, x], [0, size, 0, 0], [0, 0, 1, -0.05], [0, 0, 0, 1]]})
    }

    fn description(&self, instances: Value) -> PathBuf {
        let s = (192_f64 * 128.).sqrt();
        let description = json!({
            "profile": d::PROFILE,
            "camera": {"width": 192, "height": 128, "fps": 50,
                "radial": {"centre_px": [96, 64], "scale_px": 192, "coefficients": [-0.05, 0, 0], "support": null},
                "reference": {"ti": [[0.12, 0, 0], [0, 0.12, 0], [0, 0, 1]], "center": [0, 0, -8],
                    "frame_warps": [[[2. / s, 0, -192. / s], [0, 2. / s, -128. / s], [0, 0, 1]]]}},
            "scene": {"ground": {"pitch_m": [20, 12]},
                "assets": [{"id": "ad", "kind": "image_quad", "source": {"path": "ad.png"},
                    "sha256": sha(&png([255, 0, 0, 255]))}],
                "instances": instances}
        });
        let path = self.dir.join("description.json");
        std::fs::write(&path, description.to_string()).unwrap();
        path
    }

    fn root(&self) -> PathBuf {
        self.dir.canonicalize().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn instances(control: &SceneControl, program: &Synthetic) -> Vec<(String, String)> {
    let scene = control
        .call(
            w::SCENE_GET,
            Some(json!({"runtime_id": program.configuration.runtime_id, "scene_id": program.scene_id})),
        )
        .await
        .unwrap();
    scene["state"]["instances"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["instance_id"].as_str().unwrap().to_owned(),
                i["asset_ref"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn scene(fixture: &Fixture, assets: Value, instances: Value) -> d::LoadParams {
    serde_json::from_value(json!({"scene": {"ground": {"pitch_m": [105, 68]},
        "assets": assets, "instances": instances}}))
    .unwrap_or_else(|e| panic!("{e} in {}", fixture.dir.display()))
}

#[tokio::test]
async fn a_startup_description_shares_one_asset_between_instances() {
    let fixture = Fixture::new("startup");
    let path = fixture.description(json!([
        Fixture::instance("left", "ad", -4., 2.),
        Fixture::instance("right", "ad", 4., 2.)
    ]));
    let (program, adapter) = description::load(&path, MODE, runtime_id().unwrap()).unwrap();
    let (control, owner) = SceneControl::spawn(adapter, 8).unwrap();
    program.create_scene(&control).await.unwrap();
    assert_eq!(
        instances(&control, &program).await,
        [
            (
                description::GROUND.to_owned(),
                description::GROUND.to_owned()
            ),
            ("left".to_owned(), "ad".to_owned()),
            ("right".to_owned(), "ad".to_owned())
        ]
    );
    drop(control);
    owner.await.unwrap();

    let reserved = fixture.description(json!([Fixture::instance(
        description::GROUND,
        "ad",
        0.,
        1.
    )]));
    assert!(description::load(&reserved, MODE, runtime_id().unwrap()).is_err());
    let mut tampered: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    tampered["scene"]["assets"][0]["sha256"] = json!("0".repeat(64));
    std::fs::write(&path, tampered.to_string()).unwrap();
    assert!(description::load(&path, MODE, runtime_id().unwrap()).is_err());
}

#[tokio::test]
async fn description_load_replaces_the_scene_with_reused_and_packaged_assets() {
    let fixture = Fixture::new("load");
    let path = fixture.description(json!([Fixture::instance("left", "ad", -4., 2.)]));
    let (program, adapter) = description::load(&path, MODE, runtime_id().unwrap()).unwrap();
    let (control, owner) = SceneControl::spawn(adapter, 8).unwrap();
    program.create_scene(&control).await.unwrap();
    let worker = WorkerAssets::default();
    let store = fixture.dir.join("store");
    let loader = Loader::new(
        &program,
        control.clone(),
        vec![fixture.root()],
        worker.clone(),
        store.clone(),
    )
    .unwrap();

    let ad = json!({"id": "ad", "kind": "image_quad", "source": {"path": fixture.dir.join("ad.png")},
        "sha256": sha(&png([255, 0, 0, 255]))});
    let ad2 = json!({"id": "ad2", "kind": "image_quad",
        "source": {"package": fixture.dir.join("match.zip"), "entry": "assets/ad2.png"},
        "sha256": sha(&png([0, 0, 255, 255]))});
    let loaded = loader
        .load(scene(
            &fixture,
            json!([ad, ad2]),
            json!([
                Fixture::instance("a", "ad2", 0., 3.),
                Fixture::instance("b", "ad2", 5., 3.)
            ]),
        ))
        .await
        .unwrap();
    assert_eq!((loaded.assets_imported, loaded.assets_reused), (1, 1));
    assert_eq!(
        instances(&control, &program).await[1..],
        [
            ("a".to_owned(), "ad2".to_owned()),
            ("b".to_owned(), "ad2".to_owned())
        ]
    );
    // Workers read the server's verified copy, not the caller's package.
    std::fs::remove_file(fixture.dir.join("match.zip")).unwrap();
    let workers = worker.snapshot();
    assert!(matches!(
        &workers[0].source,
        d::AssetSource::File(file) if file.path.starts_with(&store)
    ));
    assert_eq!(description::import_for_worker(&workers).unwrap().len(), 1);

    let mut changed = ad2.clone();
    changed["sha256"] = json!(sha(b"other"));
    let error = loader
        .load(scene(&fixture, json!([changed]), json!([])))
        .await
        .unwrap_err();
    assert_eq!(error.code, w::RejectionKind::Conflict.code());
    let outside = json!({"id": "outside", "kind": "image_quad", "source": {"path": "/etc/hostname"},
        "sha256": sha(b"x")});
    assert!(
        loader
            .load(scene(&fixture, json!([outside]), json!([])))
            .await
            .is_err()
    );
    let unknown = scene(
        &fixture,
        json!([]),
        json!([Fixture::instance("c", "missing", 0., 1.)]),
    );
    assert!(loader.load(unknown).await.is_err());
    let singular = scene(
        &fixture,
        json!([]),
        json!([Fixture::instance("c", "ad", 0., 0.)]),
    );
    assert!(
        loader.load(singular).await.is_err(),
        "rejected by scene.create"
    );
    assert_eq!(
        instances(&control, &program).await.len(),
        3,
        "failed loads leave the last scene in place"
    );

    let none = Loader::new(
        &program,
        control.clone(),
        Vec::new(),
        WorkerAssets::default(),
        store.clone(),
    )
    .unwrap();
    let error = none
        .load(scene(&fixture, json!([]), json!([])))
        .await
        .unwrap_err();
    assert_eq!(error.code, w::RejectionKind::UnsupportedProfile.code());
    drop((control, loader, none));
    owner.await.unwrap();
    assert!(Path::new(&fixture.root()).exists());
}

#[tokio::test]
async fn flag_instances_need_flag_assets_and_flag_parameters() {
    let fixture = Fixture::new("flag");
    let path = fixture.description(json!([]));
    let (program, adapter) = description::load(&path, MODE, runtime_id().unwrap()).unwrap();
    let (control, owner) = SceneControl::spawn(adapter, 8).unwrap();
    program.create_scene(&control).await.unwrap();
    let loader = Loader::new(
        &program,
        control.clone(),
        vec![fixture.root()],
        WorkerAssets::default(),
        fixture.dir.join("store"),
    )
    .unwrap();
    let artwork = json!({"id": "flag", "kind": "flag", "source": {"path": fixture.dir.join("ad.png")},
        "sha256": sha(&png([255, 0, 0, 255]))});
    // Upright, facing -Y, its top-left corner 4 m up.
    let flag = |asset: &str, motion: Option<Value>| {
        let mut instance = json!({"id": "f", "asset": asset,
            "model": [[1, 0, 0, -1.6], [0, 0, -1, 0], [0, 1, 0, -4], [0, 0, 0, 1]]});
        if let Some(motion) = motion {
            instance["flag"] = motion;
        }
        instance
    };
    let motion = json!({"width_m": 3.2, "height_m": 2.0, "amplitude_m": 0.32, "frequency": 1.35,
        "speed": 1.0, "material": "pbr"});
    loader
        .load(scene(
            &fixture,
            json!([artwork]),
            json!([flag("flag", Some(motion.clone()))]),
        ))
        .await
        .unwrap();
    let state = control
        .call(
            w::SCENE_GET,
            Some(json!({"runtime_id": program.configuration.runtime_id, "scene_id": program.scene_id})),
        )
        .await
        .unwrap();
    assert_eq!(state["state"]["instances"][1]["flag"], motion);

    let mut flat = motion.clone();
    flat["width_m"] = json!(0);
    for (case, instance) in [
        ("a flag without motion", flag("flag", None)),
        (
            "an image quad with motion",
            flag("ad", Some(motion.clone())),
        ),
        ("a zero-width flag", flag("flag", Some(flat))),
    ] {
        let error = loader
            .load(scene(&fixture, json!([]), json!([instance])))
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            w::RejectionKind::IncompatibleInput.code(),
            "{case}"
        );
    }
    assert_eq!(instances(&control, &program).await[1].1, "flag");
    drop((control, loader));
    owner.await.unwrap();
}

#[test]
fn cameras_up_to_4k_render_at_any_output_size_of_their_aspect() {
    let camera = |width: u32, height: u32| -> d::RenderDescription {
        serde_json::from_value(json!({
            "profile": d::PROFILE,
            "camera": {"width": width, "height": height, "fps": 30,
                "radial": {"centre_px": [width / 2, height / 2], "scale_px": width,
                    "coefficients": [-0.05, 0, 0], "support": null}},
            "scene": {"ground": {"pitch_m": [105, 68]}, "assets": [], "instances": []}
        }))
        .unwrap()
    };
    let output = |width, height| VideoMode {
        width,
        height,
        fps: 30,
    };
    for (native, size) in [
        ((4096, 2160), (4096, 2160)),
        ((3840, 2160), (3840, 2160)),
        ((3840, 2160), (1920, 1080)),
        ((1920, 1080), (1920, 1080)),
        ((4096, 2160), (2048, 1080)),
    ] {
        let (program, _) = description::prepare(
            &camera(native.0, native.1),
            Vec::new(),
            output(size.0, size.1),
            runtime_id().unwrap(),
        )
        .unwrap_or_else(|error| panic!("{native:?} -> {size:?}: {error}"));
        assert_eq!(program.configuration.native_extent.width, native.0);
        assert_eq!(program.configuration.output_extent.width, size.0);
    }
    for (native, size) in [
        // Above DCI 4K, odd, or another aspect than the output.
        ((4098, 2160), (4098, 2160)),
        ((4096, 2162), (4096, 2162)),
        ((7680, 4320), (7680, 4320)),
        ((7680, 4320), (1920, 1080)),
        ((3840, 2160), (7680, 4320)),
        ((1921, 1080), (1920, 1080)),
        ((1920, 1080), (1280, 1024)),
    ] {
        assert!(
            description::prepare(
                &camera(native.0, native.1),
                Vec::new(),
                output(size.0, size.1),
                runtime_id().unwrap(),
            )
            .is_err(),
            "{native:?} -> {size:?}"
        );
    }
    // The synthetic demonstration keeps its own 1280x720 limit.
    assert!(Synthetic::install(output(1920, 1080), runtime_id().unwrap()).is_err());
    assert!(Synthetic::install(output(1280, 720), runtime_id().unwrap()).is_ok());
}

#[tokio::test]
#[ignore = "requires a native hardware GPU"]
async fn gpu_described_image_quads_are_drawn_unlit_through_the_tracked_camera() {
    let gpu = Gpu::request(&Gpu::instance(), None).await.unwrap();
    assert!(matches!(
        gpu.adapter.get_info().device_type,
        wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
    ));
    let fixture = Fixture::new("gpu");
    let mut frames = Vec::new();
    for visible in [true, false] {
        let mut instance = Fixture::instance("ad", "ad", 0., 3.);
        instance["visible"] = json!(visible);
        let path = fixture.description(json!([instance]));
        let (mut program, adapter) = description::load(&path, MODE, runtime_id().unwrap()).unwrap();
        let (control, owner) = SceneControl::spawn(adapter, 8).unwrap();
        program.create_scene(&control).await.unwrap();
        let mut renderer = ProgramRenderer::new(&gpu, &program.configuration).unwrap();
        renderer.set_video_lens(program.video_lens.clone());
        let (taps, tap) = Taps::observed();
        let sdi =
            SdiOutput::spawn_program(MODE, id("description-test-output").unwrap(), taps).unwrap();
        let mut port = sdi.input();
        let tracked = program.tracking.clone().unwrap();
        let geometry = tracked.geometry(&tracked.replay.as_ref().unwrap().frame(0));
        program
            .inject_with(&control, 0, &[20, 100, 40, 255].repeat(192 * 128), geometry)
            .await
            .unwrap()
            .unwrap();
        renderer.drive(&gpu, &program.scheduler, &mut port).unwrap();
        let slot = loop {
            let slot = tap.recv_timeout(Duration::from_secs(5)).unwrap();
            if slot.association.is_some() {
                break slot;
            }
        };
        frames.push(slot.uyvy);
        drop(control);
        owner.await.unwrap();
    }
    let changed = frames[0]
        .iter()
        .zip(frames[1].iter())
        .filter(|(a, b)| a != b)
        .count();
    assert!(changed > 100, "the red quad changes {changed} bytes");
}
