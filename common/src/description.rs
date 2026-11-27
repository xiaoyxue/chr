//! `chr-render-description-v1`: a renderer-neutral description of what to draw and
//! with which lens, independent of any client's own scene model (for example a
//! Fusion match). Clients convert their placements into it; `chr-server` loads it at
//! startup (`--description <json>`) or replaces its scene at runtime
//! ([`LOAD`]). See `server/docs/render-description.md`.
//!
//! Coordinates follow `server/docs/coordinates.md`: pitch-centred metres, X along
//! the pitch length, Y along its width, ground Z = 0 and up is negative Z. Matrices
//! are forward row-major and act on column vectors.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::rate::Rate;
use crate::scene_frame::{Flag, Id, Lighting, Matrix4, Motion, ShadowMode, U64};

pub const PROFILE: &str = "chr-render-description-v1";

/// Replaces the scene of a running description program. Params: [`LoadParams`].
/// Result: [`Loaded`]. The lens and native extent stay as started.
pub const LOAD: &str = "description.load";

/// Starts the reveal of a flag instance whose `reveal.start` is `cue`, and the raise
/// of a native one whose `raise.start` is `cue`, at the next rendered frame; a repeated
/// cue restarts them. A new scene clears the cues.
/// Params: [`CueRevealParams`]. Result: [`CueRevealed`].
pub const CUE_REVEAL: &str = "description.cue_reveal";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CueRevealParams {
    pub instance: Id,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CueRevealed {
    /// Cues of this instance since its scene was loaded, this one included.
    pub cue: u32,
}

/// A complete startup description: the lens is fixed for the server's lifetime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderDescription {
    /// Always [`PROFILE`].
    pub profile: String,
    pub camera: Camera,
    pub scene: Scene,
}

/// The native image and its lens. Each frame's `Ti`, `C` and warp arrive with the
/// frame (`tracking.frame`); only the radial term is fixed here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    /// Native pixels.
    pub width: u32,
    pub height: u32,
    /// The camera's nominal frame rate: an integer or a string such as
    /// `"30000/1001"` ([`Rate`]).
    pub fps: Rate,
    pub radial: Radial,
    /// A fixed camera for still renders (`chr-renderer-cli render --frame <n>`); the
    /// live server ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<ReferenceCamera>,
}

/// `radial-poly-native-px-v1`: `r = centre + (a - centre) * (1 + k1 p^2 + k2 p^4 + k3 p^6)`
/// with `p = |a - centre| / scale_px`, in native pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Radial {
    pub centre_px: [f64; 2],
    pub scale_px: f64,
    pub coefficients: [f64; 3],
    /// Validated ideal-pixel rectangle `[min_xy, max_xy]`; outside is extrapolation.
    pub support: Option<[[f64; 2]; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceCamera {
    /// Forward row-major ground XY to ideal SNC.
    pub ti: [[f64; 3]; 3],
    /// Optical centre, world metres.
    pub center: [f64; 3],
    /// Row-major reference-native-pixel to frame-SNC warps; frame `n` uses
    /// `n % len`. Empty means `W = S`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame_warps: Vec<[[f64; 3]; 3]>,
}

/// What is drawn. A loaded scene replaces the previous one completely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub ground: Ground,
    /// `null` selects the default stadium rig: four corner spots and a sun.
    #[serde(default)]
    pub lighting: Option<Lighting>,
    /// Default shadow mode of the scene's lights; `null` selects PCSS.
    #[serde(default)]
    pub shadow_mode: Option<ShadowMode>,
    pub assets: Vec<Asset>,
    pub instances: Vec<Instance>,
}

/// The ground plane: an invisible shadow catcher under the CG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ground {
    /// Pitch length (X) and width (Y) in metres; the catcher extends 1.5 times past it.
    pub pitch_m: [f64; 2],
}

