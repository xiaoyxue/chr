// Converts the match's placements into a chr-render-description-v1 scene (see
// server/docs/render-description.md) and loads it into chr-server.
// The matrices mirror FusionCreatives.update and LocalAdScene.update, so the
// program output matches the preview. They are forward row-major and act on column vectors, in chr world
// coordinates: pitch-centred metres, ground Z = 0, up is negative Z.
import * as THREE from 'three';
import { fitFusionModel } from '../placements/fusion-creatives.ts';
import { flagModel } from '../placements/flag.ts';
import { wireMotion, type WireMotion } from '../placements/model-motion.ts';
import { adRoot, modelFit } from '../placements/local-ad-transform.ts';
import { imageOpacity, type LocalAd } from '../placements/local-ad.ts';
import type { FieldDimensions, GroundPlacement, ModelPlacement, Placement } from '../shared/types.ts';
import type { WireLighting } from '../lighting/lighting.ts';

export type Rows = [number, number, number, number][];
/** Axis-aligned bounds of a loaded GLB in its own Y-up glTF space, as three.js reports them. */
export interface GlbBounds { min: [number, number, number]; max: [number, number, number] }
export type SlotGeometry = { kind: 'glb'; bounds: GlbBounds } | { kind: 'image'; width: number; height: number };
export interface DescribedAsset {
  kind: 'glb' | 'image_quad';
  entry: string;
  sha256: string;
  /** Import the GLB's alpha-blended materials as masks; the renderer has no translucent pass. */
  blendAs?: 'mask';
}
/** A local placement (2D, 3D or flag) in chr world coordinates whose material chr-server reads at `ad.materialPath`. */
export interface DescribedLocal {
  ad: LocalAd;
  /** Lowercase hex SHA-256 of the material bytes. */
  sha256: string;
  /** A 3D placement's GLB bounds. */
  bounds?: GlbBounds;
  blendAs?: 'mask';
}
/** `chr_common::scene_frame::Flag`: the flag's size in metres and its wave. */
export interface FlagParameters {
  width_m: number;
  height_m: number;
  amplitude_m: number;
  frequency: number;
  speed: number;
  material: 'simple' | 'pbr';
}
export interface DescriptionScene {
  ground: { pitch_m: [number, number] };
  assets: { id: string; kind: DescribedAsset['kind'] | 'flag' | 'environment'; source: { package: string; entry: string } | { path: string }; sha256: string; blend_as?: 'mask' }[];
  /** `opacity`: an image quad's, 0 to 1, multiplying its image's alpha. */
  instances: { id: string; asset: string; model: Rows; visible: boolean; flag?: FlagParameters; motion?: WireMotion; opacity?: number }[];
  /** Absent: chr-server's default rig. */
  lighting?: WireLighting;
}
/** A lighting preset as the description carries it, with the OpenEXR panorama it names, if any. */
export interface DescribedLighting { lighting: WireLighting; environment?: { id: string; path: string; sha256: string } }
/** One asset per distinct panorama, by its content. */
export const environmentAssetId = (sha256: string) => `IBL-${sha256.slice(0, 16)}`;
/** chr-server places at most this many instances per description. */
export const MAX_INSTANCES = 63;
/** One asset per distinct local material and kind: flags `FLAG-`, images and models `LOCAL-`. */
export const localAssetId = (ad: LocalAd, sha256: string) => `${ad.type === 'flag' ? 'FLAG' : 'LOCAL'}-${sha256.slice(0, 16)}`;
const LOCAL_KIND = { '2D': 'image_quad', '3D': 'glb', flag: 'flag' } as const;

const rows = (matrix: THREE.Matrix4): Rows => [0, 1, 2, 3].map((i) => [0, 1, 2, 3].map((j) => matrix.elements[j * 4 + i])) as Rows;

/**
 * A local 2D ad: chr's unit image quad (top-left texel at (-0.5, -0.5)) stretched
 * over the rectangle like the preview's plane: columns u · width, v · height, the
 * normal u × v, and the rectangle's centre lifted as in the preview.
 */
export function localQuadModel(ad: LocalAd): Rows {
  return rows(adRoot(ad).multiply(new THREE.Matrix4().makeScale(ad.plane.width, ad.plane.height, 1)));
}

/**
 * A local 3D ad, placed like the preview: the glTF scene centred on its footprint
 * and standing on the rectangle, turned by yaw and fitted (modelFit). chr's GLB
 * basis is glTF (x, y, z) -> (x, z, -y), so it is first turned back to glTF.
 */
