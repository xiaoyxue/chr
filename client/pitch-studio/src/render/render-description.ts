// Converts the match's placements into a chr-render-description-v1 scene (see
// server/docs/render-description.md) and loads it into chr-server.
// The matrices mirror FusionCreatives.update, so the program output matches this
// preview. They are forward row-major and act on column vectors, in chr world
// coordinates: pitch-centred metres, ground Z = 0, up is negative Z.
import { fitFusionModel } from '../placements/fusion-creatives.ts';
import type { FieldDimensions, GroundPlacement, ModelPlacement, Placement } from '../shared/types.ts';

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
export interface DescriptionScene {
  ground: { pitch_m: [number, number] };
  assets: { id: string; kind: DescribedAsset['kind']; source: { package: string; entry: string }; sha256: string; blend_as?: 'mask' }[];
  instances: { id: string; asset: string; model: Rows; visible: boolean }[];
}

/** Names of a GLB's materials with `alphaMode: BLEND`. */
export function blendedMaterials(glb: Uint8Array): string[] {
  const view = new DataView(glb.buffer, glb.byteOffset, glb.byteLength);
  const json = JSON.parse(new TextDecoder().decode(glb.subarray(20, 20 + view.getUint32(12, true)))) as
    { materials?: { name?: string; alphaMode?: string }[] };
  return (json.materials ?? []).filter((material) => material.alphaMode === 'BLEND').map((material) => material.name ?? 'unnamed');
}

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
 */
export function describeScene(options: {
  field: FieldDimensions;
  slots: Placement[];
  hidden: ReadonlySet<string>;
  geometry: ReadonlyMap<string, SlotGeometry>;
  assets: ReadonlyMap<string, DescribedAsset>;
  packagePath: string;
}): DescriptionScene {
  const scene: DescriptionScene = { ground: { pitch_m: [options.field.length, options.field.width] }, assets: [], instances: [] };
  for (const slot of options.slots) {
    const geometry = options.geometry.get(slot.id);
    const asset = slot.asset && options.assets.get(slot.asset.id);
    if (!slot.asset || !geometry || !asset) continue;
    let model: Rows;
    if (slot.type === '3D' && geometry.kind === 'glb' && asset.kind === 'glb') model = glbModel(slot, geometry.bounds);
    else if (slot.type === '2D' && geometry.kind === 'image' && asset.kind === 'image_quad') model = quadModel(slot, geometry);
    else throw new Error(`Placement ${slot.id} does not match its material.`);
    if (!scene.assets.some((listed) => listed.id === slot.asset!.id))
      scene.assets.push({ id: slot.asset.id, kind: asset.kind, source: { package: options.packagePath, entry: asset.entry },
        sha256: asset.sha256, ...(asset.blendAs ? { blend_as: asset.blendAs } : {}) });
    scene.instances.push({ id: slot.id, asset: slot.asset.id, model, visible: !options.hidden.has(slot.id) });
  }
  return scene;
}

export async function sha256(blob: Blob): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', await blob.arrayBuffer()));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

/** A JSON-RPC error answer from chr-server. */
export class RpcError extends Error {
  constructor(readonly code: number, message: string) { super(message); }
}
const METHOD_NOT_FOUND = -32601;

/** `render.get_worker_status` result (chr_common::methods::render_worker::WorkerStatus). */
export interface WorkerStatus {
  state: 'starting' | 'ready' | 'backoff';
  pid: number | null;
  restarts: number;
  deadline_misses: number;
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
async function session<T>(url: string, run: (call: Call) => Promise<T>): Promise<T> {
  const socket = new WebSocket(url);
  await new Promise<void>((resolve, reject) => {
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
  const pending = new Map<number, { method: string; resolve: (value: unknown) => void; reject: (error: unknown) => void; timer: ReturnType<typeof setTimeout> }>();
  let next = 0;
  const failAll = (error: Error) => {
    for (const request of pending.values()) { clearTimeout(request.timer); request.reject(error); }
    pending.clear();
  };
  socket.onmessage = (event) => {
    const message = JSON.parse(String(event.data)) as { id?: number; result?: unknown; error?: { code: number; message?: string } };
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    clearTimeout(request.timer);
    if (message.error) request.reject(new RpcError(message.error.code, message.error.message ?? '未知错误'));
    else request.resolve(message.result);
  };
  socket.onclose = () => failAll(new Error('chr-server 断开了连接（可能正在重启）；请确认它已恢复后重试。'));
  const call: Call = (method, params, timeoutMs = 5000) => new Promise((resolve, reject) => {
    const id = ++next;
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`chr-server 在 ${timeoutMs / 1000} 秒内没有回应 ${method}。`));
    }, timeoutMs);
    pending.set(id, { method, resolve, reject: (error) => reject(new Error(rendererFailure(method, error))), timer });
    socket.send(JSON.stringify({ jsonrpc: '2.0', id, method, ...(params === undefined ? {} : { params }) }));
  });
  try { return await run(call); }
  finally { socket.onclose = null; failAll(new Error('connection closed')); socket.close(); }
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
    const sdi = await call('sdi.get_status') as SdiStatus;
    const loaded = await call('description.load', { scene }, timeoutMs) as LoadResult;
    return { loaded, sdi, worker };
  });
}
