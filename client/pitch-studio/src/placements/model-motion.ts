// Whole-model animation of Fusion 3D placements (#157): rotation about a fixed field axis
// and a timed path of offset and scale, both about the neutral model's bottom centre.
// Fusion's field axes (x right, y down in the top view, z down) are chr's world axes,
// so offsets in metres and rotation axes pass through unchanged; angles turn
// right-handed about the axis, like the heading about +Z. The description carries it
// as `motion` (chr_common::scene_frame::Motion) and chr-server evaluates it per frame;
// the preview evaluates the same function, so both agree at any time.
import type { FusionModelMotion, FusionModelPose, FusionMotionKey } from '../shared/types.ts';

export type Vec3 = [number, number, number];
/** `chr_common::scene_frame::Motion`. */
export interface WireMotion {
  pivot: Vec3;
  rotation?: { axis: Vec3; mode: 'swing' | 'continuous'; amplitude: number; period: number };
  path?: { start: { offset: Vec3; scale: number }; end: { offset: Vec3; scale: number }; duration: number; playback: 'once' | 'loop' | 'ping_pong' };
}

const AXES = { x: [1, 0, 0], y: [0, 1, 0], z: [0, 0, 1] } as const;
const PLAYBACK = { once: 'once', loop: 'loop', 'ping-pong': 'ping_pong' } as const;
const finite = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value);
const record = (value: unknown): value is Record<string, unknown> => typeof value === 'object' && value !== null && !Array.isArray(value);

function motionKey(value: unknown, name: string): FusionMotionKey {
  if (!record(value) || !finite(value.x) || !finite(value.y) || !finite(value.z) || !finite(value.scale) || value.scale <= 0)
    throw new Error(`Fusion model motion ${name} needs finite x, y, z (metres) and a positive scale.`);
  return { x: value.x, y: value.y, z: value.z, scale: value.scale };
}

/**
 * A Fusion `modelPose`, its animation fields kept for round trips: the static fit
 * (x, y normalized offsets, scale, yaw degrees) and, when present, rotation and motion.
 */
export function validateModelPose(pose: unknown): FusionModelPose {
  if (!record(pose) || ![pose.x, pose.y, pose.scale, pose.yaw].every(finite)) throw new Error('Invalid Fusion model pose.');
  const { x, y, scale, yaw } = pose as Record<'x' | 'y' | 'scale' | 'yaw', number>;
  if (Math.abs(x) > 1 || Math.abs(y) > 1 || scale < 0.01 || scale > 1 || yaw < 0 || yaw >= 360) throw new Error('Invalid Fusion model pose.');
  const result: FusionModelPose = { x, y, scale, yaw };
  if (pose.swingAxis !== undefined) {
    const axis = pose.swingAxis;
    if (axis !== null && axis !== 'x' && axis !== 'y' && axis !== 'z') throw new Error('Fusion model swingAxis must be null, "x", "y" or "z".');
    result.swingAxis = axis as FusionModelPose['swingAxis'];
  }
  if (pose.rotationMode !== undefined) {
    if (pose.rotationMode !== 'swing' && pose.rotationMode !== 'continuous') throw new Error('Fusion model rotationMode must be "swing" or "continuous".');
    result.rotationMode = pose.rotationMode;
  }
  if (pose.swingAmplitude !== undefined) {
    if (!finite(pose.swingAmplitude) || pose.swingAmplitude < 0 || pose.swingAmplitude > 180)
      throw new Error('Fusion model swingAmplitude must be 0 to 180 degrees.');
    result.swingAmplitude = pose.swingAmplitude;
  }
  if (pose.swingPeriod !== undefined) {
    if (!finite(pose.swingPeriod) || pose.swingPeriod < 0.1 || pose.swingPeriod > 60)
      throw new Error('Fusion model swingPeriod must be 0.1 to 60 seconds.');
    result.swingPeriod = pose.swingPeriod;
  }
  if (result.swingAxis && result.swingPeriod === undefined) throw new Error('Fusion model rotation needs a swingPeriod.');
  if (pose.motion !== undefined) {
    const motion = pose.motion;
    if (!record(motion) || typeof motion.enabled !== 'boolean') throw new Error('Fusion model motion needs enabled true or false.');
    if (!finite(motion.duration) || motion.duration <= 0) throw new Error('Fusion model motion duration must be a positive number of seconds.');
    if (motion.playback !== 'once' && motion.playback !== 'loop' && motion.playback !== 'ping-pong')
      throw new Error('Fusion model motion playback must be "once", "loop" or "ping-pong".');
    result.motion = { enabled: motion.enabled, start: motionKey(motion.start, 'start'), end: motionKey(motion.end, 'end'),
      duration: motion.duration, playback: motion.playback };
  }
  return result;
}