export function localGlbModel(ad: LocalAd, bounds: GlbBounds): Rows {
  const [minX, minY, minZ] = bounds.min, [maxX, , maxZ] = bounds.max;
  const fit = modelFit(ad, [Math.max(maxX - minX, 0.001), Math.max(maxZ - minZ, 0.001)]);
  const toGltf = new THREE.Matrix4().set(1, 0, 0, 0, 0, 0, -1, 0, 0, 1, 0, 0, 0, 0, 0, 1);
  return rows(adRoot(ad)
    .multiply(new THREE.Matrix4().compose(new THREE.Vector3(), new THREE.Quaternion().setFromEuler(fit.rotation), new THREE.Vector3().setScalar(fit.scale)))
    .multiply(new THREE.Matrix4().makeTranslation(-(minX + maxX) / 2, -minY, -(minZ + maxZ) / 2))
    .multiply(toGltf));
}

export { blendedMaterials } from '../matches/offline-reader.ts';

const radians = (degrees: number) => degrees * Math.PI / 180;

/**
 * FusionCreatives centres the glTF scene on its ground footprint, turns it upright
 * (glTF +Y to chr -Z, the importer's basis) and fits it into the placement. In chr's
 * basis the centring is a translation by (-cx, -cz, minY) of the glTF bounds.
 */
export function glbModel(slot: ModelPlacement, bounds: GlbBounds): Rows {
  const width = bounds.max[0] - bounds.min[0], depth = bounds.max[2] - bounds.min[2];
  const fit = fitFusionModel(width, depth, slot);
  const c = Math.cos(fit.yaw) * fit.scale, s = Math.sin(fit.yaw) * fit.scale;
  const centre = [-(bounds.min[0] + bounds.max[0]) / 2, -(bounds.min[2] + bounds.max[2]) / 2, bounds.min[1]];
  return [
    [c, -s, 0, fit.x + c * centre[0] - s * centre[1]],
    [s, c, 0, fit.y + s * centre[0] + c * centre[1]],
    [0, 0, fit.scale, slot.z - 0.015 + fit.scale * centre[2]],
    [0, 0, 0, 1],
  ];
}

/**
 * A Fusion model's animation (#157) about its neutral bottom centre, where glbModel
 * stands it; undefined when its pose does not move.
 */
export function glbMotion(slot: ModelPlacement, bounds: GlbBounds): WireMotion | undefined {
  if (!slot.modelPose) return undefined;
  const fit = fitFusionModel(bounds.max[0] - bounds.min[0], bounds.max[2] - bounds.min[2], slot);
  return wireMotion(slot.modelPose, [fit.x, fit.y, slot.z - 0.015]);
}

/** The unit image quad scaled to fit the placement like the preview, image top towards -Y. */
export function quadModel(slot: GroundPlacement, image: { width: number; height: number }): Rows {
  const rotated = slot.assetRotation === 90 || slot.assetRotation === 270;
  const scale = Math.min(slot.sizeX / (rotated ? image.height : image.width),
    slot.sizeY / (rotated ? image.width : image.height));
  const angle = radians(slot.rz + (slot.assetRotation ?? 0));
  const c = Math.cos(angle), s = Math.sin(angle), w = image.width * scale, h = image.height * scale;
  return [[c * w, -s * h, 0, slot.x], [s * w, c * h, 0, slot.y], [0, 0, 1, -0.02], [0, 0, 0, 1]];
}

/**
 * Placements with a loaded asset become instances; each asset is listed once, read
 * by chr-server from `packagePath` (a ZIP on the onsite server) at its entry.
 * Local placements follow, their materials read from their server paths.
 */
