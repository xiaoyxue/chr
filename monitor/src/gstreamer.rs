//! GStreamer H.264 encoding of transmitted SDI slots, shared by WebRTC and SRT: the
//! native UYVY slot is scaled down to the preview size on the CPU and encoded with
//! NVENC (`nvh264enc`) or x264.

use crate::{
    VideoMode,
    gstreamer_support::{Error, check_bus, validate_mode},
};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSrc};
use gstreamer_video as video;

/// A GStreamer H.264 encoder element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264 {
    /// `nvh264enc`: NVENC on the NVIDIA GPU, from gst-plugins-bad's nvcodec.
    Nvenc,
    /// `x264enc`: software, from gst-plugins-ugly.
    X264,
}

/// Which encoder `--encoder` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// NVENC when it works for the preview, otherwise x264.
    Auto,
    Nvenc,
    X264,
}

/// NVENC's smallest H.264 frame (the `nvh264enc` pad templates); its largest,
/// 4096x4096, is beyond every preview.
const NVENC_MIN: [u32; 2] = [160, 64];

impl H264 {
    pub fn element(self) -> &'static str {
        match self {
            Self::Nvenc => "nvh264enc",
            Self::X264 => "x264enc",
        }
    }

    /// Picks the encoder for `preview`; `nvenc` checks that NVENC works.
    pub fn choose(
        choice: Choice,
        preview: [u32; 2],
        nvenc: impl FnOnce() -> Result<(), Error>,
    ) -> Result<Self, Error> {
        let fits = preview[0] >= NVENC_MIN[0] && preview[1] >= NVENC_MIN[1];
        match choice {
            Choice::X264 => Ok(Self::X264),
            Choice::Nvenc if !fits => Err(format!(
                "NVENC needs at least {}x{}; the preview is {}x{}",
                NVENC_MIN[0], NVENC_MIN[1], preview[0], preview[1]
            )
            .into()),
            Choice::Nvenc => nvenc()
                .map(|()| Self::Nvenc)
                .map_err(|error| format!("NVENC is unavailable: {error}").into()),
            Choice::Auto if !fits => Ok(Self::X264),
            Choice::Auto => Ok(match nvenc() {
                Ok(()) => Self::Nvenc,
                Err(error) => {
                    log::warn!("NVENC is unavailable ({error}); encoding with x264");
                    Self::X264
                }
            }),
        }
    }

    /// The encoder and its output caps, in `gst-launch` syntax.
    fn pipeline(self, preview: [u32; 2], fps: u32) -> String {
        // About 4 Mbit/s for 720p, scaled with the preview's pixels.
        let kbit =
            (u64::from(preview[0]) * u64::from(preview[1]) * 4000 / (1280 * 720)).clamp(1000, 8000);
        let gop = fps * 2;
        let encoder = match self {
            Self::Nvenc => format!(
                "nvh264enc name=encoder preset=p1 tune=ultra-low-latency rc-mode=cbr \
                 bitrate={kbit} gop-size={gop} bframes=0 zerolatency=true repeat-sequence-header=true"
            ),
            Self::X264 => format!(
                "x264enc name=encoder tune=zerolatency speed-preset=ultrafast bitrate={kbit} \
                 key-int-max={gop} bframes=0 threads=2"
            ),
        };
        format!(
            "{encoder} ! video/x-h264,profile=constrained-baseline,stream-format=byte-stream,alignment=au"
        )
    }

    fn input_format(self) -> &'static str {
        match self {
            Self::Nvenc => "NV12",
            Self::X264 => "I420",
        }
    }
}

/// Checks that `nvh264enc` exists and encodes a frame of `preview`.
pub fn probe_nvenc(preview: [u32; 2]) -> Result<(), Error> {
    gst::init()?;
    // A registry scanned without the NVIDIA driver libraries in the library path
    // (outside nixGL, for example) caches nvcodec without elements; loading the
    // plugin registers them now if CUDA and NVENC load.
    let _ = gst::Plugin::load_by_name("nvcodec");
    if gst::ElementFactory::find("nvh264enc").is_none() {
        return Err(
            "GStreamer has no nvh264enc; it needs nvcodec and the NVIDIA driver's \
                    libcuda and libnvidia-encode (on non-NixOS hosts run through nixGL)"
                .into(),
        );
    }
    let pipeline = gst::parse::launch(&format!(
        "videotestsrc num-buffers=1 ! video/x-raw,format=NV12,width={},height={},framerate=30/1 \
         ! nvh264enc ! fakesink",
        preview[0], preview[1]
    ))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| "NVENC probe is not a pipeline")?;
    pipeline.set_state(gst::State::Playing)?;
    let message = pipeline.bus().and_then(|bus| {
        bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
    });
    let _ = pipeline.set_state(gst::State::Null);
    match message.as_ref().map(|message| message.view()) {
        Some(gst::MessageView::Eos(_)) => Ok(()),
        Some(gst::MessageView::Error(error)) => Err(error.error().to_string().into()),
        _ => Err("nvh264enc did not encode a test frame within 5 s".into()),
    }
}

