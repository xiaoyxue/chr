// Waving flags (chr `flag` render instances), in chr world coordinates: metres,
// ground Z = 0, up is negative Z. A flag's plane rectangle is its flag-local frame:
// `origin` is the top-left corner, `u` runs along the width (image right), `v`
// down the height (image down) and their cross product is the flag normal, like
// the image of a 2D local ad. The artwork reads unmirrored from the side the normal
// points away from (like image x right, y down, z into the screen); the other side
// shows it mirrored, as a real flag does.
import type { AdPlane, FlagSettings, NativeFlagSettings, Vector3Tuple } from './local-ad.ts';

export type FlagRows = [number, number, number, number][];

// Amplitude reaches tifo scale: a 36 m flag waves like a 3.2 m one at 0.32 m with 3.6 m.
export const FLAG_LIMITS = { amplitude: [0, 5], frequency: [0.3, 3], speed: [0, 10] } as const;
export const DEFAULT_FLAG: FlagSettings = { amplitude: 0.32, frequency: 1.35, speed: 1, material: 'simple' };
/** Peak displacement off the plane per metre of amplitude. */
export const FLAG_PEAK = 1.53;
/** Default distance from the picked surface, enough for the default amplitude to clear it. */
export const FLAG_CLEARANCE = 0.5;
/** The wave of a flag seeded from a Fusion tifo: cloth, waving visibly at 26-36 m. */
export const TIFO_FLAG: FlagSettings = { amplitude: 1.1, frequency: 1.35, speed: 5.5, material: 'cloth' };
/** A seeded tifo's distance from its surface: enough for its wave's peak to clear it. */
export const TIFO_CLEARANCE = Math.max(FLAG_CLEARANCE, FLAG_PEAK * TIFO_FLAG.amplitude);

/** A native flag's wind and fabric as the reference simulation (xiaoyxue/Simulation) starts them. */
export const NATIVE_FLAG: NativeFlagSettings = { wind: 7, direction: 25, autoDirection: true, tension: 65 };
/** Wind in m/s, base direction in whole degrees, tension in percent: the reference simulation's controls. */
export const NATIVE_LIMITS = { wind: [0, 18], direction: [-180, 180], tension: [0, 100] } as const;
/** A native flag's pinned span over its cloth's length down the flag: 10 % slack. */
export const NATIVE_SLACK = 0.9;
/** Rows of a native flag's cloth, whose segments make its rest arc. */
const NATIVE_ROWS = 41;
export const nativeSettings = (flag: FlagSettings) => flag.native ?? NATIVE_FLAG;

/** The reference's fixed hash sequence of automatic wind offsets, within ±45°. */
function autoOffset(segment: number) {
  let value = Math.imul(segment + 1, 0x45d9f3b) ^ 0x6d2b79f5;
  value = Math.imul(value ^ (value >>> 16), 0x45d9f3b);
  value = Math.imul(value ^ (value >>> 16), 0x45d9f3b);
  value ^= value >>> 16;
  return ((value >>> 0) / 0xffffffff * 2 - 1) * 45;
}

/**
 * The direction a native flag's wind blows towards after `seconds`, in degrees in
 * [-180, 180): its base, or with `autoDirection` a smooth turn every 0.5 s to a new
 * target within ±45° of it, the same sequence every run (the reference's `windDirectionAt`).
 */
export function windDirectionAt(seconds: number, { direction, autoDirection }: Pick<NativeFlagSettings, 'direction' | 'autoDirection'>) {
  if (!autoDirection) return direction;
  const sample = seconds / 0.5, segment = Math.floor(sample), fraction = sample - segment;
  const blend = fraction * fraction * (3 - 2 * fraction);
  const from = segment === 0 ? 0 : autoOffset(segment - 1), to = autoOffset(segment);
  return ((direction + from + (to - from) * blend + 180) % 360 + 360) % 360 - 180;
}