export function describeScene(options: {
  field: FieldDimensions;
  slots: Placement[];
  hidden: ReadonlySet<string>;
  geometry: ReadonlyMap<string, SlotGeometry>;
  assets: ReadonlyMap<string, DescribedAsset>;
  packagePath: string;
  locals?: readonly DescribedLocal[];
  lighting?: DescribedLighting;
}): DescriptionScene {
  const scene: DescriptionScene = { ground: { pitch_m: [options.field.length, options.field.width] }, assets: [], instances: [] };
  for (const slot of options.slots) {
    const geometry = options.geometry.get(slot.id);
    const asset = slot.asset && options.assets.get(slot.asset.id);
    if (!slot.asset || !geometry || !asset) continue;
    let model: Rows, motion: WireMotion | undefined, opacity: number | undefined;
    if (slot.type === '3D' && geometry.kind === 'glb' && asset.kind === 'glb') {
      model = glbModel(slot, geometry.bounds);
      motion = glbMotion(slot, geometry.bounds);
    } else if (slot.type === '2D' && geometry.kind === 'image' && asset.kind === 'image_quad') {
      model = quadModel(slot, geometry);
      opacity = slot.assetOpacity;
    } else throw new Error(`Placement ${slot.id} does not match its material.`);
    if (!scene.assets.some((listed) => listed.id === slot.asset!.id))
      scene.assets.push({ id: slot.asset.id, kind: asset.kind, source: { package: options.packagePath, entry: asset.entry },
        sha256: asset.sha256, ...(asset.blendAs ? { blend_as: asset.blendAs } : {}) });
    scene.instances.push({ id: slot.id, asset: slot.asset.id, model, visible: !options.hidden.has(slot.id),
      ...(motion ? { motion } : {}), ...(opacity === undefined ? {} : { opacity }) });
  }
  for (const { ad, sha256, bounds, blendAs } of options.locals ?? []) {
    const asset = localAssetId(ad, sha256);
    if (!scene.assets.some((listed) => listed.id === asset))
      scene.assets.push({ id: asset, kind: LOCAL_KIND[ad.type], source: { path: ad.materialPath }, sha256, ...(blendAs ? { blend_as: blendAs } : {}) });
    if (ad.type === 'flag') {
      const { width, height } = ad.plane, { amplitude, frequency, speed, material } = ad.flag;
      scene.instances.push({ id: `flag-${ad.id}`, asset, model: flagModel(ad.plane, ad.offset), visible: ad.visible,
        flag: { width_m: width, height_m: height, amplitude_m: amplitude, frequency, speed, material } });
    } else if (ad.type === '2D') {
      scene.instances.push({ id: `local-${ad.id}`, asset, model: localQuadModel(ad), visible: ad.visible, opacity: imageOpacity(ad) });
    } else {
      scene.instances.push({ id: `local-${ad.id}`, asset, model: localGlbModel(ad, bounds), visible: ad.visible });
    }
  }
  if (options.lighting) {
    const { lighting, environment } = options.lighting;
    if (environment) scene.assets.push({ id: environment.id, kind: 'environment', source: { path: environment.path }, sha256: environment.sha256 });
    scene.lighting = lighting;
  }
  if (scene.instances.length > MAX_INSTANCES)
    throw new Error(`chr-server 每个场景最多放置 ${MAX_INSTANCES} 个实例，当前有 ${scene.instances.length} 个（含停用的广告位）；请删除部分后再发送。`);
  return scene;
}

/** A JSON-RPC error answer from chr-server. */
export class RpcError extends Error {
  constructor(readonly code: number, message: string) { super(message); }
}
const METHOD_NOT_FOUND = -32601;
const NO_TRACKER = 'no tracking worker';

/** `render.get_worker_status` result (chr_common::methods::render_worker::WorkerStatus). */
export interface WorkerStatus {
  state: 'starting' | 'ready' | 'backoff';
  pid: number | null;
  restarts: number;
  deadline_misses: number;
  last_error: string | null;
}
/** `tracking.get_worker_status` result (chr_common::methods::tracking::TrackerStatus). */
export interface TrackerStatus {
  state: WorkerStatus['state'];
  /** The worker executable's file name, e.g. `chr-tracking-sim-cli` or `pitch-analysis`. */
  worker: string;
  /** VchaoFrame stream of the current worker session. */
  stream_id: string | null;
  pid: number | null;
  restarts: number;
  requests: number;
  cameras: number;
  unavailable: number;
  not_sent: number;
  last_error: string | null;
}
/** `sdi.get_status` result (chr_common::methods::sdi::SdiStatus). */
export interface SdiStatus { video_mode: string; frames_output: number; repeated_frames: number; dropped_frames: number }
export interface LoadResult { scene_revision: string; assets_imported: number; assets_reused: number }

/** Why the render worker cannot take a scene now, or undefined when it is ready. */
export function workerProblem(status: WorkerStatus): string | undefined {
  if (status.state === 'ready') return undefined;
  const detail = status.last_error ? `最近错误：${status.last_error}。` : '';
  return status.state === 'starting'
    ? `渲染 worker 正在启动，尚未就绪；请稍候几秒再发送。${detail}`
    : `渲染 worker 已退出，正在等待自动重启（已重启 ${status.restarts} 次）。${detail}请查看渲染日志（stack 为 output/stack/render.log）。`;
}

/** Why the tracking worker cannot supply cameras now, or undefined when it is ready. */
export function trackerProblem(status: TrackerStatus): string | undefined {
  if (status.state === 'ready') return undefined;
  const detail = status.last_error ? `最近错误：${status.last_error}。` : '';
  return status.state === 'starting'
    ? `跟踪 worker 正在启动，尚未就绪；此时的画面没有相机、不会渲染，请稍候再发送。${detail}`
    : `跟踪 worker 已退出，正在等待自动重启（已重启 ${status.restarts} 次）。${detail}请查看渲染日志（stack 为 output/stack/render.log）。`;
}

