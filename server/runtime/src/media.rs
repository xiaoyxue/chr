//! Opt-in software video input and program-output encoding. No physical SDI.

use std::{
    os::fd::AsRawFd,
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSrc};
use gstreamer_video as video;

use crate::sdi::VideoMode;

type Error = Box<dyn std::error::Error + Send + Sync>;

pub struct InputFrame {
    pub sequence: u64,
    pub rgba: Vec<u8>,
}

struct Decoder(Child);

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub struct VideoInput {
    decoder: Decoder,
    _pipe: ChildStdout,
    pipeline: gst::Pipeline,
    sink: AppSink,
    exhausted: Arc<AtomicBool>,
    mode: VideoMode,
}

impl VideoInput {
    pub fn open(
        path: &Path,
        mode: VideoMode,
        expected_frames: Option<usize>,
    ) -> Result<Self, Error> {
        validate_mode(
            mode,
            if expected_frames.is_some() {
                [4096, 2160]
            } else {
                [1280, 720]
            },
        )?;
        gst::init()?;
        let path = path.canonicalize()?;
        let mut probe = Command::new("ffprobe");
        if expected_frames.is_some() {
            probe.args(["-count_frames", "-show_frames"]);
        }
        let probe = probe
            .args([
                "-v", "error", "-select_streams", "v:0", "-show_entries",
                "stream=width,height,avg_frame_rate,r_frame_rate,field_order,sample_aspect_ratio,color_range,color_space,color_transfer,color_primaries,nb_read_frames:frame=best_effort_timestamp_time",
                "-of", "json",
            ])
            .arg(&path)
            .output()?;
        if !probe.status.success() {
            return Err("ffprobe could not inspect the software video source".into());
        }
        let metadata: serde_json::Value = serde_json::from_slice(&probe.stdout)?;
        let stream = metadata["streams"]
            .as_array()
            .and_then(|streams| streams.first())
            .ok_or("source has no video stream")?;
        let width = stream["width"].as_u64().ok_or("missing source width")?;
        let height = stream["height"].as_u64().ok_or("missing source height")?;
        let rate = format!("{}/1", mode.fps);
        if let Some(expected) = expected_frames
            && (stream["nb_read_frames"]
                .as_str()
                .and_then(|v| v.parse::<usize>().ok())
                != Some(expected)
                || width != u64::from(mode.width)
                || height != u64::from(mode.height))
        {
            return Err(
                "calibrated video requires matching native extent and one warp per decoded frame"
                    .into(),
            );
        }
        if let Some(expected) = expected_frames {
            validate_clip_timeline(&metadata, expected, mode.fps)?;
        }
        if stream["avg_frame_rate"].as_str() != Some(&rate)
            || stream["r_frame_rate"].as_str() != Some(&rate)
            || stream["field_order"] != "progressive"
            || stream["sample_aspect_ratio"] != "1:1"
            || stream["color_range"] != "tv"
            || ["color_space", "color_transfer", "color_primaries"]
                .iter()
                .any(|key| stream[*key] != "bt709")
            || width == 0
            || height == 0
            || width * u64::from(mode.height) != height * u64::from(mode.width)
        {
            return Err("software video requires progressive square-pixel BT.709 limited input matching --fps and the output aspect ratio".into());
        }
        log::info!(
            "software input: {width}x{height} -> {}x{} at {} FPS; video only, no physical SDI",
            mode.width,
            mode.height,
            mode.fps
        );
        let filter = format!(
            "scale=w={}:h={}:in_range=tv:out_range=tv:in_color_matrix=bt709:out_color_matrix=bt709,format=uyvy422",
            mode.width, mode.height
        );
        let mut decoder = Decoder(
            Command::new("ffmpeg")
                .args([
                    "-nostdin",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-threads",
                    "2",
                    "-filter_threads",
                    "2",
                    "-re",
                    "-stream_loop",
                    "-1",
                    "-noautorotate",
                    "-i",
                ])
                .arg(path)
                .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf", &filter])
                .args([
                    "-fps_mode",
                    "passthrough",
                    "-threads",
                    "2",
                    "-pix_fmt",
                    "uyvy422",
                    "-c:v",
                    "rawvideo",
                    "-f",
                    "rawvideo",
                    "pipe:1",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()?,
        );
        let pipe = decoder
            .0
            .stdout
            .take()
            .ok_or("FFmpeg stdout was not piped")?;
        let pipeline = gst::parse::launch(&format!(
            "fdsrc fd={} ! rawvideoparse format=uyvy width={} height={} framerate={}/1 colorimetry=bt709 \
             ! videoconvert ! video/x-raw,format=RGBA,colorimetry=1:1:5:1 \
             ! appsink name=frames sync=false max-buffers=2 drop=true wait-on-eos=false",
            pipe.as_raw_fd(), mode.width, mode.height, mode.fps
        ))?.downcast::<gst::Pipeline>().map_err(|_| "input is not a pipeline")?;
        let sink = pipeline
            .by_name("frames")
            .ok_or("missing input sink")?
            .downcast::<AppSink>()
            .map_err(|_| "input sink is not appsink")?;
        let exhausted = Arc::new(AtomicBool::new(false));
        let sequence = AtomicU64::new(0);
        let failed = Arc::clone(&exhausted);
        sink.static_pad("sink")
            .ok_or("missing appsink pad")?
            .add_probe(gst::PadProbeType::BUFFER, move |_, probe| {
                let Ok(index) =
                    sequence.try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                        value.checked_add(1)
                    })
                else {
                    failed.store(true, Ordering::Release);
                    return gst::PadProbeReturn::Drop;
                };
                if let Some(gst::PadProbeData::Buffer(buffer)) = &mut probe.data {
                    buffer.make_mut().set_offset(index);
                }
                gst::PadProbeReturn::Ok
            });
        let source = Self {
            decoder,
            _pipe: pipe,
            pipeline,
            sink,
            exhausted,
            mode,
        };
        source.pipeline.set_state(gst::State::Playing)?;
        Ok(source)
    }

