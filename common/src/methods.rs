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

/// Server-owned instance animation in the opt-in software-video simulation.
pub mod mesh {
    use super::*;
    use crate::scene_frame::Id;

    pub const START_ROTATION: &str = "mesh.start_rotation";
    pub const STOP_ROTATION: &str = "mesh.stop_rotation";
    pub const GET_ROTATION: &str = "mesh.get_rotation";
    pub const METHODS: &[&str] = &[START_ROTATION, STOP_ROTATION, GET_ROTATION];

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct RotationTarget {
        pub runtime_id: Id,
        pub scene_id: Id,
        pub instance_id: Id,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct StartRotation {
        pub runtime_id: Id,
        pub scene_id: Id,
        pub instance_id: Id,
        /// Local XY rotation in radians per second of SDI output-slot time.
        pub angular_velocity_rad_per_s: f64,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct RotationState {
        pub runtime_id: Id,
        pub scene_id: Id,
        pub instance_id: Id,
        pub rotating: bool,
        /// Accumulated local Z angle since the model was first selected.
        pub angle_rad: f64,
        pub angular_velocity_rad_per_s: f64,
        /// SDI output-slot count of the last applied pose.
        pub frame: u64,
    }
}

/// Per-frame camera of a tracked program (`--tracked-scene` or `--description`),
/// sent by an external tracking source such as `chr-sdi-in --send-cameras` when
/// chr-server runs without `--tracking-worker`. Only `chr-server` running a
/// tracked program answers these methods. See `server/docs/sdi-simulation.md`.
pub mod tracking {
    use super::*;
    use crate::scene_frame::{Matrix3, RadialLens, U64, Vec3};

    /// Delivers the camera of one input frame. Params: [`TrackingFrame`]. Result: `null`.
    /// Rejected when the frame was already rendered or skipped, when it is a duplicate,
    /// or when the bounded backlog is full.
    pub const FRAME: &str = "tracking.frame";
    /// Reads join counters. No params. Result: [`TrackingStatus`].
    pub const GET_STATUS: &str = "tracking.get_status";

    /// Camera of the input frame whose `frame_id` in the input's current
    /// stream is `sequence`: the number the capture process (`chr-sdi-in`)
    /// stamps on the frame (VchaoFrame `frame_id`, from 0 in each stream).
    /// The runtime pairs both by this number only, never by arrival order.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct TrackingFrame {
        pub sequence: U64,
        /// Forward row-major ground-to-SNC matrix: of the ideal (undistorted)
        /// image with a `lens`, of the frame itself without one.
        pub ti: Matrix3,
        /// Optical centre in world metres; negative Z is up.
        pub camera_center: Vec3,
        /// Row-major reference-native-pixel to frame-SNC warp applied after the
        /// lens's radial term; `null` means `W = S`. Requires a `lens`.
        pub frame_warp: Option<Matrix3>,
        /// The frame's lens. Absent: the server's `--tracking-lens-*` lens, or
        /// none (pinhole). See `server/docs/tracking-worker.md#lens`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub lens: Option<RadialLens>,
    }

    /// The camera model of a frame rendered with its camera.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum CameraModel {
        /// `S^-1 Ti A(C)`, no lens.
        #[serde(rename = "pinhole")]
        Pinhole,
        /// A `radial-poly-native-px-v1` lens without a frame warp.
        #[serde(rename = "radial")]
        Radial,
        /// A `radial-poly-native-px-v1` lens and the frame's own warp.
        #[serde(rename = "radial+warp")]
        RadialWarp,
    }

