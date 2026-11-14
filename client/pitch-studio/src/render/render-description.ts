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

/** Calls description.load on chr-server through the page's /chr-render WebSocket proxy. */
export function loadDescription(url: string, scene: DescriptionScene, timeoutMs = 60000): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url);
    const timer = setTimeout(() => { socket.close(); reject(new Error('chr-server did not answer in time.')); }, timeoutMs);
    const finish = (settle: () => void) => { clearTimeout(timer); socket.close(); settle(); };
    socket.onopen = () => socket.send(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'description.load', params: { scene } }));
    socket.onerror = () => finish(() => reject(new Error(`Cannot reach chr-server at ${url}.`)));
    socket.onmessage = (event) => {
      const message = JSON.parse(String(event.data)) as { id?: unknown; result?: unknown; error?: { message?: string } };
      if (message.id !== 1) return;
      finish(() => message.error ? reject(new Error(message.error.message ?? 'description.load failed.')) : resolve(message.result));
    };
  });
}
