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
  triangle: {
    startRotation: "triangle.start_rotation",
    stopRotation: "triangle.stop_rotation",
    getState: "triangle.get_state",
  },
  webrtc: {
    offer: "webrtc.offer",
  },
  sdi: {
    getStatus: "sdi.get_status",
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
  /** Output slots the monitor (SDI -> WebRTC encoder) skipped because it fell behind. */
  monitor_dropped_frames: number;
  last_source_frame: number | null;
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