/** Where a wind at `angle` degrees blows, relative to the flag: right or left along it, towards its front (reading side) or back. */
export function windHeading(angle: number) {
  const radians = angle * Math.PI / 180, x = Math.cos(radians), z = Math.sin(radians);
  return [Math.abs(x) > 1e-6 ? (x > 0 ? '右' : '左') : '', Math.abs(z) > 1e-6 ? (z > 0 ? '正面' : '背面') : ''].filter(Boolean).join(' + ');
}

/**
 * A native flag's cloth at rest, as the renderer starts it: pinned along its top and
 * bottom edges, its rows of rest length bowed in an arc towards the side that reads
 * the artwork (-Z). Returns the flag-local point at (`s` right, `t` down).
 */
export function nativeRest(width: number, height: number): (s: number, t: number) => Vector3Tuple {
  const segments = NATIVE_ROWS - 1, segment = height / NATIVE_SLACK / segments;
  let [low, high, turn] = [0, Math.PI, 0];
  for (let i = 0; i < 40; i++) {
    turn = (low + high) / 2;
    if (segment * Math.sin(turn / 2) / Math.sin(turn / (2 * segments)) > height) low = turn;
    else high = turn;
  }
  const radius = height / (2 * Math.sin(turn / 2));
  return (s, t) => {
    const angle = (t - 0.5) * turn;
    return [width * s, height / 2 + radius * Math.sin(angle), radius * (Math.cos(turn / 2) - Math.cos(angle))];
  };
}

const dot = (a: Vector3Tuple, b: Vector3Tuple) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const cross = (a: Vector3Tuple, b: Vector3Tuple): Vector3Tuple => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
const scaled = (a: Vector3Tuple, s: number): Vector3Tuple => [a[0] * s, a[1] * s, a[2] * s];
const plus = (a: Vector3Tuple, b: Vector3Tuple): Vector3Tuple => [a[0] + b[0], a[1] + b[1], a[2] + b[2]];

/**
 * Turns a picked plane rectangle so that its artwork reads upright and unmirrored
 * from the side it was picked from. `normalSign` is +1 when the picking camera was
 * on the `u × v` side. The result's `v` follows world down (+Z) within the plane,
 * or keeps the picked direction on a near-horizontal plane, and its normal `u × v`
 * points away from the viewer. The rectangle becomes the bounding rectangle of the
 * picked corners in the new axes, which is the same rectangle when the picked edges
 * were horizontal and vertical.
 */
export function orientFlagPlane(plane: AdPlane, normalSign: number): AdPlane {
  const away = scaled(cross(plane.u, plane.v), -normalSign);
  const down = plus([0, 0, 1], scaled(away, -away[2]));
  const length = Math.hypot(...down);
  const v = length > 0.2 ? scaled(down, 1 / length) : plane.v;
  const u = cross(v, away);
  const corners = [[0, 0], [1, 0], [0, 1], [1, 1]].map(([a, b]) => plus(scaled(plane.u, a * plane.width), scaled(plane.v, b * plane.height)));
  const along = corners.map((corner) => dot(corner, u)), across = corners.map((corner) => dot(corner, v));
  const [minU, minV] = [Math.min(...along), Math.min(...across)];
  return { origin: plus(plane.origin, plus(scaled(u, minU), scaled(v, minV))), u, v,
    width: Math.max(...along) - minU, height: Math.max(...across) - minV };
}

/**
 * The rigid flag-local to world matrix, forward row-major on column vectors: its
 * columns are `u`, `v`, `u × v` and the top-left corner moved `offset` along the
 * normal. The flag's size stays out of the matrix, so its amplitude is in metres.
 */
export function flagModel(plane: AdPlane, offset: number): FlagRows {
  const { u, v } = plane, n = cross(u, v), o = plus(plane.origin, scaled(n, offset));
  return [[u[0], v[0], n[0], o[0]], [u[1], v[1], n[1], o[1]], [u[2], v[2], n[2], o[2]], [0, 0, 0, 1]];
}