/// One drawable resource. Several instances may share it; its geometry and
/// textures are imported once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub id: Id,
    pub kind: AssetKind,
    pub source: AssetSource,
    /// Lowercase hex SHA-256 of the source bytes.
    pub sha256: String,
    /// `"mask"` imports a `glb`'s `alphaMode: BLEND` materials as alpha masks
    /// (cutoff 0.5) instead of rejecting the asset; the renderer has no translucent
    /// pass. Only allowed for `glb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_as: Option<BlendAs>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlendAs {
    Mask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    /// A static binary glTF, in its own Y-up basis; the importer turns it to -Z up.
    Glb,
    /// A PNG/JPEG image on the unit quad `[-0.5, 0.5]^2` at Z = 0: the image's top
    /// edge at -Y, its left edge at -X, facing up (-Z). An unlit ground decal blended
    /// by the image's alpha times the instance's `opacity`. Scale, rotation and
    /// position are the instance's.
    ImageQuad,
    /// A PNG/JPEG image as the artwork of a waving flag. Its instances carry the
    /// flag's size and motion (`Instance::flag`); the image's top-left texel is at
    /// the flag-local origin and it reads upright from the flag's -Z side. Texels
    /// with alpha below 0.5 are cut out.
    Flag,
    /// A linear-light OpenEXR latitude-longitude panorama (2:1) for image-based
    /// lighting, in the renderer's convention: +X at the image centre, up (-Z) along
    /// the top row. Only `lighting.environment.asset_ref` may use it, never an instance.
    Environment,
}

/// Where the server reads an asset: a file, or an entry of a ZIP package such as
/// a Fusion offline package. Paths are absolute, or relative to a startup file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssetSource {
    Package(PackageEntry),
    File(FilePath),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageEntry {
    /// ZIP file.
    pub package: PathBuf,
    /// Entry name inside it, such as `assets/ASSET-1.glb`.
    pub entry: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilePath {
    pub path: PathBuf,
}

/// One placement of an asset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub id: Id,
    pub asset: Id,
    /// Asset coordinates (the `glb` importer's -Z-up basis, the image quad, or the
    /// flag-local frame) to world metres, forward row-major.
    pub model: Matrix4,
    #[serde(default = "visible")]
    pub visible: bool,
    /// Size and motion of a `flag` asset's instance; required for flags and
    /// rejected for other assets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag: Option<Flag>,
    /// Rigid animation of the whole instance about its `model` (#157).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<Motion>,
    /// Opacity of an `image_quad` instance in `[0, 1]`: its alpha is the image's
    /// alpha times this. Absent: 0.5 (#163). Rejected for other assets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
}