/** The operator-facing reason a renderer call failed. */
export function rendererFailure(method: string, error: unknown): string {
  if (!(error instanceof RpcError)) return error instanceof Error ? error.message : String(error);
  if (error.code === METHOD_NOT_FOUND) {
    if (method === 'render.get_worker_status')
      return 'chr-server 已连接，但没有运行渲染 worker（可能是默认的三角形程序）。请用 --tracked-scene 或 --description 启动渲染程序。';
    if (method === 'description.load')
      return 'chr-server 没有启用场景描述加载：请以 --description 和 --asset-root 启动（stack.toml 的 description / asset_root）。';
  }
  return `chr-server 拒绝了 ${method}（错误 ${error.code}）：${error.message}`;
}

type Call = (method: string, params?: unknown, timeoutMs?: number) => Promise<unknown>;

/** Runs `run` on one connection to chr-server through the page's /chr-render WebSocket proxy. */
/** One open control connection per page, kept between sends: through a port forward or an SSH
 * tunnel, opening a new WebSocket can take far longer than the calls themselves. */
let connection: { url: string; ready: Promise<Call> } | undefined;

function connect(url: string): Promise<Call> {
  const socket = new WebSocket(url);
  const pending = new Map<number, { method: string; resolve: (value: unknown) => void; reject: (error: unknown) => void; timer: ReturnType<typeof setTimeout> }>();
  let next = 0;
  const failAll = (error: Error) => {
    for (const request of pending.values()) { clearTimeout(request.timer); request.reject(error); }
    pending.clear();
  };
  const opened = new Promise<void>((resolve, reject) => {
    const unreachable = async (reason: string) => {
      socket.close();
      let target = '';
      try { target = ((await (await fetch('/chr-render-info', { cache: 'no-store' })).json()) as { target: string }).target; }
      catch { /* An older page server has no /chr-render-info; the message stays generic. */ }
      reject(new Error(`无法连接 chr-server：${reason}。页面代理 /chr-render${target ? ` 转发到 ${target}` : ''}；`
        + '请确认渲染服务已启动（uv run server/scripts/stack.py status），且页面服务的 CHR_RENDER_URL 与 chr-server 的 --ws 地址一致。'));
    };
    const timer = setTimeout(() => { void unreachable('5 秒内未建立连接'); }, 5000);
    socket.onopen = () => { clearTimeout(timer); socket.onerror = null; resolve(); };
    socket.onerror = () => { clearTimeout(timer); void unreachable('连接被拒绝'); };
  });
  socket.onmessage = (event) => {
    const message = JSON.parse(String(event.data)) as { id?: number; result?: unknown; error?: { code: number; message?: string } };
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    clearTimeout(request.timer);
    if (message.error) request.reject(new RpcError(message.error.code, message.error.message ?? '未知错误'));
    else request.resolve(message.result);
  };
  socket.onclose = () => {
    if (connection?.url === url) connection = undefined;
    failAll(new Error('chr-server 断开了连接（可能正在重启）；请确认它已恢复后重试。'));
  };
  const call: Call = (method, params, timeoutMs = 5000) => new Promise((resolve, reject) => {
    const id = ++next;
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`chr-server 在 ${timeoutMs / 1000} 秒内没有回应 ${method}。`));
    }, timeoutMs);
    pending.set(id, { method, resolve, reject: (error) => reject(new Error(
      method === 'tracking.get_worker_status' && error instanceof RpcError && error.code === METHOD_NOT_FOUND
        ? NO_TRACKER : rendererFailure(method, error))), timer });
    socket.send(JSON.stringify({ jsonrpc: '2.0', id, method, ...(params === undefined ? {} : { params }) }));
  });
  return opened.then(() => call);
}

/** Runs `run` on the page's control connection, opening it when there is none. */
async function session<T>(url: string, run: (call: Call) => Promise<T>): Promise<T> {
  if (connection?.url !== url) {
    const ready = connect(url);
    connection = { url, ready };
    ready.catch(() => { if (connection?.ready === ready) connection = undefined; });
  }
  return run(await connection.ready);
}

/**
 * Checks that chr-server is reachable and its render worker is ready, then calls
 * description.load. Every failure becomes an operator-facing message.
 */
export function sendDescription(url: string, scene: DescriptionScene, timeoutMs = 60000) {
  return session(url, async (call) => {
    const worker = await call('render.get_worker_status') as WorkerStatus;
    const problem = workerProblem(worker);
    if (problem) throw new Error(problem);
    // Only a server started with --tracking-worker answers; otherwise cameras come from tracking.frame.
    const tracker = await call('tracking.get_worker_status').catch((error: unknown): undefined => {
      if (error instanceof Error && error.message.startsWith(NO_TRACKER)) return undefined;
      throw error;
    }) as TrackerStatus | undefined;
    const trackingProblem = tracker && trackerProblem(tracker);
    if (trackingProblem) throw new Error(trackingProblem);
    const sdi = await call('sdi.get_status') as SdiStatus;
    const loaded = await call('description.load', { scene }, timeoutMs) as LoadResult;
    return { loaded, sdi, worker };
  });
}
