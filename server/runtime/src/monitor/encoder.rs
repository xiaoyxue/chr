//! H.264 encoding and the bounded fan-out of encoded frames to WebRTC viewers.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use bytes::Bytes;
use openh264::{
    OpenH264API,
    encoder::{
        BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Profile,
        RateControlMode, UsageType, VuiConfig,
    },
    formats::YUVSource,
};
use tokio::sync::broadcast;

/// One encoded H.264 access unit in Annex B format.
#[derive(Debug)]
pub struct EncodedFrame {
    /// Output slot index; consecutive slots are one frame duration apart.
    pub frame: u64,
    /// True for IDR frames, which carry SPS/PPS and start a decodable sequence.
    pub keyframe: bool,
    pub data: Bytes,
    /// Native sidecar only; never a new public wire envelope.
    pub program: Option<crate::sdi::ProgramAssociation>,
}

/// Bounded fan-out from the monitor encoder to viewers. A viewer that falls behind
/// loses frames rather than delaying the encoder or other viewers.
pub struct VideoFeed {
    frames: broadcast::Sender<Arc<EncodedFrame>>,
    keyframe_requested: AtomicBool,
}

impl Default for VideoFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoFeed {
    /// Frames kept for a slow viewer before it is considered lagged.
    const CAPACITY: usize = 4;

    pub fn new() -> Self {
        Self {
            frames: broadcast::Sender::new(Self::CAPACITY),
            keyframe_requested: AtomicBool::new(false),
        }
    }

    /// The monitor skips encoding while nobody watches.
    pub fn has_viewers(&self) -> bool {
        self.frames.receiver_count() > 0
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<EncodedFrame>> {
        self.frames.subscribe()
    }

    pub fn publish(&self, frame: EncodedFrame) {
        // No receivers is not an error: the last viewer may have just left.
        let _ = self.frames.send(Arc::new(frame));
    }

    /// Asks the encoder for an IDR frame, for a new viewer or one that lost frames.
    pub fn request_keyframe(&self) {
        self.keyframe_requested.store(true, Ordering::Relaxed);
    }

    pub fn take_keyframe_request(&self) -> bool {
        self.keyframe_requested.swap(false, Ordering::Relaxed)
    }
}

/// Software H.264 encoder (Constrained Baseline, low-latency settings).
pub struct H264Encoder {
    encoder: Encoder,
}

/// Annex B bytes and whether they form a keyframe, or `None` if no frame was produced.
pub type EncodeResult = Result<Option<(Vec<u8>, bool)>, openh264::Error>;

impl H264Encoder {
    /// `vui` signals the YCbCr matrix and range decoders must use; it has to match how
    /// the encoded YUV was produced. The frame size comes from each encoded frame.
    pub fn new(fps: u32, vui: VuiConfig) -> Result<Self, openh264::Error> {
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .profile(Profile::Baseline)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(4_000_000))
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            // A periodic IDR bounds recovery time if a keyframe request is ever missed.
            .intra_frame_period(IntraFramePeriod::from_num_frames(fps.saturating_mul(2)))
            .skip_frames(false)
            .vui(vui);
        Ok(Self {
            encoder: Encoder::with_api_config(OpenH264API::from_source(), config)?,
        })
    }

    /// Encodes planar 4:2:0 YUV whose colorimetry matches this encoder's VUI.
    pub fn encode_yuv(&mut self, yuv: &impl YUVSource, force_keyframe: bool) -> EncodeResult {
        if force_keyframe {
            self.encoder.force_intra_frame();
        }
        let bitstream = self.encoder.encode(yuv)?;
        let keyframe = match bitstream.frame_type() {
            FrameType::IDR | FrameType::I => true,
            FrameType::P | FrameType::IPMixed => false,
            FrameType::Skip | FrameType::Invalid => return Ok(None),
        };
        Ok(Some((bitstream.to_vec(), keyframe)))
    }
}

#[cfg(test)]
mod tests {
    use openh264::formats::YUVBuffer;

    use super::*;

    /// Splits Annex B data into NAL unit types.
    fn nal_types(data: &[u8]) -> Vec<u8> {
        openh264::nal_units(data)
            .filter_map(|nal| {
                let start = nal.iter().position(|&byte| byte == 1)? + 1;
                nal.get(start).map(|header| header & 0x1f)
            })
            .collect()
    }

    #[test]
    fn first_and_forced_frames_are_idr_with_parameter_sets() {
        let (width, height) = (64, 48);
        let mut encoder = H264Encoder::new(30, VuiConfig::bt709()).unwrap();
        let mut frame = YUVBuffer::new(width as usize, height as usize);
        let (first, keyframe) = encoder.encode_yuv(&frame, false).unwrap().unwrap();
        assert!(keyframe);
        let types = nal_types(&first);
        assert!(
            types.contains(&7) && types.contains(&8) && types.contains(&5),
            "{types:?}"
        );

        // A small change; a drastic one would trigger scene-change IDR frames.
        let mut changed = vec![0u8; (width * height * 3 / 2) as usize];
        changed[..64].fill(200);
        frame = YUVBuffer::from_vec(changed, width as usize, height as usize);
        let (_, keyframe) = encoder.encode_yuv(&frame, false).unwrap().unwrap();
        assert!(!keyframe, "second frame should be predicted");
        let (forced, keyframe) = encoder.encode_yuv(&frame, true).unwrap().unwrap();
        assert!(keyframe);
        assert!(nal_types(&forced).contains(&5));
    }

    #[test]
    fn feed_tracks_viewers_and_keyframe_requests() {
        let feed = VideoFeed::new();
        assert!(!feed.has_viewers());
        let receiver = feed.subscribe();
        assert!(feed.has_viewers());
        drop(receiver);
        assert!(!feed.has_viewers());

        assert!(!feed.take_keyframe_request());
        feed.request_keyframe();
        assert!(feed.take_keyframe_request());
        assert!(!feed.take_keyframe_request());
    }
}
