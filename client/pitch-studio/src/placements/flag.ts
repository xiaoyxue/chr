// Waving flags (chr `flag` render instances), in chr world coordinates: metres,
// ground Z = 0, up is negative Z. A flag's plane rectangle is its flag-local frame:
// `origin` is the top-left corner, `u` runs along the width (image right), `v`
// down the height (image down) and their cross product is the flag normal, like
// the image of a 2D local ad. The artwork reads unmirrored from the side the normal
// points away from (like image x right, y down, z into the screen); the other side
// shows it mirrored, as a real flag does.
import type { AdPlane, FlagSettings, Vector3Tuple } from './local-ad.ts';

export type FlagRows = [number, number, number, number][];

export const FLAG_LIMITS = { amplitude: [0, 0.7], frequency: [0.3, 3], speed: [0, 10] } as const;
export const DEFAULT_FLAG: FlagSettings = { amplitude: 0.32, frequency: 1.35, speed: 1, material: 'simple' };
/** Peak displacement off the plane per metre of amplitude. */
export const FLAG_PEAK = 1.53;
/** Default distance from the picked surface, enough for the default amplitude to clear it. */
export const FLAG_CLEARANCE = 0.5;

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

/** A flag's wave fields as typed in the editor, before they are saved. */
export interface FlagDraft { amplitude: string; frequency: string; speed: string; material: FlagSettings['material'] }
const WAVE_FIELDS = [['amplitude', '振幅'], ['frequency', '频率'], ['speed', '速度']] as const;

export const flagDraft = ({ amplitude, frequency, speed, material }: FlagSettings): FlagDraft =>
  ({ amplitude: String(amplitude), frequency: String(frequency), speed: String(speed), material });

/** Whether the typed fields differ from the saved flag; a cleared field counts as a change. */
export function flagDraftChanged(draft: FlagDraft, flag: FlagSettings) {
  return draft.material !== flag.material || WAVE_FIELDS.some(([key]) => draft[key].trim() === '' || Number(draft[key]) !== flag[key]);
}

/** The typed wave settings, or the first field outside its range. */
export function parseFlagDraft(draft: FlagDraft): { wave: Pick<FlagSettings, 'amplitude' | 'frequency' | 'speed' | 'material'> } | { error: string } {
  const wave = { amplitude: 0, frequency: 0, speed: 0, material: draft.material };
  for (const [key, label] of WAVE_FIELDS) {
    const value = Number(draft[key]), [min, max] = FLAG_LIMITS[key];
    if (draft[key].trim() === '' || !Number.isFinite(value) || value < min || value > max) return { error: `${label}应介于 ${min} – ${max}。` };
    wave[key] = value;
  }
  return { wave };
}
