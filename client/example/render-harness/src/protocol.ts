// TypeScript mirror of the shared protocol in `common/src/` (Rust, the source of truth):
// `jsonrpc.rs` for the envelope and `methods.rs` for business methods. Keep both in sync.
// Approved synthetic scene/frame payloads are separate from the existing runtime methods.
export * as SceneFrame from "./scene-frame.ts";

export const JSONRPC_VERSION = "2.0";

export const ErrorCode = {
  ParseError: -32700,
  InvalidRequest: -32600,
  MethodNotFound: -32601,
  InvalidParams: -32602,
  InternalError: -32603,
  ServerBusy: -32000,
  ServerUnavailable: -32001,
} as const;

export type Id = string | number | null;

export interface RpcRequest {
  jsonrpc: typeof JSONRPC_VERSION;
  method: string;
  params?: unknown;
  /** Absent for notifications, which never get a response. */
  id?: Id;
}

export interface RpcErrorObject {
  code: number;
  message: string;
  data?: unknown;
}

export type RpcResponse =
  | { jsonrpc: typeof JSONRPC_VERSION; id: Id; result: unknown }
  | { jsonrpc: typeof JSONRPC_VERSION; id: Id; error: RpcErrorObject };

export const Methods = {
  mesh: {
    startRotation: "mesh.start_rotation",
    stopRotation: "mesh.stop_rotation",
    getRotation: "mesh.get_rotation",
  },
  triangle: {
    startRotation: "triangle.start_rotation",
    stopRotation: "triangle.stop_rotation",
    getState: "triangle.get_state",
  },
  /** Served by chr-monitor on its own WebSocket, not by chr-server. */
  webrtc: {
    offer: "webrtc.offer",
  },
  sdi: {
    getStatus: "sdi.get_status",
  },
  renderWorker: {
    getStatus: "render.get_worker_status",
  },
  /** Answered only by a tracked program (`chr-server --tracked-scene` or `--description`). */
  tracking: {
    getStatus: "tracking.get_status",
  },
  /** Served by chr-monitor. */
  monitor: {
    getStatus: "monitor.get_status",
  },
} as const;

export interface StartRotationParams {
  /** Radians per second of logical time; positive turns clockwise on screen. */
  angular_velocity_rad_per_s?: number;
}

/** Scene state as applied by the render loop. `frame` is logical time. */
export interface TriangleState {
  rotating: boolean;
  /** In [0, 2π); positive turns clockwise on screen. */
  angle_rad: number;
  angular_velocity_rad_per_s: number;
  /** Index of the first frame rendered with this state. */
  frame: number;
}

export interface MeshRotationTarget {
  runtime_id: string;
  scene_id: string;
  instance_id: string;
}

export interface MeshStartRotation extends MeshRotationTarget {
  /** Model-local XY rotation, radians per second of SDI output-slot time. */
  angular_velocity_rad_per_s: number;
}

export interface MeshRotationState extends MeshRotationTarget, TriangleState {}

/** Same shape as the browser's RTCSessionDescriptionInit. */
export interface SessionDescription {
  type: "offer" | "answer";
  sdp: string;
}

/** Simulated SDI output counters since the server started. */
export interface SdiStatus {
  video_mode: string;
  pixel_format: string;
  /** Output slots sent, including repeats. */
  frames_output: number;
  /** Slots that repeated the previous frame because no new frame was ready. */
  repeated_frames: number;
  /** Rendered frames discarded because the output queue was full. */
  dropped_frames: number;
  last_source_frame: number | null;
}

/** The render worker process supervised by chr-server. */
export interface WorkerStatus {
  state: "starting" | "ready" | "backoff";
  /** Process ID of the current worker, if one is running. */
  pid: number | null;
  /** Workers started after the first. */
  restarts: number;
  /** Requests the worker did not finish within the render deadline. */
  deadline_misses: number;
  /** Frames transmitted as their input image without CG. */
  clean_feed_frames: number;
  /** Frames without CG whose input could not be converted, so the port repeated. */
  repeated_for_unavailable_clean_feed: number;
  last_error: string | null;
}

/** Tracking join counters since the server started. */
export interface TrackingStatus {
  /** `tracking.frame` calls accepted into the backlog. */
  samples_received: number;
  /** Calls rejected because their frame was already rendered or skipped. */
  samples_late: number;
  /** Input frames submitted for rendering with their camera. */
  frames_joined: number;
  /** Input frames whose camera did not arrive within the wait; not rendered. */
  frames_without_tracking: number;
  /** Input frames not rendered because the backlog was full or a newer frame resolved first. */
  frames_dropped: number;
  /** Captured frames the input ring overwrote before the server read them. */
  frames_overwritten: number;
}

/** chr-monitor counters since the monitor started. */
export interface MonitorStatus {
  /** Transmitted slots encoded for viewers. */
  frames_encoded: number;
  /** Transmitted slots not encoded while someone watched, because a newer slot was read first. */
  frames_skipped: number;
  /** Active WebRTC previews. */
  viewers: number;
  /** Connected SRT callers. */
  srt_callers: number;
}

export function isTriangleState(value: unknown): value is TriangleState {
  if (typeof value !== "object" || value === null) return false;
  const state = value as Record<string, unknown>;
  return (
    typeof state.rotating === "boolean" &&
    typeof state.angle_rad === "number" &&
    typeof state.angular_velocity_rad_per_s === "number" &&
    typeof state.frame === "number"
  );
}
