//! The tracking hop's wire contract: vchao's `VchaoFrame` v1 raw-frame boundary
//! (Hong-Xiang/vchao `live-video/src/unixfd.rs` and
//! `live-video/README.md#native-unixfd-boundary`), mirrored here without
//! depending on vchao's crates.
//!
//! Every buffer on both tracking sockets is one video frame: packed RGBA8 as the
//! buffer's single memfd-backed memory, with vchao's raw wire caps
//! (`video/x-raw,format=RGBA` plus the `vchao-*` fields: [`LAYOUT`], [`STAGE`],
//! [`CLOCK_DOMAIN`]), `offset = frame_id`, `offset_end = frame_id + 1`, PTS =
//! `source_ns`, the frame duration and no DTS. It carries exactly one
//! `GstCustomMeta` named [`META`] whose structure has a string field `json`: a
//! [`FrameMeta`]. chr-server sends `selection: null`; the worker returns every
//! frame exactly once, in order, with the same memory and `selection` set. JSON
//! uses round-trip float formatting (serde_json's `float_roundtrip`), so `t`
//! crosses bit for bit. The GStreamer side (caps, buffers, meta) is
//! chr-runtime's `worker::tracking::wire`.
//!
//! `t` is forward, row-major, acting on homogeneous column vectors: ground XY
//! metres to this frame's SNC (`server/docs/coordinates.md`). vchao estimates it
//! against the real, distorted image and sends no lens, so chr renders it with
//! the pinhole camera.
//!
//! chr's mirror accepts two optional extensions that vchao never sends, for a
//! worker whose `t` maps to the ideal (undistorted) image, such as the
//! `chr-tracking-sim-cli` replay: the session's `lens` ([`FrameMeta::lens`]) and
//! each pose's `frame_warp`. Every other unknown field is still an error. It
//! also accepts vchao's `smoothed` pose ([`Pose::Smoothed`], vchao PR #232).

use serde::{Deserialize, Serialize};

/// Name of the custom meta on every frame.
pub const META: &str = "VchaoFrame";
pub const VERSION: u32 = 1;
/// Both processes stamp `source_ns` and `selected_ns` with the same host's
/// `GstSystemClock` (`CLOCK_MONOTONIC`).
pub const CLOCK_DOMAIN: &str = "same-host-absolute-gst-system-clock-v1";
/// The `vchao-layout` caps field: packed RGBA8.
pub const LAYOUT: &str = "packed-rgba-v1";
/// The `vchao-stage` caps field: timestamps are source times, not scheduled
/// output times.
pub const STAGE: &str = "source";

/// One real estimate the selected pose is built from, keyed by its own frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub frame_id: u64,
    pub sequence: u64,
    pub source_ns: u64,
    pub observed_ns: u64,
    pub t: [f64; 9],
}

/// The geometry selected for exactly this frame, tagged by `status`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Pose {
    /// No geometry; never an identity matrix.
    Unavailable {},
    /// The frame's own estimate: `t` is bit-equal to `left.t`.
    Fresh {
        t: [f64; 9],
        left: Endpoint,
        /// chr extension: see [`Pose::frame_warp`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame_warp: Option<[f64; 9]>,
    },
    /// An earlier estimate: `t` is bit-equal to `left.t`.
    Reused {
        t: [f64; 9],
        left: Endpoint,
        interpolation_failure: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame_warp: Option<[f64; 9]>,
    },
    /// Interpolated between two estimates.
    Interpolated {
        t: [f64; 9],
        left: Endpoint,
        right: Endpoint,
        alpha: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame_warp: Option<[f64; 9]>,
    },
    /// vchao's temporal smoothing (vchao PR #232): `t` is the symmetric Gaussian
    /// matrix-log average of the neighbouring estimates around `raw`, the
    /// `fresh`, `reused` or `interpolated` selection it replaces. `raw` carries
    /// no frame warp: the warp would belong to the unsmoothed `t`.
    Smoothed {
        t: [f64; 9],
        raw: Box<Pose>,
        sigma_frames: f64,
        strength: f64,
    },
}

/// What [`Pose`] a frame got, for counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoseStatus {
    Fresh,
    Interpolated,
    Reused,
    Unavailable,
}

impl Pose {
    /// The selection the pose stands for; a smoothed pose counts as its `raw`.
    pub fn status(&self) -> PoseStatus {
        match self {
            Self::Unavailable {} => PoseStatus::Unavailable,
            Self::Fresh { .. } => PoseStatus::Fresh,
            Self::Reused { .. } => PoseStatus::Reused,
            Self::Interpolated { .. } => PoseStatus::Interpolated,
            Self::Smoothed { raw, .. } => raw.status(),
        }
    }

    pub fn smoothed(&self) -> bool {
        matches!(self, Self::Smoothed { .. })
    }