    /// Where the lens of a frame came from.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub enum LensSource {
        /// The tracking data: the tracking worker's frames or `tracking.frame`.
        Worker,
        /// chr-server's `--tracking-lens-k/-centre/-scale` (or `stack.toml`).
        Cli,
        /// No lens anywhere: pinhole.
        None,
        /// `--tracking-lens pinhole`: every lens source is ignored.
        ForcedPinhole,
    }

    /// The camera model of the last frame rendered with its camera.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ActiveCameraModel {
        pub model: CameraModel,
        pub source: LensSource,
    }

    /// Counters since the server started.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct TrackingStatus {
        /// `tracking.frame` calls accepted into the backlog.
        pub samples_received: u64,
        /// Calls rejected because their frame was already rendered or skipped.
        pub samples_late: u64,
        /// Input frames submitted for rendering with their camera.
        pub frames_joined: u64,
        /// Input frames whose camera did not arrive within the wait, including
        /// frames a tracking worker returned `unavailable` or never received.
        pub frames_without_tracking: u64,
        /// Input frames transmitted as clean feed: their own pixels without CG,
        /// because they had no camera. They keep the output's cadence and delay.
        #[serde(default)]
        pub frames_clean_feed: u64,
        /// Frames a tracking worker returned with their own estimate (`fresh`).
        /// The four `poses_*` counters cover every returned frame of every
        /// worker session; the first three come with a camera.
        #[serde(default)]
        pub poses_fresh: u64,
        /// Frames returned with a pose interpolated between two estimates.
        #[serde(default)]
        pub poses_interpolated: u64,
        /// Frames returned with an earlier frame's estimate (`reused`).
        #[serde(default)]
        pub poses_reused: u64,
        /// Frames a tracking worker returned without a camera (`unavailable`).
        #[serde(default)]
        pub poses_unavailable: u64,
        /// Frames a tracking worker released by its overload rule (`overdue`),
        /// whatever their pose.
        #[serde(default)]
        pub poses_overdue: u64,
        /// Camera model and lens source of the last frame rendered with its
        /// camera; `null` before the first.
        #[serde(default)]
        pub camera_model: Option<ActiveCameraModel>,
    }

    /// Reads the tracking worker's supervision counters. No params. Result:
    /// [`TrackerStatus`]. Only a server started with `--tracking-worker` answers it;
    /// its cameras then come from that worker, and `tracking.frame` is rejected.
    pub const GET_WORKER_STATUS: &str = "tracking.get_worker_status";

    /// Counters since the server started. Whether a frame was rendered with its
    /// camera is counted by [`TrackingStatus`].
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct TrackerStatus {
        pub state: super::render_worker::WorkerState,
        /// The worker executable's file name, for example `chr-tracking-sim-cli`
        /// (the replay stand-in) or `pitch-analysis`.
        #[serde(default)]
        pub worker: String,
        /// The VchaoFrame `stream_id` of the current worker session; every
        /// started worker gets a new one.
        #[serde(default)]
        pub stream_id: Option<String>,
        /// Process ID of the current worker, if one is running.
        pub pid: Option<u32>,
        /// Workers started after the first.
        pub restarts: u64,
        /// Frames sent to the worker.
        pub requests: u64,
        /// Frames the worker returned with a camera (`fresh`, `interpolated` or `reused`).
        pub cameras: u64,
        /// Frames the worker returned as `unavailable`.
        pub unavailable: u64,
        /// Frames not sent because no worker was ready, or because the worker held
        /// as many frames as chr-server allows in flight (it is then replaced).
        pub not_sent: u64,
        pub last_error: Option<String>,
    }
}

/// WebRTC signaling, served by `chr-monitor` on its own WebSocket, not by
/// `chr-server`. Media itself never travels over the JSON-RPC transports.
pub mod webrtc {
    use super::*;

    /// Sends a browser offer and receives the monitor's answer for a receive-only video
    /// stream of the program output: what the SDI port transmits, encoded as H.264.
    /// Non-trickle: the answer already contains every monitor candidate, and the offer
    /// should contain the client's. Params: [`SessionDescription`] with `type: "offer"`.
    /// Result: [`SessionDescription`] with `type: "answer"`.
    ///
    /// The peer connection lives until the client closes it or until the connection
    /// that created it disconnects.
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
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct SdiStatus {
        /// For example `1280x720p30` or `1920x1080p59.94`.
        pub video_mode: String,
        /// The output's frame rate now: an integer or a string such as
        /// `"60000/1001"`.
        pub rate: crate::rate::Rate,
        /// For example `UYVY 4:2:2 8-bit, BT.709 limited range`.
        pub pixel_format: String,
        /// Output slots sent, including repeats.
        pub frames_output: u64,
        /// Slots that repeated the previous frame because no new frame was ready.
        pub repeated_frames: u64,
        /// Rendered frames discarded because the output queue was full.
        pub dropped_frames: u64,
        /// Render frame index of the most recent output slot.
        pub last_source_frame: Option<u64>,
        /// Input format (rate or extent) changes followed at runtime.
        #[serde(default)]
        pub format_changes: u64,
        /// Slots sent as clean feed while a format change was in progress: from
        /// the first frame in the new format to its first CG frame.
        #[serde(default)]
        pub transition_clean_slots: u64,
        /// Slots filled (hold, black or slate) because the input delivered no
        /// frames (`--input-loss`).
        #[serde(default)]
        pub input_loss_slots: u64,
        /// Times the input stopped delivering frames.
        #[serde(default)]
        pub input_losses: u64,
        /// Timecode of the most recent output slot's frame, `hh:mm:ss:ff`
        /// (`hh:mm:ss;ff` drop-frame), the input's timecode output with the
        /// same delay as its frame.
        #[serde(default)]
        pub last_timecode: Option<String>,
        /// Output slots whose frame carried captions (CEA-608/708).
        #[serde(default)]
        pub caption_frames: u64,
        /// The embedded audio passed through with the video.
        #[serde(default)]
        pub audio: Option<AudioStatus>,
        /// The GStreamer core's program: its delay, what each slot got and how
        /// much time the CG had (#131). `null` for a generated program (the
        /// triangle, `--synthetic-program`, `--pitch-demo`, `--software-video`),
        /// which has no capture input.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub core: Option<CoreStatus>,
    }