    pub fn try_frame(&mut self) -> Result<Option<InputFrame>, Error> {
        check_bus(&self.pipeline)?;
        if self.exhausted.load(Ordering::Acquire) {
            return Err("software input sequence exhausted".into());
        }
        let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::ZERO) else {
            if self.sink.is_eos() || self.decoder.0.try_wait()?.is_some() {
                return Err("software video source stopped".into());
            }
            return Ok(None);
        };
        let info = video::VideoInfo::from_caps(sample.caps().ok_or("input has no caps")?)?;
        if info.width() != self.mode.width
            || info.height() != self.mode.height
            || info.format() != video::VideoFormat::Rgba
            || info.stride()[0] != (self.mode.width * 4) as i32
        {
            return Err("unexpected software input image layout".into());
        }
        let buffer = sample.buffer().ok_or("input sample has no buffer")?;
        let pixels = buffer.map_readable()?;
        if pixels.len() != self.mode.width as usize * self.mode.height as usize * 4 {
            return Err("unexpected software input byte count".into());
        }
        Ok(Some(InputFrame {
            sequence: buffer.offset(),
            rgba: pixels.as_slice().to_vec(),
        }))
    }
}

fn validate_clip_timeline(
    metadata: &serde_json::Value,
    count: usize,
    fps: u32,
) -> Result<(), Error> {
    let frames = metadata["frames"]
        .as_array()
        .ok_or("missing decoded clip timestamps")?;
    if frames.len() != count
        || frames.iter().enumerate().any(|(i, frame)| {
            frame["best_effort_timestamp_time"]
                .as_str()
                .and_then(|v| v.parse::<f64>().ok())
                .is_none_or(|pts| {
                    !pts.is_finite() || (pts - i as f64 / f64::from(fps)).abs() > 0.00001
                })
        })
    {
        return Err(
            "calibrated replay requires zero-based contiguous CFR presentation timestamps".into(),
        );
    }
    Ok(())
}

impl Drop for VideoInput {
    fn drop(&mut self) {
        let _ = self.decoder.0.kill();
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

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

fn validate_mode(mode: VideoMode, max: [u32; 2]) -> Result<(), Error> {
    if mode.width == 0
        || mode.height == 0
        || mode.width > max[0]
        || mode.height > max[1]
        || !mode.width.is_multiple_of(2)
        || !mode.height.is_multiple_of(2)
        || !(1..=120).contains(&mode.fps)
    {
        return Err(
            "software video requires even dimensions within the profile limit and 1..120 FPS"
                .into(),
        );
    }
    Ok(())
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn check_bus(pipeline: &gst::Pipeline) -> Result<(), Error> {
    if let Some(message) = pipeline
        .bus()
        .and_then(|bus| bus.pop_filtered(&[gst::MessageType::Error]))
        && let gst::MessageView::Error(error) = message.view()
    {
        return Err(format!("GStreamer: {}", error.error()).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibrated_clip_rejects_missing_reordered_or_variable_timestamps() {
        let valid = serde_json::json!({"frames":[
            {"best_effort_timestamp_time":"0.000000"},
            {"best_effort_timestamp_time":"0.033333"},
            {"best_effort_timestamp_time":"0.066667"}
        ]});
        assert!(validate_clip_timeline(&valid, 3, 30).is_ok());
        assert!(validate_clip_timeline(&valid, 4, 30).is_err());
        for bad in ["0.000000", "0.040000", "NaN"] {
            let mut metadata = valid.clone();
            metadata["frames"][1]["best_effort_timestamp_time"] = bad.into();
            assert!(validate_clip_timeline(&metadata, 3, 30).is_err());
        }
    }

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