    /// The selected forward row-major ground-to-SNC matrix, if any: for a
    /// smoothed pose the smoothed one.
    pub fn t(&self) -> Option<[f64; 9]> {
        match self {
            Self::Unavailable {} => None,
            Self::Fresh { t, .. }
            | Self::Reused { t, .. }
            | Self::Interpolated { t, .. }
            | Self::Smoothed { t, .. } => Some(*t),
        }
    }

    /// chr extension: this frame's warp after the session's lens, row-major,
    /// reference native pixel to frame SNC. Only with a session lens.
    pub fn frame_warp(&self) -> Option<[f64; 9]> {
        match self {
            Self::Unavailable {} | Self::Smoothed { .. } => None,
            Self::Fresh { frame_warp, .. }
            | Self::Reused { frame_warp, .. }
            | Self::Interpolated { frame_warp, .. } => *frame_warp,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    /// [`CLOCK_DOMAIN`] time at which the choice became final.
    pub selected_ns: u64,
    /// Released by the worker's overload rule before its selection was otherwise final.
    pub overdue: bool,
    pub pose: Pose,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameMeta {
    pub version: u32,
    pub stream_id: String,
    pub frame_id: u64,
    /// Capture time in [`CLOCK_DOMAIN`]; strictly increasing within a stream.
    pub source_ns: u64,
    pub clock_domain: String,
    pub duration_ns: u64,
    /// `null` from chr-server; set by the worker.
    pub selection: Option<Selection>,
    /// chr extension, never sent by chr-server or vchao: the worker session's
    /// `radial-poly-native-px-v1` lens, the same in every returned frame of the
    /// session. Absent or `null`: the session has no lens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens: Option<chr_common::scene_frame::RadialLens>,
}

impl FrameMeta {
    /// The meta chr-server sends with frame `frame_id` of `stream_id`:
    /// `selection: null` and no lens.
    pub fn request(stream_id: &str, frame_id: u64, source_ns: u64, duration_ns: u64) -> Self {
        Self {
            version: VERSION,
            stream_id: stream_id.to_owned(),
            frame_id,
            source_ns,
            clock_domain: CLOCK_DOMAIN.to_owned(),
            duration_ns,
            selection: None,
            lens: None,
        }
    }
}

/// A frame's meta from its `json` field, validated as vchao validates it:
/// well-formed, version 1 and the clock domain. The caller then checks that
/// its `frame_id` and `duration_ns` match the buffer, and [`check_selection`].
pub fn parse(json: &str) -> Result<FrameMeta, String> {
    let meta: FrameMeta =
        serde_json::from_str(json).map_err(|error| format!("malformed {META} meta: {error}"))?;
    if meta.version != VERSION {
        return Err(format!("{META} version {}, not {VERSION}", meta.version));
    }
    if meta.clock_domain != CLOCK_DOMAIN {
        return Err(format!("{META} clock domain {:?}", meta.clock_domain));
    }
    Ok(meta)
}

/// Checks a selection as vchao does: its matrices are finite and, for
/// `fresh`/`reused`, bit-equal to their endpoint's; a smoothed pose wraps a
/// valid `fresh`, `reused` or `interpolated` selection with `sigma_frames > 0`
/// and `0 < strength <= 1`.
pub fn check_selection(meta: &FrameMeta) -> Result<(), String> {
    match &meta.selection {
        Some(selection) => check_pose(&selection.pose),
        None => Ok(()),
    }
}

fn check_pose(pose: &Pose) -> Result<(), String> {
    let (t, endpoints) = match pose {
        Pose::Unavailable {} => return Ok(()),
        Pose::Fresh { t, left, .. } | Pose::Reused { t, left, .. } => {
            if t.map(f64::to_bits) != left.t.map(f64::to_bits) {
                return Err("a retained pose differs from its endpoint estimate".into());
            }
            (t, vec![left])
        }
        Pose::Interpolated { t, left, right, .. } => (t, vec![left, right]),
        Pose::Smoothed {
            t,
            raw,
            sigma_frames,
            strength,
        } => {
            if matches!(**raw, Pose::Unavailable {} | Pose::Smoothed { .. }) {
                return Err("smoothing must wrap a raw estimated selection".into());
            }
            if raw.frame_warp().is_some() {
                return Err("a smoothed pose's raw selection has a frame warp".into());
            }
            if !(*sigma_frames > 0.0 && *strength > 0.0 && *strength <= 1.0) {
                return Err("invalid smoothing parameters".into());
            }
            check_pose(raw)?;
            (t, vec![])
        }
    };
    if !t
        .iter()
        .chain(endpoints.iter().flat_map(|endpoint| endpoint.t.iter()))
        .all(|v| v.is_finite())
    {
        return Err("a non-finite selected Ti".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `selection()` of vchao's `live-video/src/unixfd.rs` tests at cd0b904,
    /// serialized the way vchao serializes it.
    const VCHAO_SELECTION: &str = r#"{"selected_ns":9,"overdue":false,"pose":{"status":"interpolated","t":[0.1,0.2,0.3333333333333333,-4e-17,5.0,6.0,7.0,8.0,0.9],"left":{"frame_id":3,"sequence":3,"source_ns":1,"observed_ns":2,"t":[1.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,1.0]},"right":{"frame_id":7,"sequence":7,"source_ns":5,"observed_ns":6,"t":[1.0,0.0,0.5,0.0,1.0,0.0,0.0,0.0,1.0]},"alpha":0.25}}"#;

    fn endpoint(t: [f64; 9]) -> Endpoint {
        Endpoint {
            frame_id: 5,
            sequence: 5,
            source_ns: 1,
            observed_ns: 2,
            t,
        }
    }

    fn json(meta: &FrameMeta) -> String {
        serde_json::to_string(meta).unwrap()
    }

    #[test]
    fn a_vchao_document_parses_and_its_bits_survive_a_round_trip() {
        let selection: Selection = serde_json::from_str(VCHAO_SELECTION).unwrap();
        let t = selection.pose.t().unwrap();
        assert_eq!(t[2].to_bits(), (1.0_f64 / 3.0).to_bits());
        assert_eq!(t[3].to_bits(), (-4e-17_f64).to_bits());
        assert_eq!(selection.pose.status(), PoseStatus::Interpolated);
        // Values whose shortest decimal needs all 17 digits.
        let hard = [
            0.1 + 0.2,
            1.0 / 3.0,
            -f64::MIN_POSITIVE,
            f64::MAX,
            5e-324,
            0.006723152590559368,
            -1.3084453865223984e-05,
            2.0_f64.sqrt(),
            -0.0,
        ];
        let pose = Pose::Fresh {
            t: hard,
            left: endpoint(hard),
            frame_warp: None,
        };
        let back: Pose = serde_json::from_str(&serde_json::to_string(&pose).unwrap()).unwrap();
        assert_eq!(back.t().unwrap().map(f64::to_bits), hard.map(f64::to_bits));
        assert_eq!(
            serde_json::to_string(&Pose::Unavailable {}).unwrap(),
            r#"{"status":"unavailable"}"#
        );
        let warped: Pose = serde_json::from_str(
            r#"{"status":"reused","t":[1,0,0,0,1,0,0,0,1],"left":{"frame_id":1,"sequence":1,"source_ns":1,"observed_ns":2,"t":[1,0,0,0,1,0,0,0,1]},"interpolation_failure":null,"frame_warp":[2,0,0,0,2,0,0,0,1]}"#,
        )
        .unwrap();
        assert_eq!(
            warped.frame_warp(),
            Some([2., 0., 0., 0., 2., 0., 0., 0., 1.])
        );
        assert_eq!(selection.pose.frame_warp(), None, "vchao sends no warp");
        let meta: FrameMeta = serde_json::from_str(
            r#"{"version":1,"stream_id":"s","frame_id":0,"source_ns":1,"clock_domain":"c","duration_ns":1,"selection":null,"lens":{"model":"radial-poly-native-px-v1","centre_px":[960,540],"scale_px":960,"k":[-0.05,0,0]}}"#,
        )
        .unwrap();
        assert_eq!(meta.lens.unwrap().k[0].get(), -0.05);
        for bad in [
            r#"{"status":"unavailable","t":[1,0,0,0,1,0,0,0,1]}"#,
            r#"{"status":"estimated","t":[1,0,0,0,1,0,0,0,1]}"#,
            r#"{"status":"fresh","t":[1,0,0,0,1,0,0,0]}"#,
            r#"{"status":"unavailable","frame_warp":[1,0,0,0,1,0,0,0,1]}"#,
            r#"{"status":"fresh","t":[1,0,0,0,1,0,0,0,1],"left":{"frame_id":1,"sequence":1,"source_ns":1,"observed_ns":2,"t":[1,0,0,0,1,0,0,0,1]},"frame_warps":[1,0,0,0,1,0,0,0,1]}"#,
        ] {
            assert!(serde_json::from_str::<Pose>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_request_has_a_null_selection_and_no_lens() {
        let meta = FrameMeta::request("chr-test", 5, 1 << 50, 20_000_000);
        assert_eq!(
            json(&meta),
            format!(
                r#"{{"version":1,"stream_id":"chr-test","frame_id":5,"source_ns":{},"clock_domain":"{CLOCK_DOMAIN}","duration_ns":20000000,"selection":null}}"#,
                1_u64 << 50
            )
        );
        assert_eq!(parse(&json(&meta)).unwrap(), meta);
    }

    #[test]
    fn malformed_meta_is_rejected() {
        let request = FrameMeta::request("chr-test", 5, 100, 20_000_000);
        assert!(parse("{}").is_err(), "not a meta");

        let mut meta = request.clone();
        meta.version = 2;
        assert!(parse(&json(&meta)).is_err(), "version 2");

        let mut meta = request.clone();
        meta.clock_domain = "another".into();
        assert!(parse(&json(&meta)).is_err(), "another clock domain");

        let id: [f64; 9] = [1., 0., 0., 0., 1., 0., 0., 0., 1.];
        let mut other = id;
        other[2] = f64::from_bits(id[2].to_bits() + 1);
        let mut meta = request.clone();
        meta.selection = Some(Selection {
            selected_ns: 7,
            overdue: false,
            pose: Pose::Fresh {
                t: other,
                left: endpoint(id),
                frame_warp: None,
            },
        });
        assert!(
            check_selection(&parse(&json(&meta)).unwrap()).is_err(),
            "fresh t differs from its endpoint"
        );

        let t = "[1e400,0,0,0,1,0,0,0,1]";
        let infinite = json(&request).replace(
            r#""selection":null"#,
            &format!(
                r#""selection":{{"selected_ns":1,"overdue":false,"pose":{{"status":"fresh","t":{t},"left":{{"frame_id":5,"sequence":5,"source_ns":1,"observed_ns":2,"t":{t}}}}}}}"#
            ),
        );
        assert!(
            parse(&infinite)
                .and_then(|meta| check_selection(&meta))
                .is_err(),
            "a non-finite Ti"
        );

        let mut extra = serde_json::to_value(&request).unwrap();
        extra["extra"] = 1.into();
        assert!(parse(&extra.to_string()).is_err(), "an unknown field");

        let mut meta = request;
        meta.selection = Some(serde_json::from_str(VCHAO_SELECTION).unwrap());
        check_selection(&parse(&json(&meta)).unwrap()).unwrap();
    }

    /// vchao PR #232's `smoothed_geometry_wraps_a_checked_raw_selection`.
    #[test]
    fn a_smoothed_pose_wraps_a_checked_raw_selection() {
        let left =
            r#"{"frame_id":1,"sequence":1,"source_ns":1,"observed_ns":2,"t":[1,0,0,0,1,0,0,0,1]}"#;
        let check = |raw: &str, sigma: f64, strength: f64| {
            let pose = format!(
                r#"{{"status":"smoothed","t":[1,0,0.5,0,1,0,0,0,1],"raw":{raw},"sigma_frames":{sigma},"strength":{strength}}}"#
            );
            let mut meta = FrameMeta::request("chr-test", 5, 100, 20_000_000);
            meta.selection = Some(Selection {
                selected_ns: 7,
                overdue: false,
                pose: serde_json::from_str(&pose).unwrap(),
            });
            check_selection(&parse(&json(&meta)).unwrap()).map(|()| meta)
        };
        let fresh = format!(r#"{{"status":"fresh","t":[1,0,0,0,1,0,0,0,1],"left":{left}}}"#);
        let pose = check(&fresh, 1.0, 0.5).unwrap().selection.unwrap().pose;
        assert!(pose.smoothed());
        assert_eq!(pose.status(), PoseStatus::Fresh, "counted as its raw");
        assert_eq!(pose.t().unwrap()[2], 0.5, "the smoothed t");
        assert_eq!(pose.frame_warp(), None);
        assert!(
            !serde_json::from_str::<Selection>(VCHAO_SELECTION)
                .unwrap()
                .pose
                .smoothed()
        );

        let altered = format!(r#"{{"status":"fresh","t":[2,0,0,0,1,0,0,0,1],"left":{left}}}"#);
        assert!(check(&altered, 1.0, 0.5).is_err(), "an invalid raw");
        assert!(check(&fresh, 1.0, 0.0).is_err(), "strength 0");
        assert!(check(&fresh, 1.0, 1.5).is_err(), "strength above 1");
        assert!(check(&fresh, 0.0, 1.0).is_err(), "sigma 0");
        assert!(check(r#"{"status":"unavailable"}"#, 1.0, 1.0).is_err());
        let warped = format!(
            r#"{{"status":"fresh","t":[1,0,0,0,1,0,0,0,1],"left":{left},"frame_warp":[1,0,0,0,1,0,0,0,1]}}"#
        );
        assert!(check(&warped, 1.0, 1.0).is_err(), "a raw frame warp");
        let nested = format!(
            r#"{{"status":"smoothed","t":[1,0,0,0,1,0,0,0,1],"raw":{fresh},"sigma_frames":1,"strength":1}}"#
        );
        assert!(
            check(&nested, 1.0, 1.0).is_err(),
            "smoothing a smoothed pose"
        );
    }
}
