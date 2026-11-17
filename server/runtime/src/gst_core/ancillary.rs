//! A frame's ancillary data besides its pixels: its timecode (RP 188 / VITC,
//! `GstVideoTimeCodeMeta`) and its captions (CEA-608/708,
//! `GstVideoCaptionMeta`), from the input to the output with the frame.
//!
//! GStreamer 1.28's `unixfdsink` sends only metas that can serialize
//! themselves, and neither of those can: a probe sent a timecode-stamped
//! frame across a socket and it arrived with no meta. The capture hop
//! (`chr-sdi-in` to chr-server) is chr's own, so the input carries both in a
//! `GstCustomMeta` of its own, [`CAPTURE_META`], which serializes as JSON like
//! the VchaoFrame meta beside it; the core puts them back on the program frame
//! as GStreamer's metas, which `decklinkvideosink` outputs (`timecode-format`,
//! `cc-line`). The tracking hop rebuilds every frame from its memory with only
//! the VchaoFrame meta, so the tracking worker never sees this one.

use std::sync::OnceLock;

use chr_common::rate::Rate;
use gstreamer as gst;
use gstreamer_video::{
    self as gst_video, ValidVideoTimeCode, VideoCaptionType, VideoTimeCodeFlags,
};
use serde::{Deserialize, Serialize};

use super::Error;
use crate::sdi::Scan;

/// Name of the capture hop's meta: a structure with one string field `json`,
/// an [`Ancillary`].
pub const CAPTURE_META: &str = "chr-capture-ancillary";

/// Registers [`CAPTURE_META`]; idempotent.
pub fn init() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| gst::meta::CustomMeta::register(CAPTURE_META, &[]));
}

/// SMPTE timecode of a frame: `hh:mm:ss:ff`, counting frames at `fps`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timecode {
    pub hours: u32,
    pub minutes: u32,
    pub seconds: u32,
    pub frames: u32,
    /// The field it labels in an interlaced frame (1 or 2); 0 if progressive.
    pub field_count: u32,
    /// The frame rate it counts at (25 for 1080i50).
    pub fps: Rate,
    /// Drop-frame counting, at 30000/1001 and 60000/1001.
    pub drop_frame: bool,
    pub interlaced: bool,
}

impl Timecode {
    /// The timecode of the frame `since_midnight` after midnight at `fps`:
    /// the time of day, as a live camera's free-running timecode counts it.
    pub fn time_of_day(
        fps: Rate,
        scan: Scan,
        since_midnight: std::time::Duration,
    ) -> Result<Self, Error> {
        let midnight = Self::start(fps, scan, 0, 0, 0, 0)?;
        let nanos = u64::try_from(since_midnight.as_nanos() % 86_400_000_000_000)?;
        midnight.add_frames(fps.frame_at(nanos))
    }

    /// `hh:mm:ss:ff` at `fps`; drop-frame at the 1000/1001 rates.
    pub fn start(
        fps: Rate,
        scan: Scan,
        hours: u32,
        minutes: u32,
        seconds: u32,
        frames: u32,
    ) -> Result<Self, Error> {
        let interlaced = scan != Scan::Progressive;
        let tc = Self {
            hours,
            minutes,
            seconds,
            frames,
            field_count: u32::from(interlaced),
            fps,
            drop_frame: fps.den() == 1001 && fps.num().is_multiple_of(30000),
            interlaced,
        };
        tc.to_gst()?;
        Ok(tc)
    }

    /// `hh:mm:ss:ff` or `hh:mm:ss;ff` (drop-frame) at `fps`, as FFmpeg's
    /// `timecode` tag writes it.
    pub fn parse(text: &str, fps: Rate, scan: Scan) -> Result<Self, Error> {
        let parts: Vec<&str> = text.split([':', ';', '.']).collect();
        let [hours, minutes, seconds, frames] = parts[..] else {
            return Err(format!("{text:?} is not a timecode hh:mm:ss:ff").into());
        };
        Self::start(
            fps,
            scan,
            hours.parse()?,
            minutes.parse()?,
            seconds.parse()?,
            frames.parse()?,
        )
    }

    /// This timecode `frames` frames later.
    pub fn add_frames(&self, frames: u64) -> Result<Self, Error> {
        let mut tc = self.to_gst()?;
        tc.add_frames(i64::try_from(frames)?);
        Self::of_gst(&tc)
    }