pub struct Encoder {
    pipeline: gst::Pipeline,
    input: AppSrc,
    output: AppSink,
    encoder: gst::Element,
    mode: VideoMode,
    h264: H264,
    preview: [u32; 2],
    timestamp_offset: Option<i128>,
    /// The last input buffer, refilled once the pipeline has released it: a fresh
    /// 4K buffer costs page faults on every frame.
    spare: Option<gst::Buffer>,
}

impl Encoder {
    pub fn new(mode: VideoMode, choice: Choice) -> Result<Self, Error> {
        validate_mode(mode, crate::MAX_SIZE)?;
        gst::init()?;
        let preview = mode.preview();
        let h264 = H264::choose(choice, preview, || probe_nvenc(preview))?;
        // Scaling a 4K slot down costs about 17 ms of CPU; four threads keep its
        // latency under 10 ms. Without a separate chroma resampling pass bilinear
        // scaling costs about a third less, invisible in a preview.
        let threads = (mode.width * mode.height).div_ceil(1920 * 1080).clamp(1, 4);
        let pipeline = gst::parse::launch(&format!(
            "appsrc name=input is-live=true format=time block=false max-buffers=2 \
             caps=video/x-raw,format=UYVY,width={},height={},framerate={}/1,colorimetry=bt709,interlace-mode=progressive \
             ! videoconvertscale method=bilinear chroma-mode=none n-threads={threads} dither=none \
             ! video/x-raw,format={},width={},height={},pixel-aspect-ratio=1/1,colorimetry=bt709 \
             ! {} ! appsink name=encoded sync=false max-buffers=2 drop=false wait-on-eos=false",
            mode.width, mode.height, mode.fps, h264.input_format(), preview[0], preview[1],
            h264.pipeline(preview, mode.fps)
        ))?.downcast::<gst::Pipeline>().map_err(|_| "encoder is not a pipeline")?;
        let input = pipeline
            .by_name("input")
            .ok_or("missing encoder input")?
            .downcast::<AppSrc>()
            .map_err(|_| "encoder input is not appsrc")?;
        let output = pipeline
            .by_name("encoded")
            .ok_or("missing encoder output")?
            .downcast::<AppSink>()
            .map_err(|_| "encoder output is not appsink")?;
        let encoder = pipeline.by_name("encoder").ok_or("missing H.264 encoder")?;
        let result = Self {
            pipeline,
            input,
            output,
            encoder,
            mode,
            h264,
            preview,
            timestamp_offset: None,
            spare: None,
        };
        result.pipeline.set_state(gst::State::Playing)?;
        Ok(result)
    }

    /// The encoder element in use.
    pub fn h264(&self) -> H264 {
        self.h264
    }

    /// The encoded size.
    pub fn preview(&self) -> [u32; 2] {
        self.preview
    }

