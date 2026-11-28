import { FLAG_LIMITS, NATIVE_LIMITS } from './flag.ts';
import { validReveal } from './reveal.ts';
import type { ArtworkRotation, FieldDimensions, FlagReveal } from '../shared/types.ts';

export type Vector3Tuple = [number, number, number];
export type AdSpace = 'pitch' | 'cloud';
export interface AdPlane {
  origin: Vector3Tuple;
  u: Vector3Tuple;
  v: Vector3Tuple;
  width: number;
  height: number;
}
/** A flag's motion and look; its size is the plane rectangle (see flag.ts). */
export interface FlagSettings {
  /**
   * `default` (absent): SimpleWave, its centre line pinned, waving by `amplitude`,
   * `frequency` and `speed`. `native`: a cloth pinned along its top and bottom edges,
   * simulated by the renderer in the wind of `native`.
   */
  type?: 'default' | 'native';
  /** Wave coefficient in metres; the peak displacement is at most 1.53 times this. */
  amplitude: number;
  /** Primary wave cycles from the centre line to either edge. */
  frequency: number;
  speed: number;
  /** A native flag's wind and fabric, kept while the flag is `default`. Absent: `NATIVE_FLAG`. */
  native?: NativeFlagSettings;
  /** `cloth`: PBR fabric, matte with a soft sheen at grazing angles. */
  material: 'simple' | 'pbr' | 'cloth';
}
/** The reference simulation's controls (xiaoyxue/Simulation). */
export interface NativeFlagSettings {
  /** Wind speed in m/s. */
  wind: number;
  /**
   * Base direction the wind blows towards, in whole degrees: 0 along the flag to its
   * right as the artwork reads, +90 towards the side that reads it.
   */
  direction: number;
  /** Turns the wind within ±45° of `direction`, to a new target every 0.5 s. */
  autoDirection: boolean;
  /** Bending stiffness in percent, from soft and flowing to crisp and firm. */
  tension: number;
  /**
   * The part of the cloth's length its pinned top and bottom edges do not span, in
   * percent, 0 (taut) to 20. Absent (as stored before it existed): the reference's 10.
   */
  slack?: number;
  /**
   * Raises the flag from folded over `duration` seconds, from the scene load or from a
   * cue (as a reveal's `start`); absent, it hangs raised from the start.
   */
  raise?: { duration: number; start: 'load' | 'cue' };
}
export interface LocalAd {
  id: string;
  name: string;
  type: '2D' | '3D' | 'flag';
  space?: AdSpace;
  cloudId?: string;
  plane: AdPlane;
  normalSign: number;
  offset: number;
  scale: number;
  yaw: number;
  visible: boolean;
  /** The material loaded for the preview; not stored, it is read back from `materialPath`. */
  material?: File;
  /** Absolute path of the material on the onsite server, where chr-server reads it; a file chosen locally is uploaded to the studio store. */
  materialPath?: string;
  /** The material's original file name, shown to the operator. */
  materialName?: string;
  /**
   * What a render description needs from the material, noted when it was chosen so a send
   * downloads nothing: its SHA-256 and, for a 3D placement's GLB, the model's bounds and
   * alpha-blended material names. A send uses them only while the server file still has this hash.
   */
  materialSha256?: string;
  materialBounds?: { min: Vector3Tuple; max: Vector3Tuple };
  materialBlended?: string[];
  /** Present exactly for flags. */
  flag?: FlagSettings;
  /** A 2D ad's opacity, 0 to 1: the image's alpha is multiplied by this. Absent: `LOCAL_IMAGE_OPACITY`. */
  opacity?: number;
  /** A flag's clockwise turn of its artwork as it reads; the artwork stretches to the rectangle. Absent: 0. */
  artworkRotation?: ArtworkRotation;
  /** How a flag appears; absent, it is shown from the start. */
  reveal?: FlagReveal;
  /** A flag whose rectangle follows its (turned) image's aspect when resized. */
  aspectLocked?: boolean;
  /** The Fusion Tifo placement a flag was seeded from (placements/tifo.ts). */
  fusionTifoId?: string;
}

/** Opacity of a local 2D ad that has none of its own, as chr-server has drawn image quads since #163. */
export const LOCAL_IMAGE_OPACITY = 0.5;
export const imageOpacity = (ad: LocalAd) => ad.opacity ?? LOCAL_IMAGE_OPACITY;

const validPath = (path: unknown) => path === undefined || (typeof path === 'string' && path.startsWith('/'));

/** Early flag records kept their server path as `flag.artworkPath`. */
type StoredFlag = FlagSettings & { artworkPath?: string };

const within = (value: unknown, [min, max]: readonly [number, number]) => typeof value === 'number' && value >= min && value <= max;

function validNative(native: NativeFlagSettings) {
  return Boolean(native) && typeof native === 'object' && within(native.wind, NATIVE_LIMITS.wind)
    && within(native.direction, NATIVE_LIMITS.direction) && Number.isInteger(native.direction)
    && within(native.tension, NATIVE_LIMITS.tension) && typeof native.autoDirection === 'boolean'
    && (native.slack === undefined || within(native.slack, NATIVE_LIMITS.slack))
    && (native.raise === undefined || (Boolean(native.raise) && typeof native.raise === 'object'
      && Number.isFinite(native.raise.duration) && native.raise.duration > 0 && ['load', 'cue'].includes(native.raise.start)));
}

function validFlag(flag: StoredFlag) {
  return Boolean(flag) && typeof flag === 'object'
    && (['amplitude', 'frequency', 'speed'] as const).every((key) => within(flag[key], FLAG_LIMITS[key]))
    && [undefined, 'default', 'native'].includes(flag.type) && (flag.native === undefined || validNative(flag.native))
    && ['simple', 'pbr', 'cloth'].includes(flag.material) && validPath(flag.artworkPath);
}