/** The pose's animation about `pivot` (world metres), or undefined when it stands still. */
export function wireMotion(pose: FusionModelPose, pivot: Vec3): WireMotion | undefined {
  const mode = pose.rotationMode ?? 'swing';
  const amplitude = (pose.swingAmplitude ?? 0) * Math.PI / 180;
  const rotates = Boolean(pose.swingAxis) && (mode === 'continuous' || amplitude > 0);
  const moves = pose.motion?.enabled === true;
  if (!rotates && !moves) return undefined;
  const key = (k: FusionMotionKey) => ({ offset: [k.x, k.y, k.z] as Vec3, scale: k.scale });
  const motion = pose.motion as FusionModelMotion;
  return {
    pivot,
    ...(rotates ? { rotation: { axis: [...AXES[pose.swingAxis!]] as Vec3, mode, amplitude, period: pose.swingPeriod! } } : {}),
    ...(moves ? { path: { start: key(motion.start), end: key(motion.end), duration: motion.duration, playback: PLAYBACK[motion.playback] } } : {}),
  };
}

/** Pivot offset (metres), scale multiplier and angle (radians) `seconds` after the start, as chr-server samples them. */
export function sampleMotion(motion: WireMotion, seconds: number) {
  const t = Math.max(seconds, 0);
  const r = motion.rotation;
  const angle = !r ? 0 : r.mode === 'continuous' ? 2 * Math.PI * ((t / r.period) % 1) : r.amplitude * Math.sin(2 * Math.PI * t / r.period);
  const p = motion.path;
  if (!p) return { offset: [0, 0, 0] as Vec3, scale: 1, angle };
  const d = p.duration;
  let progress: number;
  if (p.playback === 'once') progress = Math.min(t / d, 1);
  else if (p.playback === 'loop') progress = (t % d) / d;
  else { const u = (t % (2 * d)) / d; progress = u > 1 ? 2 - u : u; }
  const lerp = (a: number, b: number) => a + (b - a) * progress;
  return { offset: p.start.offset.map((v, i) => lerp(v, p.end.offset[i])) as Vec3, scale: lerp(p.start.scale, p.end.scale), angle };
}

/**
 * `T(pivot + offset) * scale * R(axis, angle) * T(-pivot)` at `seconds`, row-major; the
 * animated model is this times the neutral model.
 */
export function motionRows(motion: WireMotion, seconds: number): number[][] {
  const { offset, scale, angle } = sampleMotion(motion, seconds);
  const axis = motion.rotation?.axis ?? [0, 0, 1];
  const length = Math.hypot(...axis);
  const [x, y, z] = axis.map((v) => v / length);
  const c = Math.cos(angle), s = Math.sin(angle), k = 1 - c;
  const r = [
    [c + x * x * k, x * y * k - z * s, x * z * k + y * s],
    [y * x * k + z * s, c + y * y * k, y * z * k - x * s],
    [z * x * k - y * s, z * y * k + x * s, c + z * z * k],
  ];
  const p = motion.pivot;
  return [0, 1, 2].map((row) => {
    const linear = r[row].map((v) => v * scale);
    return [...linear, p[row] + offset[row] - linear.reduce((sum, v, i) => sum + v * p[i], 0)];
  }).concat([[0, 0, 0, 1]]);
}