    pub fn encode(
        &mut self,
        slot: u64,
        pixels: &[u8],
        keyframe: bool,
    ) -> Result<(Vec<u8>, bool), Error> {
        check_bus(&self.pipeline)?;
        if pixels.len() != self.mode.width as usize * self.mode.height as usize * 2 {
            return Err("unexpected SDI monitor frame size".into());
        }
        if keyframe {
            let event = video::UpstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            if !self
                .encoder
                .static_pad("src")
                .ok_or("missing encoder pad")?
                .send_event(event)
            {
                return Err("encoder rejected keyframe request".into());
            }
        }
        let pts = gst::ClockTime::from_nseconds(
            (u128::from(slot) * 1_000_000_000 / u128::from(self.mode.fps)).try_into()?,
        );
        let mut buffer = match self.spare.take() {
            Some(buffer) if buffer.is_writable() => buffer,
            _ => gst::Buffer::with_size(pixels.len())?,
        };
        let writable = buffer.get_mut().ok_or("input buffer is not writable")?;
        writable
            .copy_from_slice(0, pixels)
            .map_err(|_| "could not fill the encoder input")?;
        writable.set_pts(pts);
        writable.set_duration(gst::ClockTime::SECOND / u64::from(self.mode.fps));
        self.input.push_buffer(buffer.clone())?;
        let sample = self.output.try_pull_sample(gst::ClockTime::from_seconds(1));
        check_bus(&self.pipeline)?;
        let sample = sample.ok_or("GStreamer monitor encoding timed out")?;
        self.spare = Some(buffer);
        let buffer = sample.buffer().ok_or("encoder returned no buffer")?;
        // x264enc adds a fixed timestamp offset; never associate a delayed AU with
        // a different transmitted slot, even after gaps in the monitor tap.
        let encoded_pts = buffer.pts().ok_or("encoded frame has no timestamp")?;
        let offset = i128::from(encoded_pts.nseconds()) - i128::from(pts.nseconds());
        if self
            .timestamp_offset
            .is_some_and(|expected| offset != expected)
        {
            return Err("encoded frame does not match the transmitted SDI slot".into());
        }
        self.timestamp_offset = Some(offset);
        let keyframe = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
        let bytes = buffer.map_readable()?.as_slice().to_vec();
        Ok((bytes, keyframe))
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unavailable() -> Result<(), Error> {
        Err("no GPU".into())
    }

    #[test]
    fn chooses_nvenc_when_it_works_and_falls_back_to_x264() {
        let full = [1920, 1080];
        assert_eq!(
            H264::choose(Choice::Auto, full, || Ok(())).unwrap(),
            H264::Nvenc
        );
        assert_eq!(
            H264::choose(Choice::Auto, full, unavailable).unwrap(),
            H264::X264
        );
        // Below NVENC's minimum size the probe is not even tried.
        assert_eq!(
            H264::choose(Choice::Auto, [64, 48], || panic!("probed")).unwrap(),
            H264::X264
        );
        assert_eq!(
            H264::choose(Choice::X264, full, || panic!("probed")).unwrap(),
            H264::X264
        );
        assert_eq!(
            H264::choose(Choice::Nvenc, full, || Ok(())).unwrap(),
            H264::Nvenc
        );
        assert!(H264::choose(Choice::Nvenc, full, unavailable).is_err());
        assert!(H264::choose(Choice::Nvenc, [64, 48], || Ok(())).is_err());
    }

    fn encodes_slots_and_forces_keyframes(mode: VideoMode, choice: Choice) -> Encoder {
        let mut encoder = Encoder::new(mode, choice).unwrap();
        let pixels = [128, 80, 128, 80].repeat((mode.width * mode.height / 2) as usize);
        let (first, keyframe) = encoder.encode(7, &pixels, true).unwrap();
        assert!(keyframe);
        assert!(openh264::nal_units(&first).any(|unit| {
            unit.iter()
                .position(|byte| *byte == 1)
                .is_some_and(|start| unit.get(start + 1).is_some_and(|header| header & 31 == 5))
        }));
        let (_, keyframe) = encoder.encode(8, &pixels, false).unwrap();
        assert!(!keyframe);
        let (_, keyframe) = encoder.encode(19, &pixels, true).unwrap();
        assert!(keyframe);
        assert!(encoder.encode(20, &pixels[..100], false).is_err());
        encoder
    }

    #[test]
    fn encodes_actual_output_slots_and_forces_keyframes_after_gaps() {
        let mode = VideoMode {
            width: 64,
            height: 48,
            fps: 30,
        };
        let encoder = encodes_slots_and_forces_keyframes(mode, Choice::Auto);
        assert_eq!(encoder.h264(), H264::X264);
        assert_eq!(encoder.preview(), [64, 48]);
    }

    #[test]
    fn scales_a_4k_slot_down_to_1080p() {
        let mode = VideoMode {
            width: 3840,
            height: 2160,
            fps: 30,
        };
        let encoder = encodes_slots_and_forces_keyframes(mode, Choice::X264);
        assert_eq!(encoder.preview(), [1920, 1080]);
    }

    /// Needs an NVIDIA GPU with NVENC and its driver libraries in the library path.
    #[test]
    #[ignore = "requires NVENC"]
    fn encodes_with_nvenc() {
        let mode = VideoMode {
            width: 3840,
            height: 2160,
            fps: 30,
        };
        let encoder = encodes_slots_and_forces_keyframes(mode, Choice::Nvenc);
        assert_eq!(encoder.h264(), H264::Nvenc);
        assert_eq!(encoder.preview(), [1920, 1080]);
    }
}