/** A stored record in the current shape: an early flag's `artworkPath` becomes its `materialPath`. */
export function upgradeLocalAd(ad: LocalAd): LocalAd {
  if (!ad.flag || !('artworkPath' in ad.flag)) return ad;
  const { artworkPath, ...flag } = ad.flag as StoredFlag;
  return { ...ad, materialPath: ad.materialPath ?? artworkPath, flag };
}

/** Whether a stored record is a well-formed local ad; records from before flags stay valid. */
export function validLocalAd(value: unknown): value is LocalAd {
  if (!value || typeof value !== 'object') return false;
  const ad = value as LocalAd;
  const vector = (point: number[]) => Array.isArray(point) && point.length === 3 && point.every((x) => Number.isFinite(x) && Math.abs(x) <= 1e6);
  return typeof ad.id === 'string' && typeof ad.name === 'string'
    && (ad.space === 'pitch' ? ad.cloudId === undefined : (ad.space === undefined || ad.space === 'cloud') && typeof ad.cloudId === 'string')
    && ['2D', '3D', 'flag'].includes(ad.type) && (ad.type === 'flag' ? validFlag(ad.flag) : ad.flag === undefined)
    && (ad.type === 'flag' || [ad.artworkRotation, ad.reveal, ad.aspectLocked, ad.fusionTifoId].every((field) => field === undefined))
    && (ad.artworkRotation === undefined || [0, 90, 180, 270].includes(ad.artworkRotation))
    && (ad.reveal === undefined || validReveal(ad.reveal))
    && (ad.aspectLocked === undefined || typeof ad.aspectLocked === 'boolean')
    && (ad.fusionTifoId === undefined || (typeof ad.fusionTifoId === 'string' && /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(ad.fusionTifoId)))
    && (ad.opacity === undefined || (ad.type === '2D' && typeof ad.opacity === 'number' && ad.opacity >= 0 && ad.opacity <= 1))
    && typeof ad.visible === 'boolean' && Boolean(ad.plane)
    && vector(ad.plane.origin) && vector(ad.plane.u) && vector(ad.plane.v)
    && [ad.plane.u, ad.plane.v].every((axis) => Math.abs(axis.reduce((sum, x) => sum + x * x, 0) - 1) < 1e-5)
    && Math.abs(ad.plane.u.reduce((sum, x, i) => sum + x * ad.plane.v[i], 0)) < 1e-5
    && [ad.plane.width, ad.plane.height, ad.scale].every((x) => Number.isFinite(x) && x >= 0.01 && x <= 1000)
    && Number.isFinite(ad.offset) && Math.abs(ad.offset) <= 1000
    && Number.isFinite(ad.yaw) && Math.abs(ad.yaw) <= 360
    && [1, -1].includes(ad.normalSign) && (ad.material === undefined || ad.material instanceof File) && validPath(ad.materialPath)
    && (ad.materialName === undefined || (typeof ad.materialName === 'string' && ad.materialName.length <= 255))
    && (ad.materialSha256 === undefined || (ad.materialPath !== undefined && typeof ad.materialSha256 === 'string' && /^[0-9a-f]{64}$/.test(ad.materialSha256)))
    && (ad.materialBounds === undefined || (ad.type === '3D' && Boolean(ad.materialBounds) && vector(ad.materialBounds.min) && vector(ad.materialBounds.max)))
    && (ad.materialBlended === undefined || (ad.type === '3D' && Array.isArray(ad.materialBlended) && ad.materialBlended.length <= 1000
      && ad.materialBlended.every((name) => typeof name === 'string' && name.length <= 255)));
}

/** Flags are placed on point-cloud planes only; early records may still hold pitch flags. */
export function withoutPitchFlags(ads: LocalAd[]) {
  const kept = ads.filter((ad) => !(ad.space === 'pitch' && ad.type === 'flag'));
  return { kept, removed: ads.length - kept.length };
}

export function validatePitchAd(ad: LocalAd, field: FieldDimensions) {
  if (ad.type === 'flag') throw new Error('旗帜只能放在点云平面上。');
  const { origin, u, v, width, height } = ad.plane;
  if (u[2] !== 0 || v[2] !== 0 || (ad.type === '2D' && origin[2] !== 0) || ad.offset !== 0 || ad.normalSign !== -1) {
    throw new Error('球场广告的底面应平行地面；2D 广告必须贴地，3D 高度使用 Z 调整。');
  }
  for (const [a, b] of [[0, 0], [width, 0], [0, height], [width, height]]) {
    if (Math.abs(origin[0] + a * u[0] + b * v[0]) > field.length / 2 + 1e-6
      || Math.abs(origin[1] + a * u[1] + b * v[1]) > field.width / 2 + 1e-6) {
      throw new Error('广告位底面超出球场，请调整位置或缩小尺寸。');
    }
  }
}

export const MAX_LOCAL_ADS = 200;

/** A match's local placements as stored on the server: valid, unique and without loaded material files. */
export function validateStoredLocalAds(data: unknown): LocalAd[] {
  if (!Array.isArray(data) || data.length > MAX_LOCAL_ADS || !data.every((ad) => validLocalAd(ad) && ad.material === undefined)
    || new Set(data.map((ad: LocalAd) => ad.id)).size !== data.length) throw new Error(`本地广告位数据无效、重复或超过 ${MAX_LOCAL_ADS} 个。`);
  return data;
}

/** An ad as stored: the material file stays on the server, only its path and name are kept. */
export function storedLocalAd({ material, ...ad }: LocalAd): LocalAd {
  return { ...ad, ...(material ? { materialName: material.name } : {}) };
}