/**
 * The renderer's SimpleWave displacement of the flag point at texture coordinates
 * (`s` right, `t` down, both in [0, 1]) after `time` scaled seconds, in flag-local
 * metres. The centre line s = 0.5 stays in the plane.
 */
export function flagWave(s: number, t: number, width: number, height: number, flag: Pick<FlagSettings, 'amplitude' | 'frequency'>, time = 0): Vector3Tuple {
  const r = Math.abs(2 * s - 1), k = 2 * Math.PI * flag.frequency, amplitude = flag.amplitude * r * r;
  const a = k * r - 1.6 * time, b = 1.8 * k * r + 3 * t - 2.3 * time, c = 0.65 * k * r - 4 * t - 0.9 * time;
  return [width * s, height * t + amplitude * (0.14 - 0.08 * Math.sin(b)),
    amplitude * (Math.sin(a) + 0.35 * Math.sin(b) + 0.18 * Math.cos(c))];
}

/** A flag's motion fields as typed in the editor, before they are saved. */
export interface FlagDraft {
  type: FlagType;
  amplitude: string; frequency: string; speed: string;
  wind: string; direction: string; autoDirection: boolean; tension: string;
  material: FlagSettings['material'];
}
export type FlagType = NonNullable<FlagSettings['type']>;
const WAVE_FIELDS = [['amplitude', '振幅'], ['frequency', '频率'], ['speed', '速度']] as const;
const NATIVE_FIELDS = [['wind', '风速'], ['direction', '风向'], ['tension', '布料张力']] as const;

export const flagDraft = (flag: FlagSettings): FlagDraft => {
  const { amplitude, frequency, speed, material } = flag, native = nativeSettings(flag);
  return { type: flag.type ?? 'default', amplitude: String(amplitude), frequency: String(frequency), speed: String(speed),
    wind: String(native.wind), direction: String(native.direction), autoDirection: native.autoDirection, tension: String(native.tension), material };
};

/** Whether the typed fields of the draft's type differ from the saved flag; a cleared field counts as a change. */
export function flagDraftChanged(draft: FlagDraft, flag: FlagSettings) {
  if (draft.type !== (flag.type ?? 'default') || draft.material !== flag.material) return true;
  if (draft.type === 'default') return WAVE_FIELDS.some(([key]) => draft[key].trim() === '' || Number(draft[key]) !== flag[key]);
  const native = nativeSettings(flag);
  return draft.autoDirection !== native.autoDirection || NATIVE_FIELDS.some(([key]) => draft[key].trim() === '' || Number(draft[key]) !== native[key]);
}

/**
 * The typed settings of the draft's type, or the first field outside its range. The
 * other type's saved settings are kept, so switching back restores them.
 */
export function parseFlagDraft(draft: FlagDraft): { wave: Partial<FlagSettings> } | { error: string } {
  const number = (text: string) => text.trim() === '' ? Number.NaN : Number(text);
  if (draft.type === 'native') {
    const native = { wind: 0, direction: 0, autoDirection: draft.autoDirection, tension: 0 };
    for (const [key, label] of NATIVE_FIELDS) {
      const value = number(draft[key]), [min, max] = NATIVE_LIMITS[key];
      if (!(value >= min && value <= max) || (key === 'direction' && !Number.isInteger(value)))
        return { error: `${label}应介于 ${min} – ${max}${key === 'direction' ? ' 之间的整数度' : ''}。` };
      native[key] = value;
    }
    return { wave: { type: 'native', native, material: draft.material } };
  }
  const wave = { type: 'default' as const, amplitude: 0, frequency: 0, speed: 0, material: draft.material };
  for (const [key, label] of WAVE_FIELDS) {
    const value = number(draft[key]), [min, max] = FLAG_LIMITS[key];
    if (!(value >= min && value <= max)) return { error: `${label}应介于 ${min} – ${max}。` };
    wave[key] = value;
  }
  return { wave };
}
