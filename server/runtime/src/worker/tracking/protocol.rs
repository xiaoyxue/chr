//! The tracking hop's wire contract: vchao's `VchaoFrame` v1 raw-frame boundary
//! (Hong-Xiang/vchao `live-video/src/unixfd.rs` and
//! `live-video/README.md#native-unixfd-boundary`), mirrored here without
//! depending on vchao's crates.
//!
//! Every buffer on both tracking sockets is one video frame: packed RGBA8 as the
//! buffer's single memfd-backed memory, with the caps of [`Wire::caps`]
//! (`video/x-raw,format=RGBA` plus the `vchao-*` fields), `offset = frame_id`,
//! `offset_end = frame_id + 1`, PTS = `source_ns`, the frame duration and no DTS.
//! It carries exactly one `GstCustomMeta` named [`META`] whose structure has a
//! string field `json`: a [`FrameMeta`]. chr-server sends `selection: null`;
//! the worker returns every frame exactly once, in order, with the same memory
//! and `selection` set. JSON uses round-trip float formatting (serde_json's
//! `float_roundtrip`), so `t` crosses bit for bit.
//!
//! `t` is forward, row-major, acting on homogeneous column vectors: ground XY
//! metres to this frame's SNC (`server/docs/coordinates.md`). vchao estimates it
//! against the real, distorted image and sends no lens, so chr renders it with
//! the pinhole camera.
//!
//! chr's mirror accepts two optional extensions that vchao never sends, for a
//! worker whose `t` maps to the ideal (undistorted) image, such as the
//! `chr-tracking-sim-cli` replay: the session's `lens` ([`FrameMeta::lens`]) and
//! each pose's `frame_warp`. Every other unknown field is still an error.

use gstreamer::{self as gst, prelude::*};
use serde::{Deserialize, Serialize};

use crate::worker::transport::{Error, Inbound};

