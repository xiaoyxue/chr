//! The tracking hop's GStreamer side: `chr_tracker::protocol`'s `VchaoFrame`
//! v1 frames as buffers on the two unixfd sockets
//! ([`transport`](crate::worker::transport)).
//!
//! Every buffer is one video frame: packed RGBA8 as the buffer's single
//! memfd-backed memory, with the caps of [`Wire::caps`]
//! (`video/x-raw,format=RGBA` plus the `vchao-*` fields), `offset = frame_id`,
//! `offset_end = frame_id + 1`, PTS = `source_ns`, the frame duration and no DTS.
//! It carries exactly one `GstCustomMeta` named [`META`] whose structure has a
//! string field `json`: a [`FrameMeta`].

use chr_common::rate::Rate;
use chr_tracker::protocol::{self, CLOCK_DOMAIN, FrameMeta, LAYOUT, META, STAGE};
use gstreamer::{self as gst, prelude::*};

use crate::worker::transport::{Error, Inbound};

/// The current time in [`CLOCK_DOMAIN`].
pub fn now_ns() -> u64 {
    gst::SystemClock::obtain().time().nseconds()
}

/// One stream of frames: a worker session. A restarted worker gets a new stream,
/// whose `frame_id`s start at 0 again, and so does a new extent or rate: vchao
/// takes a caps change within a stream as fatal.
#[derive(Clone, Debug)]
pub struct Wire {
    pub stream_id: String,
    pub width: u32,
    pub height: u32,
    pub fps: Rate,
}

impl Wire {
    /// Nanoseconds per frame, rounded down: every frame's VchaoFrame
    /// `duration_ns`. Timestamps come from the clock, so this rounding never
    /// accumulates.
    pub fn duration_ns(&self) -> u64 {
        self.fps.period_ns()
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
            .field("framerate", fraction(self.fps))
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
            &FrameMeta::request(&self.stream_id, frame_id, source_ns, self.duration_ns()),
        )?;
        Ok(buffer)
    }
}

/// `rate` as a GStreamer fraction; [`Rate`]'s terms are 31-bit.
pub fn fraction(rate: Rate) -> gst::Fraction {
    gst::Fraction::new(rate.num() as i32, rate.den() as i32)
}

/// The rate of a caps `framerate` fraction.
pub fn rate_of(fraction: gst::Fraction) -> Result<Rate, Error> {
    let (num, den) = (fraction.numer(), fraction.denom());
    Ok(Rate::new(u32::try_from(num)?, u32::try_from(den)?)?)
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
/// matrices are finite and, for `fresh`/`reused`, bit-equal to their endpoint's
/// ([`protocol::parse`], [`protocol::check_selection`]).
pub fn read(buffer: &gst::BufferRef) -> Result<FrameMeta, Error> {
    let meta = gst::meta::CustomMeta::from_buffer(buffer, META)
        .map_err(|_| format!("a frame without the {META} meta"))?;
    let meta = protocol::parse(meta.structure().get::<&str>("json")?)?;
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
    protocol::check_selection(&meta)?;
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
    use chr_tracker::protocol::{Endpoint, Pose, Selection};

    /// `selection()` of vchao's `live-video/src/unixfd.rs` tests at cd0b904,
    /// serialized the way vchao serializes it.
    const VCHAO_SELECTION: &str = r#"{"selected_ns":9,"overdue":false,"pose":{"status":"interpolated","t":[0.1,0.2,0.3333333333333333,-4e-17,5.0,6.0,7.0,8.0,0.9],"left":{"frame_id":3,"sequence":3,"source_ns":1,"observed_ns":2,"t":[1.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,1.0]},"right":{"frame_id":7,"sequence":7,"source_ns":5,"observed_ns":6,"t":[1.0,0.0,0.5,0.0,1.0,0.0,0.0,0.0,1.0]},"alpha":0.25}}"#;

    fn wire() -> Wire {
        Wire {
            stream_id: "chr-test".into(),
            width: 4,
            height: 2,
            fps: Rate::fps(50),
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
    fn fractional_rates_are_caps_fractions_and_a_rounded_duration() {
        gst::init().unwrap();
        let ntsc = Wire {
            fps: Rate::new(60000, 1001).unwrap(),
            ..wire()
        };
        let caps = ntsc.caps();
        let structure = caps.structure(0).unwrap();
        let framerate = structure.get::<gst::Fraction>("framerate").unwrap();
        assert_eq!((framerate.numer(), framerate.denom()), (60000, 1001));
        assert_eq!(rate_of(framerate).unwrap(), ntsc.fps);
        assert_eq!(ntsc.duration_ns(), 16_683_333);
        assert_eq!(
            structure.get::<u64>("vchao-replay-duration-ns").unwrap(),
            16_683_333
        );
        assert!(rate_of(gst::Fraction::new(0, 1)).is_err());
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
    }

    #[test]
    fn frames_and_selected_ti_cross_a_socket_unchanged() {
        crate::worker::transport::init().unwrap();
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