    /// Frames since midnight: consecutive frames count one apart.
    pub fn frame_count(&self) -> Result<u64, Error> {
        Ok(self.to_gst()?.frames_since_daily_jam())
    }

    pub fn to_gst(&self) -> Result<ValidVideoTimeCode, Error> {
        let mut flags = VideoTimeCodeFlags::empty();
        if self.drop_frame {
            flags |= VideoTimeCodeFlags::DROP_FRAME;
        }
        if self.interlaced {
            flags |= VideoTimeCodeFlags::INTERLACED;
        }
        Ok(ValidVideoTimeCode::new(
            crate::worker::tracking::wire::fraction(self.fps),
            None,
            flags,
            self.hours,
            self.minutes,
            self.seconds,
            self.frames,
            self.field_count,
        )?)
    }

    pub fn of_gst(tc: &ValidVideoTimeCode) -> Result<Self, Error> {
        Ok(Self {
            hours: tc.hours(),
            minutes: tc.minutes(),
            seconds: tc.seconds(),
            frames: tc.frames(),
            field_count: tc.field_count(),
            fps: crate::worker::tracking::wire::rate_of(tc.fps())?,
            drop_frame: tc.flags().contains(VideoTimeCodeFlags::DROP_FRAME),
            interlaced: tc.flags().contains(VideoTimeCodeFlags::INTERLACED),
        })
    }
}

impl std::fmt::Display for Timecode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let separator = if self.drop_frame { ';' } else { ':' };
        write!(
            f,
            "{:02}:{:02}:{:02}{separator}{:02}",
            self.hours, self.minutes, self.seconds, self.frames
        )
    }
}

/// The caption formats of `GstVideoCaptionMeta`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptionKind {
    Cea608Raw,
    /// CEA-608 as SMPTE 334-1 Annex A triplets (field byte and a byte pair).
    Cea608S3341a,
    Cea708Raw,
    /// CEA-708 caption distribution packets, as SDI carries them (SMPTE 334).
    Cea708Cdp,
}

impl CaptionKind {
    fn gst(self) -> VideoCaptionType {
        match self {
            Self::Cea608Raw => VideoCaptionType::Cea608Raw,
            Self::Cea608S3341a => VideoCaptionType::Cea608S3341a,
            Self::Cea708Raw => VideoCaptionType::Cea708Raw,
            Self::Cea708Cdp => VideoCaptionType::Cea708Cdp,
        }
    }

    fn of_gst(kind: VideoCaptionType) -> Option<Self> {
        match kind {
            VideoCaptionType::Cea608Raw => Some(Self::Cea608Raw),
            VideoCaptionType::Cea608S3341a => Some(Self::Cea608S3341a),
            VideoCaptionType::Cea708Raw => Some(Self::Cea708Raw),
            VideoCaptionType::Cea708Cdp => Some(Self::Cea708Cdp),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caption {
    pub kind: CaptionKind,
    pub data: Vec<u8>,
}

/// What a captured frame carries besides its pixels; an audio buffer, its
/// capture time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ancillary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timecode: Option<Timecode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captions: Vec<Caption>,
    /// An audio buffer's capture time on the same-host system clock (a frame's
    /// is its VchaoFrame `source_ns`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_ns: Option<u64>,
}

impl Ancillary {
    pub fn is_empty(&self) -> bool {
        self.timecode.is_none() && self.captions.is_empty()
    }

    /// The timecode and captions of GStreamer's own metas on `buffer`, as an
    /// input card's source element attaches them.
    pub fn of_gst_metas(buffer: &gst::BufferRef) -> Self {
        Self {
            timecode: buffer
                .meta::<gst_video::VideoTimeCodeMeta>()
                .and_then(|meta| Timecode::of_gst(&meta.tc()).ok()),
            captions: buffer
                .iter_meta::<gst_video::VideoCaptionMeta>()
                .filter_map(|meta| {
                    Some(Caption {
                        kind: CaptionKind::of_gst(meta.caption_type())?,
                        data: meta.data().to_vec(),
                    })
                })
                .collect(),
            capture_ns: None,
        }
    }

    /// Puts them on `buffer` as GStreamer's metas, for the output.
    pub fn attach_gst(&self, buffer: &mut gst::BufferRef) -> Result<(), Error> {
        if let Some(timecode) = &self.timecode {
            gst_video::VideoTimeCodeMeta::add(buffer, &timecode.to_gst()?);
        }
        for caption in self
            .captions
            .iter()
            .filter(|caption| !caption.data.is_empty())
        {
            gst_video::VideoCaptionMeta::add(buffer, caption.kind.gst(), &caption.data);
        }
        Ok(())
    }

    /// Carries them across the capture hop: [`CAPTURE_META`] on `buffer`.
    pub fn attach(&self, buffer: &mut gst::BufferRef) -> Result<(), Error> {
        init();
        let json = serde_json::to_string(self)?;
        gst::meta::CustomMeta::add(buffer, CAPTURE_META)?
            .mut_structure()
            .set("json", json);
        Ok(())
    }

    /// What the capture hop carried, if anything.
    pub fn read(buffer: &gst::BufferRef) -> Result<Option<Self>, Error> {
        init();
        let Ok(meta) = gst::meta::CustomMeta::from_buffer(buffer, CAPTURE_META) else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(
            meta.structure().get::<&str>("json")?,
        )?))
    }
}