fn visible() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadParams {
    pub scene: Scene,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Loaded {
    pub scene_id: Id,
    pub scene_revision: U64,
    /// Assets read and imported by this call.
    pub assets_imported: u32,
    /// Assets already registered with the same ID and SHA-256.
    pub assets_reused: u32,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::scene_frame::FlagMotion;

    #[test]
    fn sources_and_defaults_parse_strictly() {
        let scene: Scene = serde_json::from_value(json!({
            "ground": {"pitch_m": [105, 68]},
            "assets": [
                {"id": "car", "kind": "glb", "sha256": "00",
                 "source": {"package": "/m.zip", "entry": "assets/car.glb"}},
                {"id": "ad", "kind": "image_quad", "sha256": "11", "source": {"path": "ad.png"}}
            ],
            "instances": [{"id": "p1", "asset": "car",
                "model": [[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]]}]
        }))
        .unwrap();
        assert!(matches!(scene.assets[0].source, AssetSource::Package(_)));
        assert!(matches!(scene.assets[1].source, AssetSource::File(_)));
        assert!(scene.instances[0].visible);
        assert_eq!((scene.lighting, scene.shadow_mode), (None, None));
        let flag: Instance = serde_json::from_value(json!({"id": "f1", "asset": "flag",
            "model": [[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]],
            "flag": {"width_m": 3.2, "height_m": 2, "amplitude_m": 0.32, "frequency": 1.35,
                     "speed": 1, "material": "simple"}}))
        .unwrap();
        assert_eq!(flag.flag.unwrap().width_m.get(), 3.2);
        let flag = json!({"width_m": 1, "height_m": 1, "amplitude_m": 0, "frequency": 1,
                          "speed": 1, "material": "simple"});
        assert!(serde_json::from_value::<Flag>(flag.clone()).is_ok());
        // Without a type, wave or material, a flag takes a tifo's: 1.1 m, 1.35, 5.5, cloth.
        let bare: Flag = serde_json::from_value(json!({"width_m": 36, "height_m": 13.5})).unwrap();
        let FlagMotion::Wave(wave) = bare.motion else {
            panic!("a flag without a type is a default flag")
        };
        assert_eq!(
            [wave.amplitude_m, wave.frequency, wave.speed].map(|x| x.get()),
            [1.1, 1.35, 5.5]
        );
        assert_eq!(bare.material, crate::scene_frame::FlagMaterial::Cloth);
        let mut tifo = flag.clone();
        tifo["artwork_rotation"] = json!(90);
        tifo["reveal"] = json!({"direction": "center_horizontal", "duration_s": 2, "start": "cue"});
        let tifo: Flag = serde_json::from_value(tifo).unwrap();
        assert_eq!(tifo.artwork_rotation, Some(90));
        assert_eq!(
            tifo.reveal.unwrap().start,
            crate::scene_frame::RevealStart::Cue
        );
        for direction in [
            "left_to_right",
            "right_to_left",
            "top_to_bottom",
            "bottom_to_top",
            "center_horizontal",
            "center_vertical",
        ] {
            let mut revealed = flag.clone();
            revealed["reveal"] = json!({"direction": direction, "duration_s": 90, "start": "load"});
            let parsed: Flag = serde_json::from_value(revealed).unwrap();
            assert_eq!(
                serde_json::to_value(parsed.reveal.unwrap().direction).unwrap(),
                direction
            );
        }
        for (key, value) in [
            ("pole", json!(true)),
            ("material", json!("silk")),
            (
                "reveal",
                json!({"direction": "center-horizontal", "duration_s": 2, "start": "load"}),
            ),
            (
                "reveal",
                json!({"direction": "left-to-right", "duration_s": 2, "start": "load"}),
            ),
            (
                "reveal",
                json!({"direction": "center_horizontal", "duration_s": 2}),
            ),
        ] {
            let mut bad = flag.clone();
            bad[key] = value;
            assert!(serde_json::from_value::<Flag>(bad).is_err(), "{key}");
        }
        // A native flag takes the reference simulation's wind and fabric.
        let native: Flag = serde_json::from_value(
            json!({"width_m": 3.2, "height_m": 4.8, "type": "native", "wind_mps": 12}),
        )
        .unwrap();
        let FlagMotion::Native(cloth) = native.motion else {
            panic!("a native flag simulates cloth")
        };
        assert_eq!(
            [
                cloth.wind_mps,
                cloth.wind_direction_deg,
                cloth.tension,
                cloth.slack
            ]
            .map(|x| x.get()),
            [12.0, 25.0, 0.65, 0.1]
        );
        assert!(cloth.auto_direction);
        assert_eq!(cloth.raise, None);
        assert!(!native.cued());
        // A native flag may be raised, from the scene load or on a cue.
        let raised: Flag = serde_json::from_value(json!({"width_m": 40, "height_m": 40,
            "type": "native", "raise": {"duration_s": 8, "start": "cue"}}))
        .unwrap();
        let FlagMotion::Native(cloth) = &raised.motion else {
            panic!("a native flag simulates cloth")
        };
        assert_eq!(cloth.raise.as_ref().unwrap().duration_s.get(), 8.0);
        assert!(raised.cued());
        // Each type keeps exactly its own fields through a round trip.
        for parsed in [
            native,
            raised,
            serde_json::from_value(flag.clone()).unwrap(),
        ] {
            let wire = serde_json::to_value(&parsed).unwrap();
            assert_eq!(serde_json::from_value::<Flag>(wire).unwrap(), parsed);
        }
        // A default flag is written as before native flags.
        let wire = serde_json::to_value(bare).unwrap();
        assert!(wire.get("type").is_none() && wire.get("wind_mps").is_none());
        for bad in [
            json!({"width_m": 1, "height_m": 1, "type": "native", "speed": 1}),
            json!({"width_m": 1, "height_m": 1, "amplitude_m": 1, "type": "native"}),
            json!({"width_m": 1, "height_m": 1, "tension": 0.5}),
            json!({"width_m": 1, "height_m": 1, "slack": 0.02}),
            json!({"width_m": 1, "height_m": 1, "raise": {"duration_s": 6, "start": "load"}}),
            json!({"width_m": 1, "height_m": 1, "type": "native", "raise": {"duration_s": 6}}),
            json!({"width_m": 1, "height_m": 1, "type": "default", "auto_direction": false}),
            json!({"width_m": 1, "height_m": 1, "type": "physical"}),
        ] {
            assert!(
                serde_json::from_value::<Flag>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        for bad in [
            json!({"path": "a.png", "entry": "x"}),
            json!({"package": "/m.zip"}),
            json!({"url": "http://x"}),
        ] {
            assert!(
                serde_json::from_value::<AssetSource>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }
}
