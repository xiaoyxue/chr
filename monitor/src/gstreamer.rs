//! Software H.264 encoding of transmitted SDI slots, shared by WebRTC and SRT.

use crate::{
    VideoMode,
    gstreamer_support::{Error, check_bus, validate_mode},
};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSrc};
use gstreamer_video as video;

pub struct Encoder {
    pipeline: gst::Pipeline,
    input: AppSrc,
    output: AppSink,
    encoder: gst::Element,
    mode: VideoMode,
    timestamp_offset: Option<i128>,
}

impl Encoder {
    pub fn new(mode: VideoMode) -> Result<Self, Error> {
        validate_mode(mode, [1280, 720])?;
        gst::init()?;
        let pipeline = gst::parse::launch(&format!(
            "appsrc name=input is-live=true format=time block=false max-buffers=2 \
             caps=video/x-raw,format=UYVY,width={},height={},framerate={}/1,colorimetry=bt709,interlace-mode=progressive \
             ! videoconvert ! video/x-raw,format=I420,colorimetry=bt709 \
             ! x264enc name=encoder tune=zerolatency speed-preset=ultrafast bitrate=4000 key-int-max={} bframes=0 threads=2 \
             ! video/x-h264,profile=constrained-baseline,stream-format=byte-stream,alignment=au \
             ! appsink name=encoded sync=false max-buffers=2 drop=false wait-on-eos=false",
            mode.width, mode.height, mode.fps, mode.fps * 2
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
            timestamp_offset: None,
        };
        result.pipeline.set_state(gst::State::Playing)?;
        Ok(result)
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
        let mut buffer = gst::Buffer::from_mut_slice(pixels.to_vec());
        let writable = buffer.get_mut().ok_or("new buffer is not writable")?;
        writable.set_pts(pts);
        writable.set_duration(gst::ClockTime::SECOND / u64::from(self.mode.fps));
        self.input.push_buffer(buffer)?;
        let sample = self.output.try_pull_sample(gst::ClockTime::from_seconds(1));
        check_bus(&self.pipeline)?;
        let sample = sample.ok_or("GStreamer monitor encoding timed out")?;
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

    #[test]
    fn encodes_actual_output_slots_and_forces_keyframes_after_gaps() {
        let mode = VideoMode {
            width: 64,
            height: 48,
            fps: 30,
        };
        let mut encoder = Encoder::new(mode).unwrap();
        let pixels = [128, 80, 128, 80].repeat(64 * 48 / 2);
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
    }
}