/// Name of the custom meta on every frame.
pub const META: &str = "VchaoFrame";
pub const VERSION: u32 = 1;
/// Both processes stamp `source_ns` and `selected_ns` with the same host's
/// `GstSystemClock` (`CLOCK_MONOTONIC`).
pub const CLOCK_DOMAIN: &str = "same-host-absolute-gst-system-clock-v1";
const LAYOUT: &str = "packed-rgba-v1";
/// Timestamps are source times, not scheduled output times.
const STAGE: &str = "source";

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
    pub fn status(&self) -> PoseStatus {
        match self {
            Self::Unavailable {} => PoseStatus::Unavailable,
            Self::Fresh { .. } => PoseStatus::Fresh,
            Self::Reused { .. } => PoseStatus::Reused,
            Self::Interpolated { .. } => PoseStatus::Interpolated,
        }
    }

    /// The selected forward row-major ground-to-SNC matrix, if any.
    pub fn t(&self) -> Option<[f64; 9]> {
        match self {
            Self::Unavailable {} => None,
            Self::Fresh { t, .. } | Self::Reused { t, .. } | Self::Interpolated { t, .. } => {
                Some(*t)
            }
        }
    }

    /// chr extension: this frame's warp after the session's lens, row-major,
    /// reference native pixel to frame SNC. Only with a session lens.
    pub fn frame_warp(&self) -> Option<[f64; 9]> {
        match self {
            Self::Unavailable {} => None,
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

/// The current time in [`CLOCK_DOMAIN`].
pub fn now_ns() -> u64 {
    gst::SystemClock::obtain().time().nseconds()
}

/// One stream of frames: a worker session. A restarted worker gets a new stream,
/// whose `frame_id`s start at 0 again.
#[derive(Clone, Debug)]
pub struct Wire {
    pub stream_id: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Wire {
    /// Nanoseconds per frame, rounded down.
    pub fn duration_ns(&self) -> u64 {
        1_000_000_000 / u64::from(self.fps)
    }

    pub fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// vchao's raw wire caps (`live_video::wire_caps`) for a live source. chr's
    /// capture is not a looped clip, so the replay window it requires is one
    /// frame long.
    pub fn caps(&self) -> gst::Caps {
        gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", self.width as i32)
            .field("height", self.height as i32)
            .field("framerate", gst::Fraction::new(self.fps as i32, 1))
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .field("interlace-mode", "progressive")
            .field("vchao-stream-id", self.stream_id.as_str())
            .field("vchao-clock-domain", CLOCK_DOMAIN)
            .field("vchao-layout", LAYOUT)
            .field("vchao-stage", STAGE)
            .field("vchao-replay-frames", 1_u64)
            .field("vchao-replay-duration-ns", self.duration_ns())
            .build()
    }

    /// The buffer that sends `memory` (one packed RGBA8 frame) as `frame_id`,
    /// with `selection: null`.
    pub fn frame(
        &self,
        frame_id: u64,
        source_ns: u64,
        memory: gst::Memory,
    ) -> Result<gst::Buffer, Error> {
        let mut buffer = gst::Buffer::new();
        let writable = buffer.get_mut().expect("new buffer is not shared");
        writable.append_memory(memory);
        writable.set_offset(frame_id);
        writable.set_offset_end(frame_id + 1);
        writable.set_pts(gst::ClockTime::from_nseconds(source_ns));
        writable.set_duration(gst::ClockTime::from_nseconds(self.duration_ns()));
        attach(
            writable,
            &FrameMeta {
                version: VERSION,
                stream_id: self.stream_id.clone(),
                frame_id,
                source_ns,
                clock_domain: CLOCK_DOMAIN.to_owned(),
                duration_ns: self.duration_ns(),
                selection: None,
                lens: None,
            },
        )?;
        Ok(buffer)
    }
}

/// Sets the buffer's meta, replacing one it already carries.
pub fn attach(buffer: &mut gst::BufferRef, meta: &FrameMeta) -> Result<(), Error> {
    crate::worker::transport::init()?;
    let json = serde_json::to_string(meta)?;
    match gst::meta::CustomMeta::from_mut_buffer(buffer, META) {
        Ok(mut existing) => existing.mut_structure().set("json", json),
        Err(_) => gst::meta::CustomMeta::add(buffer, META)?
            .mut_structure()
            .set("json", json),
    }
    Ok(())
}

/// The buffer's meta, validated as vchao validates it: present, version 1, the
/// clock domain, offset and duration matching the buffer, and a selection whose
/// matrices are finite and, for `fresh`/`reused`, bit-equal to their endpoint's.
pub fn read(buffer: &gst::BufferRef) -> Result<FrameMeta, Error> {
    let meta = gst::meta::CustomMeta::from_buffer(buffer, META)
        .map_err(|_| format!("a frame without the {META} meta"))?;
    let meta: FrameMeta = serde_json::from_str(meta.structure().get::<&str>("json")?)
        .map_err(|error| format!("malformed {META} meta: {error}"))?;
    if meta.version != VERSION {
        return Err(format!("{META} version {}, not {VERSION}", meta.version).into());
    }
    if meta.clock_domain != CLOCK_DOMAIN {
        return Err(format!("{META} clock domain {:?}", meta.clock_domain).into());
    }
    if buffer.offset() != meta.frame_id
        || buffer.duration().map(gst::ClockTime::nseconds) != Some(meta.duration_ns)
    {
        return Err(format!(
            "{META} of frame {} does not match its buffer (offset {}, duration {:?})",
            meta.frame_id,
            buffer.offset(),
            buffer.duration()
        )
        .into());
    }
    if let Some(selection) = &meta.selection {
        let (t, endpoints) = match &selection.pose {
            Pose::Unavailable {} => return Ok(meta),
            Pose::Fresh { t, left, .. } | Pose::Reused { t, left, .. } => {
                if t.map(f64::to_bits) != left.t.map(f64::to_bits) {
                    return Err("a retained pose differs from its endpoint estimate".into());
                }
                (t, vec![left])
            }
            Pose::Interpolated { t, left, right, .. } => (t, vec![left, right]),
        };
        if !t
            .iter()
            .chain(endpoints.iter().flat_map(|endpoint| endpoint.t.iter()))
            .all(|v| v.is_finite())
        {
            return Err("a non-finite selected Ti".into());
        }
    }
    Ok(meta)
}

/// Checks that `caps` are the wire caps of `stream_id`'s stream.
pub fn check_caps(caps: &gst::CapsRef, stream_id: &str) -> Result<(), Error> {
    let structure = caps.structure(0).ok_or("frame caps without a structure")?;
    let field = |name: &str| structure.get::<&str>(name).ok();
    if structure.name() != "video/x-raw"
        || field("format") != Some("RGBA")
        || field("vchao-layout") != Some(LAYOUT)
        || field("vchao-clock-domain") != Some(CLOCK_DOMAIN)
        || field("vchao-stage") != Some(STAGE)
    {
        return Err(format!("frame caps are not vchao's raw RGBA wire caps: {caps}").into());
    }
    if field("vchao-stream-id") != Some(stream_id) {
        return Err(format!("frame caps of another stream than {stream_id:?}: {caps}").into());
    }
    Ok(())
}

/// One frame read from a tracking socket, its meta validated by [`read`].
pub struct Received {
    pub meta: FrameMeta,
    pub buffer: gst::Buffer,
    pub caps: Option<gst::Caps>,
}

impl Inbound for Received {
    fn decode(sample: gst::Sample) -> Result<Self, Error> {
        let buffer = sample.buffer_owned().ok_or("a sample without a buffer")?;
        Ok(Self {
            meta: read(&buffer)?,
            caps: sample.caps_owned(),
            buffer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::transport::{Receiver, Sender, SharedPixels};

    /// `selection()` of vchao's `live-video/src/unixfd.rs` tests at cd0b904,
    /// serialized the way vchao serializes it.
    const VCHAO_SELECTION: &str = r#"{"selected_ns":9,"overdue":false,"pose":{"status":"interpolated","t":[0.1,0.2,0.3333333333333333,-4e-17,5.0,6.0,7.0,8.0,0.9],"left":{"frame_id":3,"sequence":3,"source_ns":1,"observed_ns":2,"t":[1.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,1.0]},"right":{"frame_id":7,"sequence":7,"source_ns":5,"observed_ns":6,"t":[1.0,0.0,0.5,0.0,1.0,0.0,0.0,0.0,1.0]},"alpha":0.25}}"#;

    fn wire() -> Wire {
        Wire {
            stream_id: "chr-test".into(),
            width: 4,
            height: 2,
            fps: 50,
        }
    }

    fn selected(buffer: &mut gst::Buffer, pose: Pose) {
        let mut meta = read(buffer).unwrap();
        meta.selection = Some(Selection {
            selected_ns: 7,
            overdue: false,
            pose,
        });
        attach(buffer.make_mut(), &meta).unwrap();
    }

    fn endpoint(t: [f64; 9]) -> Endpoint {
        Endpoint {
            frame_id: 5,
            sequence: 5,
            source_ns: 1,
            observed_ns: 2,
            t,
        }
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
    fn frames_carry_the_identity_timing_and_a_null_selection() {
        crate::worker::transport::init().unwrap();
        let pixels = SharedPixels::copy_from(&[3; 32]).unwrap();
        let buffer = wire().frame(5, 1 << 50, pixels.memory()).unwrap();
        assert_eq!((buffer.offset(), buffer.offset_end()), (5, 6));
        assert_eq!(buffer.pts().unwrap().nseconds(), 1 << 50);
        assert_eq!(buffer.duration().unwrap().nseconds(), 20_000_000);
        let meta = read(&buffer).unwrap();
        assert_eq!(meta.selection, None);
        let json = gst::meta::CustomMeta::from_buffer(&buffer, META)
            .unwrap()
            .structure()
            .get::<String>("json")
            .unwrap();
        assert_eq!(
            json,
            format!(
                r#"{{"version":1,"stream_id":"chr-test","frame_id":5,"source_ns":{},"clock_domain":"{CLOCK_DOMAIN}","duration_ns":20000000,"selection":null}}"#,
                1_u64 << 50
            )
        );
        check_caps(&wire().caps(), "chr-test").unwrap();
        assert!(check_caps(&wire().caps(), "another").is_err());
    }

    #[test]
    fn malformed_meta_is_rejected() {
        crate::worker::transport::init().unwrap();
        let pixels = SharedPixels::copy_from(&[3; 32]).unwrap();
        let frame = || wire().frame(5, 100, pixels.memory()).unwrap();
        let mut bare = gst::Buffer::new();
        bare.get_mut().unwrap().set_offset(5);
        assert!(read(&bare).is_err(), "no meta");

        let mut buffer = frame();
        let mut meta = read(&buffer).unwrap();
        meta.version = 2;
        attach(buffer.make_mut(), &meta).unwrap();
        assert!(read(&buffer).is_err(), "version 2");

        let mut buffer = frame();
        buffer.make_mut().set_offset(6);
        assert!(read(&buffer).is_err(), "offset is not frame_id");

        let mut buffer = frame();
        buffer
            .make_mut()
            .set_duration(gst::ClockTime::from_nseconds(1));
        assert!(read(&buffer).is_err(), "another duration");

        let mut buffer = frame();
        let id: [f64; 9] = [1., 0., 0., 0., 1., 0., 0., 0., 1.];
        let mut other = id;
        other[2] = f64::from_bits(id[2].to_bits() + 1);
        selected(
            &mut buffer,
            Pose::Fresh {
                t: other,
                left: endpoint(id),
                frame_warp: None,
            },
        );
        assert!(read(&buffer).is_err(), "fresh t differs from its endpoint");

        let mut buffer = frame();
        let t = "[1e400,0,0,0,1,0,0,0,1]";
        let json = serde_json::to_string(&read(&buffer).unwrap())
            .unwrap()
            .replace(
                r#""selection":null"#,
                &format!(
                    r#""selection":{{"selected_ns":1,"overdue":false,"pose":{{"status":"fresh","t":{t},"left":{{"frame_id":5,"sequence":5,"source_ns":1,"observed_ns":2,"t":{t}}}}}}}"#
                ),
            );
        gst::meta::CustomMeta::from_mut_buffer(buffer.make_mut(), META)
            .unwrap()
            .mut_structure()
            .set("json", json);
        assert!(read(&buffer).is_err(), "a non-finite Ti");

        let mut buffer = frame();
        let mut json = serde_json::to_value(read(&buffer).unwrap()).unwrap();
        json["extra"] = 1.into();
        gst::meta::CustomMeta::from_mut_buffer(buffer.make_mut(), META)
            .unwrap()
            .mut_structure()
            .set("json", json.to_string());
        assert!(read(&buffer).is_err(), "an unknown field");
    }

    #[test]
    fn frames_and_selected_ti_cross_a_socket_unchanged() {
        let dir = std::env::temp_dir().join(format!("chr-vchao-frame-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("frames.sock");
        let sender = Sender::listen_with(&path, Some(&wire().caps()), 4).unwrap();
        let receiver = Receiver::connect(&path).unwrap();
        let pixels = SharedPixels::copy_from(&(0..32).collect::<Vec<u8>>()).unwrap();
        let mut buffer = wire().frame(5, 1 << 50, pixels.memory()).unwrap();
        let selection: Selection = serde_json::from_str(VCHAO_SELECTION).unwrap();
        selected(&mut buffer, selection.pose.clone());
        sender.push(buffer).unwrap();
        let received = Received::decode(receiver.reader().recv_sample().unwrap().unwrap()).unwrap();
        check_caps(received.caps.as_ref().unwrap(), "chr-test").unwrap();
        assert_eq!(received.meta.frame_id, 5);
        let pose = received.meta.selection.unwrap().pose;
        assert_eq!(
            pose.t().unwrap().map(f64::to_bits),
            selection.pose.t().unwrap().map(f64::to_bits)
        );
        assert_eq!(
            *received.buffer.map_readable().unwrap(),
            *(0..32).collect::<Vec<u8>>()
        );
        drop((sender, receiver));
        let _ = std::fs::remove_dir_all(dir);
    }
}
