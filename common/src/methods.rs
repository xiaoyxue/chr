//! Business methods carried over JSON-RPC, with their parameter and result types.
//!
//! Method names follow `<namespace>.<command>`. The TypeScript mirror lives in
//! `client/example/render-harness/src/protocol.ts`; keep both in sync.

use serde::{Deserialize, Serialize};

/// Controls for the mock rotating triangle.
pub mod triangle {
    use super::*;

    /// Starts rotating. Params: optional [`StartRotationParams`]. Result: [`TriangleState`].
    pub const START_ROTATION: &str = "triangle.start_rotation";
    /// Stops rotating and keeps the current angle. No params. Result: [`TriangleState`].
    pub const STOP_ROTATION: &str = "triangle.stop_rotation";
    /// Reads the current state without changing it. No params. Result: [`TriangleState`].
    pub const GET_STATE: &str = "triangle.get_state";

    /// Default angular velocity, in radians per second of logical (frame) time.
    pub const DEFAULT_ANGULAR_VELOCITY_RAD_PER_S: f64 = std::f64::consts::FRAC_PI_2;

    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct StartRotationParams {
        /// Radians per second of logical time. Positive turns clockwise on screen.
        /// Defaults to [`DEFAULT_ANGULAR_VELOCITY_RAD_PER_S`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub angular_velocity_rad_per_s: Option<f64>,
    }

    /// Scene state as applied by the render loop.
    ///
    /// Commands are applied between frames. `frame` is the index of the first frame that
    /// is rendered with this state; it is logical time, independent of the wall clock.
    #[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
    pub struct TriangleState {
        pub rotating: bool,
        /// Current angle in radians, in `[0, 2π)`. Positive turns clockwise on screen.
        pub angle_rad: f64,
        pub angular_velocity_rad_per_s: f64,
        pub frame: u64,
    }
}

/// WebRTC signaling. Media itself never travels over the control transports.
pub mod webrtc {
    use super::*;

    /// Sends a browser offer and receives the server's answer for a receive-only video
    /// stream of the program output: what the SDI port transmits, encoded as H.264.
    /// Non-trickle: the answer already contains every server candidate, and the offer
    /// should contain the client's. Params: [`SessionDescription`] with `type: "offer"`.
    /// Result: [`SessionDescription`] with `type: "answer"`.
    ///
    /// The peer connection lives until the client closes it or until the control
    /// connection that created it disconnects.
    pub const OFFER: &str = "webrtc.offer";

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum SdpType {
        Offer,
        Answer,
    }

    /// Same shape as the browser's `RTCSessionDescriptionInit`.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SessionDescription {
        #[serde(rename = "type")]
        pub sdp_type: SdpType,
        pub sdp: String,
    }
}

/// The simulated SDI program output.
pub mod sdi {
    use super::*;

    /// Reads output counters. No params. Result: [`SdiStatus`].
    pub const GET_STATUS: &str = "sdi.get_status";

    /// Counters since the server started. Output slots tick at the video mode's cadence
    /// whether or not a new rendered frame is ready.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SdiStatus {
        /// For example `1280x720p30`.
        pub video_mode: String,
        /// For example `UYVY 4:2:2 8-bit, BT.709 limited range`.
        pub pixel_format: String,
        /// Output slots sent, including repeats.
        pub frames_output: u64,
        /// Slots that repeated the previous frame because no new frame was ready.
        pub repeated_frames: u64,
        /// Rendered frames discarded because the output queue was full.
        pub dropped_frames: u64,
        /// Output slots the monitor (SDI -> WebRTC encoder) skipped because it fell behind.
        pub monitor_dropped_frames: u64,
        /// Render frame index of the most recent output slot.
        pub last_source_frame: Option<u64>,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn session_description_matches_browser_shape() {
        let offer: webrtc::SessionDescription =
            serde_json::from_value(json!({"type": "offer", "sdp": "v=0"})).unwrap();
        assert_eq!(offer.sdp_type, webrtc::SdpType::Offer);
        assert_eq!(
            serde_json::to_value(webrtc::SessionDescription {
                sdp_type: webrtc::SdpType::Answer,
                sdp: "v=0".into()
            })
            .unwrap(),
            json!({"type": "answer", "sdp": "v=0"})
        );
    }

    #[test]
    fn start_rotation_params_are_optional_and_strict() {
        let empty: triangle::StartRotationParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty.angular_velocity_rad_per_s, None);
        assert!(
            serde_json::from_value::<triangle::StartRotationParams>(json!({"speed": 1})).is_err()
        );
    }
}