    /// Telemetry of the GStreamer core (a tracked or description program).
    /// Repeats, input-loss and format-change slots are [`SdiStatus`]'s.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct CoreStatus {
        /// The program delay D: capture to output of every frame, ms.
        pub program_delay_ms: f64,
        /// How long before it is due each slot is decided, ms.
        pub decision_margin_ms: f64,
        /// Slots decided since start with their rendered frame (CG).
        pub cg_slots: u64,
        /// Slots decided with their clean feed: no CG by the decision.
        pub clean_slots: u64,
        /// Slots with neither frame at their decision; the output repeats.
        pub empty_slots: u64,
        /// Rendered frames that arrived after their slot was decided.
        pub late_cg: u64,
        /// Slots decided after they were due, which the output skips.
        pub late_decisions: u64,
        /// `cg_slots / (cg_slots + clean_slots)`; `null` before the first slot.
        pub cg_ratio: Option<f64>,
        /// The last complete report window; `null` during the first.
        pub window: Option<CoreWindow>,
    }

    /// The core's program over one report window (10 s).
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct CoreWindow {
        pub seconds: f64,
        pub cg_slots: u64,
        pub clean_slots: u64,
        pub cg_ratio: Option<f64>,
        /// Per-slot slack of the CG slots: how long before its slot was
        /// decided each rendered frame arrived, ms.
        pub slack_ms: Option<Spread>,
        /// Render budget at submit: the time left until each rendered frame's
        /// slot is decided when it went to the render worker, ms.
        pub render_budget_ms: Option<Spread>,
        /// Capture to output of the slots, measured at the output, ms.
        pub delay_ms: Option<Spread>,
    }

    /// Minimum, mean and maximum of `count` values.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Spread {
        pub min: f64,
        pub mean: f64,
        pub max: f64,
        pub count: u64,
    }

    /// Audio output of the GStreamer core: the input's audio delayed by the
    /// same program delay as its video.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct AudioStatus {
        /// Samples per second.
        pub sample_rate: u32,
        pub channels: u32,
        /// Sample frames output so far.
        pub samples_output: u64,
        /// Mean capture-to-output delay of the audio over the last report, µs.
        pub delay_us: i64,
        /// Lip sync: the audio's mean delay minus the video's over the last
        /// report, µs; positive when the audio is late.
        pub offset_us: i64,
    }
}

/// The out-of-process render worker that `chr-server` supervises.
pub mod render_worker {
    use super::*;

    /// Reads worker supervision counters. No params. Result: [`WorkerStatus`].
    pub const GET_STATUS: &str = "render.get_worker_status";

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum WorkerState {
        /// Started, waiting for the worker to report ready.
        Starting,
        Ready,
        /// No worker; the next one starts when the backoff ends.
        Backoff,
    }

    /// Counters since the server started.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct WorkerStatus {
        pub state: WorkerState,
        /// Process ID of the current worker, if one is running.
        pub pid: Option<u32>,
        /// Workers started after the first.
        pub restarts: u64,
        /// Requests the worker did not finish within the render deadline.
        pub deadline_misses: u64,
        /// Frames transmitted as their input image without CG.
        pub clean_feed_frames: u64,
        /// Frames without CG whose input could not be converted to the output, so
        /// the port repeated its previous frame.
        pub repeated_for_unavailable_clean_feed: u64,
        pub last_error: Option<String>,
    }
}

/// The `chr-monitor` process, which encodes the program tap for viewers.
pub mod monitor {
    use super::*;

    /// Reads monitor counters. No params. Result: [`MonitorStatus`].
    pub const GET_STATUS: &str = "monitor.get_status";

    /// Counters since the monitor started.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct MonitorStatus {
        /// Transmitted slots encoded for viewers.
        pub frames_encoded: u64,
        /// Transmitted slots that were not encoded while someone was watching,
        /// because the monitor read a newer slot first.
        pub frames_skipped: u64,
        /// Active WebRTC previews.
        pub viewers: u64,
        /// Connected SRT callers.
        pub srt_callers: u64,
        /// H.264 encoder of the preview: `nvh264enc` (NVENC), `x264enc` or `openh264`.
        pub encoder: String,
        /// Preview size: the program output scaled down to fit 1920x1080, keeping its
        /// aspect ratio; WebRTC and SRT carry the same stream.
        pub preview_width: u32,
        pub preview_height: u32,
        /// Changes of the program tap's mode (rate or extent) that the monitor
        /// followed by restarting its encoder.
        #[serde(default)]
        pub mode_changes: u64,
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
