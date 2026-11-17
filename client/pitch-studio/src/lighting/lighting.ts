// Lighting presets of the studio: what the operator edits in 广告位预览 › 场景灯光 and what a
// render description's `scene.lighting` becomes (server/docs/render-description.md#lighting).
// Positions are chr world metres: X along the length, Y across (down in the top view),
// heights above the ground (chr Z is negative up). Colours are linear RGB multipliers
// whose largest channel is 1; a light's colour times its intensity is what chr receives.
import type { FieldDimensions } from '../shared/types.ts';

export type Rgb = [number, number, number];
interface LightBase { id: string; name: string; enabled: boolean; color: Rgb; shadows: boolean }
/** A distant light such as the sun: where it comes from, and its irradiance in W/m². */
export interface DirectionalLight extends LightBase {
  kind: 'directional';
  /** Degrees in the top view from +X towards +Y of the side the light comes from. */
  azimuth: number;
  /** Degrees above the horizon. */
  elevation: number;
  irradiance: number;
}
/** A floodlight at `position` (x, y, height) aimed at the ground point `target`; intensity in W/sr. */
export interface SpotLight extends LightBase {
  kind: 'spot';
  position: Rgb;
  target: [number, number];
  intensity: number;
  /** Half angles in degrees: full intensity inside `inner`, none beyond `outer`. */
  inner: number;
  outer: number;
  /** Shadow-source radius in metres. */
  radius: number;
}
export interface PointLight extends LightBase { kind: 'point'; position: Rgb; intensity: number; radius: number }
export type StudioLight = DirectionalLight | SpotLight | PointLight;
export interface LightingPreset {
  id: string;
  name: string;
  lights: StudioLight[];
  /**
   * Image-based lighting from an OpenEXR panorama on the onsite server; gain scales it, rotation
   * turns it (degrees). `maxRadiance` clamps brighter panorama texels before gain, to drop a sun the
   * directional light already carries (#177); absent or null keeps the panorama whole.
   */
  environment: { enabled: boolean; path: string | null; gain: number; rotation: number; maxRadiance?: number | null };
  /** Uniform fill radiance from every direction, W/(m² sr). */
  constant: { enabled: boolean; intensity: number; color: Rgb };
  /** Set on built-ins as offered, never stored; `edited` when the library holds the operator's version. */
  builtIn?: true;
  edited?: true;
}
/**
 * The global lighting document: the operator's presets, their edits of built-ins (under
 * the built-in's ID) and each match's choice.
 */
export interface LightingLibrary { version: 1; presets: LightingPreset[]; selected: Record<string, string> }
/** `scene_frame::Lighting` as chr-server reads it. */
export interface WireLighting {
  lights: Record<string, unknown>[];
  environment: { asset_ref: string; gain: number; rotation: number; visible: boolean; max_radiance?: number } | null;
  constant?: Rgb;
}

/** chr-server draws at most this many lights in a description. */
export const MAX_LIGHTS = 8;
export const DEFAULT_PRESET = 'builtin-default';
export const ORLANDO_PRESET = 'builtin-orlando';
export const ORLANDO_EXR = 'orlando_stadium_4k.exr';
/** The panorama the default preset lights with, and the first choice when IBL is switched on. */
export const DEFAULT_EXR = 'fly-studio-03_4K.exr';
/**
 * The cap the Orlando preset sets and switching the cap on starts from: its sun and floodlights
 * hold over half of the panorama's energy above it, a studio panorama's softboxes about 1 %.
 */
export const DEFAULT_MAX_RADIANCE = 50;
const BUILT_IN_IDS = [DEFAULT_PRESET, ORLANDO_PRESET];
const ID = /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/;
const MATCH_ID = /^[A-Za-z0-9][A-Za-z0-9_-]{0,79}$/;

const radians = (degrees: number) => degrees * Math.PI / 180;
const degrees = (radians: number) => radians * 180 / Math.PI;
const unit = (rgb: Rgb): Rgb => { const max = Math.max(...rgb); return rgb.map((v) => v / max) as Rgb; };