/// A CEA-608 field-1 byte pair with odd parity in each byte, as SMPTE 334-1
/// Annex A carries it: the test pattern's synthetic captions, two digits
/// counting frames.
pub fn cea608_digits(frame: u64) -> Caption {
    let odd = |byte: u8| byte | (u8::from(byte.count_ones().is_multiple_of(2)) << 7);
    let tens = b'0' + (frame / 10 % 10) as u8;
    let units = b'0' + (frame % 10) as u8;
    Caption {
        kind: CaptionKind::Cea608S3341a,
        data: vec![0x80, odd(tens), odd(units)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdi::FieldOrder;

    #[test]
    fn timecodes_count_frames_with_drop_frame_at_ntsc_rates() {
        gst::init().unwrap();
        let tff = Scan::Interlaced(FieldOrder::TopFirst);
        let pal = Timecode::parse("10:00:00:00", Rate::fps(25), tff).unwrap();
        assert_eq!(
            (pal.field_count, pal.interlaced, pal.drop_frame),
            (1, true, false)
        );
        assert_eq!(
            pal.add_frames(25 * 61 + 3).unwrap().to_string(),
            "10:01:01:03"
        );
        assert_eq!(
            pal.add_frames(1).unwrap().frame_count().unwrap(),
            pal.frame_count().unwrap() + 1
        );
        // 29.97 drops frames 0 and 1 of every minute but each tenth.
        let ntsc = Rate::new(30000, 1001).unwrap();
        let df = Timecode::parse("00:00:59;29", ntsc, Scan::Progressive).unwrap();
        assert!(df.drop_frame);
        assert_eq!(df.add_frames(1).unwrap().to_string(), "00:01:00;02");
        // An hour of 59.94 is 215784 frames of drop-frame timecode.
        let fast = Timecode::start(
            Rate::new(60000, 1001).unwrap(),
            Scan::Progressive,
            0,
            0,
            0,
            0,
        )
        .unwrap();
        assert_eq!(fast.add_frames(215_784).unwrap().to_string(), "01:00:00;00");
        let noon = Timecode::time_of_day(
            Rate::fps(50),
            Scan::Progressive,
            std::time::Duration::from_millis(12 * 3_600_000 + 30),
        )
        .unwrap();
        assert_eq!(noon.to_string(), "12:00:00:01");
        assert!(Timecode::parse("10:00:00", Rate::fps(25), tff).is_err());
        assert!(Timecode::parse("10:00:00:25", Rate::fps(25), tff).is_err());
    }

    #[test]
    fn ancillary_crosses_the_capture_hop_and_becomes_gstreamer_metas() {
        gst::init().unwrap();
        let ancillary = Ancillary {
            timecode: Some(
                Timecode::parse("01:02:03:04", Rate::fps(50), Scan::Progressive).unwrap(),
            ),
            captions: vec![cea608_digits(42)],
            capture_ns: None,
        };
        assert_eq!(ancillary.captions[0].data, [0x80, 0x34, 0x32]);
        let mut buffer = gst::Buffer::new();
        ancillary.attach(buffer.get_mut().unwrap()).unwrap();
        assert_eq!(Ancillary::read(&buffer).unwrap(), Some(ancillary.clone()));
        assert_eq!(Ancillary::read(&gst::Buffer::new()).unwrap(), None);
        let mut output = gst::Buffer::new();
        ancillary.attach_gst(output.get_mut().unwrap()).unwrap();
        assert_eq!(Ancillary::of_gst_metas(&output), ancillary);
    }
}