/** The tower spots and the sun of chr-server's default rig for `field` (server/runtime/src/program/description.rs). */
function towerRig({ length, width }: FieldDimensions): StudioLight[] {
  const spots = ([[-1, -1], [-1, 1], [1, -1], [1, 1]] as const).map(([sx, sy], index): SpotLight => ({
    kind: 'spot', id: `tower-${index}`, name: `灯塔 ${index + 1}`, enabled: true, color: unit([16000, 16800, 20000]), shadows: true,
    position: [sx * length / 2, sy * width / 2, 35], target: [0, 0], intensity: 20000,
    inner: degrees(0.4), outer: degrees(0.9), radius: 0.5,
  }));
  // The default sun travels along [0.5, 0.6, 1.0], so it comes from the -X -Y side.
  const sun: DirectionalLight = { kind: 'directional', id: 'sun', name: '太阳', enabled: true, color: unit([2.5, 2.35, 2.1]),
    shadows: true, azimuth: degrees(Math.atan2(-0.6, -0.5)) + 360, elevation: degrees(Math.atan2(1, Math.hypot(0.5, 0.6))),
    irradiance: 2.5 };
  return [...spots, sun];
}

/**
 * The built-in presets: chr-server's default rig lit by the fly-studio panorama, and the
 * Orlando stadium panorama, its sun capped off, with a fill next to the same rig.
 */
export function builtInPresets(field: FieldDimensions, iblDir: string): LightingPreset[] {
  const dir = iblDir.replace(/\/+$/, '');
  return [
    { id: DEFAULT_PRESET, name: '默认：四角灯塔 + 太阳 + fly-studio IBL', builtIn: true, lights: towerRig(field),
      environment: { enabled: true, path: `${dir}/${DEFAULT_EXR}`, gain: 0.5, rotation: 0 }, constant: { enabled: false, intensity: 0.1, color: [1, 1, 1] } },
    { id: ORLANDO_PRESET, name: 'Orlando 球场 IBL + 灯塔 + 太阳 + 常量光', builtIn: true, lights: towerRig(field),
      environment: { enabled: true, path: `${dir}/${ORLANDO_EXR}`, gain: 0.5, rotation: 0, maxRadiance: DEFAULT_MAX_RADIANCE },
      constant: { enabled: true, intensity: 0.1, color: [1, 1, 1] } },
  ];
}

/** Every preset on offer: the built-ins, each replaced by the operator's edit if any, then the operator's own. */
export function availablePresets(library: LightingLibrary, builtIns: LightingPreset[]): LightingPreset[] {
  const edits = new Map(library.presets.map((preset) => [preset.id, preset]));
  return [
    ...builtIns.map((preset): LightingPreset => edits.has(preset.id) ? { ...edits.get(preset.id), builtIn: true, edited: true } : preset),
    ...library.presets.filter((preset) => !BUILT_IN_IDS.includes(preset.id)),
  ];
}

/** The preset `matchId` uses: its choice if it still exists, otherwise the default rig. */
export function selectedPreset(library: LightingLibrary, builtIns: LightingPreset[], matchId: string) {
  const all = availablePresets(library, builtIns);
  return all.find((preset) => preset.id === library.selected[matchId]) ?? all.find((preset) => preset.id === DEFAULT_PRESET);
}

/** Where a directional light travels: away from the side it comes from, downwards (+Z). */
export function travelDirection(light: DirectionalLight): Rgb {
  const a = radians(light.azimuth), e = radians(light.elevation);
  return [-Math.cos(e) * Math.cos(a), -Math.cos(e) * Math.sin(a), Math.sin(e)];
}

const scaled = (color: Rgb, value: number) => color.map((c) => c * value) as Rgb;

/** The preset as `scene.lighting`; `environment` is the asset ID of its panorama, when it has one. */
export function describeLighting(preset: LightingPreset, environment?: string): WireLighting {
  const lights = preset.lights.filter((light) => light.enabled).map((light) => {
    const common = { light_id: `${light.kind}-${light.id}`, shadow_mode: light.shadows ? null : 'off' };
    if (light.kind === 'directional')
      return { ...common, kind: 'directional', direction: travelDirection(light), irradiance: scaled(light.color, light.irradiance), angular_radius: 0.00465 };
    const position = [light.position[0], light.position[1], -light.position[2]];
    if (light.kind === 'point')
      return { ...common, kind: 'point', position, intensity: scaled(light.color, light.intensity), source_radius: light.radius };
    return { ...common, kind: 'spot', position, intensity: scaled(light.color, light.intensity), source_radius: light.radius,
      direction: [light.target[0] - light.position[0], light.target[1] - light.position[1], light.position[2]],
      inner_angle: radians(light.inner), outer_angle: radians(light.outer) };
  });
  if (lights.length > MAX_LIGHTS) throw new Error(`chr-server 最多绘制 ${MAX_LIGHTS} 盏灯，当前预设启用了 ${lights.length} 盏；请停用部分灯光。`);
  const sky = environmentPath(preset);
  if (sky && !environment) throw new Error('环境贴图尚未登记为渲染素材。');
  const fill = preset.constant.enabled ? scaled(preset.constant.color, preset.constant.intensity) : [0, 0, 0];
  return {
    lights,
    environment: sky ? { asset_ref: environment, gain: preset.environment.gain, rotation: radians(preset.environment.rotation), visible: false,
      ...(preset.environment.maxRadiance ? { max_radiance: preset.environment.maxRadiance } : {}) } : null,
    ...(fill.some((v) => v > 0) ? { constant: fill as Rgb } : {}),
  };
}

/** The panorama a preset lights with, when its IBL is enabled. */
export function environmentPath(preset: LightingPreset) {
  return preset.environment.enabled ? preset.environment.path : null;
}

const finite = (value: unknown, min: number, max: number): value is number => typeof value === 'number' && Number.isFinite(value) && value >= min && value <= max;
const rgb = (value: unknown): value is Rgb => Array.isArray(value) && value.length === 3 && value.every((v) => finite(v, 0, 1))
  && Math.max(...value) > 0;
const point = (value: unknown, size: number, bound: number) => Array.isArray(value) && value.length === size && value.every((v) => finite(v, -bound, bound));

function validLight(light: StudioLight) {
  if (!light || typeof light !== 'object' || !ID.test(light.id) || typeof light.name !== 'string' || light.name.length > 80
    || typeof light.enabled !== 'boolean' || typeof light.shadows !== 'boolean' || !rgb(light.color)) return false;
  if (light.kind === 'directional') return finite(light.azimuth, -360, 360) && finite(light.elevation, 0, 90) && finite(light.irradiance, 0, 1e4);
  if (!point(light.position, 3, 1e4) || !finite(light.position[2], 0, 1000) || !finite(light.intensity, 0, 1e9) || !finite(light.radius, 0, 100)) return false;
  if (light.kind === 'point') return true;
  // A spot needs a direction: a target other than its foot point, or some height.
  return light.kind === 'spot' && point(light.target, 2, 1e4) && finite(light.inner, 0, 89) && finite(light.outer, 0, 89.9) && light.inner < light.outer
    && (light.target[0] !== light.position[0] || light.target[1] !== light.position[1] || light.position[2] > 0);
}

/** Whether `preset` is a well-formed stored preset: the operator's own, or an edit of a built-in. */
export function validPreset(preset: LightingPreset) {
  const { environment, constant } = preset ?? {} as LightingPreset;
  return Boolean(preset) && ID.test(preset.id) && (!preset.id.startsWith('builtin-') || BUILT_IN_IDS.includes(preset.id))
    && !preset.builtIn && !preset.edited
    && typeof preset.name === 'string' && preset.name.trim().length > 0 && preset.name.length <= 80
    && Array.isArray(preset.lights) && preset.lights.length <= 32 && preset.lights.every(validLight)
    && new Set(preset.lights.map((light) => light.id)).size === preset.lights.length
    && Boolean(environment) && typeof environment.enabled === 'boolean' && (environment.path === null || (typeof environment.path === 'string' && /^\/.+\.exr$/i.test(environment.path)))
    && finite(environment.gain, 0, 100) && finite(environment.rotation, -360, 360)
    && (environment.maxRadiance == null || (finite(environment.maxRadiance, 0, 1e6) && environment.maxRadiance > 0))
    && Boolean(constant) && typeof constant.enabled === 'boolean' && finite(constant.intensity, 0, 100) && rgb(constant.color)
    && (!environment.enabled || environment.path !== null);
}

/** The stored lighting document, or an error the store returns to the page. */
export function validateLightingLibrary(data: unknown): LightingLibrary {
  const library = data as LightingLibrary;
  if (!library || library.version !== 1 || !Array.isArray(library.presets) || library.presets.length > 100
    || !library.presets.every(validPreset) || new Set(library.presets.map((preset) => preset.id)).size !== library.presets.length
    || !library.selected || typeof library.selected !== 'object' || Array.isArray(library.selected)
    || !Object.entries(library.selected).every(([match, preset]) => MATCH_ID.test(match) && typeof preset === 'string' && ID.test(preset)))
    throw new Error('灯光预设数据无效。');
  return library;
}

export const emptyLibrary = (): LightingLibrary => ({ version: 1, presets: [], selected: {} });
