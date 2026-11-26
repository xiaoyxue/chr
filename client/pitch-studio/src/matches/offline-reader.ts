// Reads and verifies a Fusion offline ZIP without browser or three.js APIs, so the page and its
// onsite server (scripts/studio-store.ts) check a package by the same rules.
import { unzip } from 'fflate';
import { MAX_PLY_BYTES } from '../analysis/ply-result.ts';

const MAX_MATERIAL_BYTES = 256 * 1024 * 1024;
const MAX_MANIFEST_BYTES = 1024 * 1024;
export const EXTENSIONS: Record<string, string> = {
  'image/png': '.png', 'image/jpeg': '.jpg', 'image/webp': '.webp', 'model/gltf-binary': '.glb',
};

export function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid offline package manifest.');
  return value as Record<string, unknown>;
}

function checkMaterial(bytes: Uint8Array, contentType: string) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const text = (offset: number, length: number) => new TextDecoder().decode(bytes.subarray(offset, offset + length));
  if (contentType === 'model/gltf-binary') {
    if (bytes.length < 24 || text(0, 4) !== 'glTF' || view.getUint32(4, true) !== 2
      || view.getUint32(8, true) !== bytes.length || text(16, 4) !== 'JSON'
      || view.getUint32(12, true) > bytes.length - 20) throw new Error('Invalid embedded GLB material.');
    const root: unknown = JSON.parse(text(20, view.getUint32(12, true)));
    const pending: unknown[] = [root];
    while (pending.length) {
      const item = pending.pop();
      if (!item || typeof item !== 'object') continue;
      for (const [key, value] of Object.entries(item)) {
        if (key === 'uri' && (typeof value !== 'string' || !value.startsWith('data:')))
          throw new Error('Offline GLB materials must not reference external files.');
        if (value && typeof value === 'object') pending.push(value);
      }
    }
  } else if (contentType === 'image/png') {
    if (bytes.length < 24 || ![137, 80, 78, 71, 13, 10, 26, 10].every((x, i) => bytes[i] === x))
      throw new Error('Invalid PNG material.');
  } else if (contentType === 'image/jpeg') {
    if (bytes.length < 4 || bytes[0] !== 255 || bytes[1] !== 216 || bytes[2] !== 255
      || bytes[bytes.length - 2] !== 255 || bytes[bytes.length - 1] !== 217) throw new Error('Invalid JPEG material.');
  } else if (contentType === 'image/webp') {
    if (bytes.length < 30 || text(0, 4) !== 'RIFF' || text(8, 4) !== 'WEBP'
      || view.getUint32(4, true) !== bytes.length - 8) throw new Error('Invalid WebP material.');
  } else throw new Error('Unsupported offline material format.');
}

/** Names of a GLB's materials with `alphaMode: BLEND`. */
export function blendedMaterials(glb: Uint8Array): string[] {
  const view = new DataView(glb.buffer, glb.byteOffset, glb.byteLength);
  const json = JSON.parse(new TextDecoder().decode(glb.subarray(20, 20 + view.getUint32(12, true)))) as
    { materials?: { name?: string; alphaMode?: string }[] };
  return (json.materials ?? []).filter((material) => material.alphaMode === 'BLEND').map((material) => material.name ?? 'unnamed');
}

export interface OfflineAsset { id: string; contentType: string; bytes: Uint8Array; sha256: string }
/** A v2 package's point cloud of the field (`match.field.pointCloud`), in pitch coordinates. */
export interface OfflinePointCloud { contentPath: string; fileName: string; sha256: string; bytes: Uint8Array }

async function sha256(bytes: Uint8Array) {
  const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', new Uint8Array(bytes).buffer));
  return Array.from(hash, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

/**
 * The manifest's match, as Fusion wrote it, and each material, checked against the
 * manifest's metadata and SHA-256 and by its format; a version 2 package may also
 * carry the field's point cloud, checked the same way. Which placements use which
 * material is the caller's check (mapFusionMatch).
 */
export async function readOfflinePackage(bytes: Uint8Array): Promise<{ source: Record<string, unknown>; assets: OfflineAsset[]; pointCloud?: OfflinePointCloud }> {
  if (!bytes.length || bytes.length > MAX_MATERIAL_BYTES + MAX_PLY_BYTES + 4 * MAX_MANIFEST_BYTES)
    throw new Error('Select a nonempty Fusion ZIP package up to 324 MB (256 MB of materials, 64 MB of point cloud).');
  const names = new Set<string>();
  let total = 0;
  const files = await new Promise<Record<string, Uint8Array>>((resolve, reject) => {
    unzip(bytes, { filter: (entry) => {
      if (names.has(entry.name) || names.size >= 1025
        || !/^(?:manifest\.json|assets\/[A-Za-z0-9][A-Za-z0-9_-]{0,63}\.(?:png|jpg|webp|glb)|point-cloud\/[A-Za-z0-9][A-Za-z0-9_-]{0,63}\.ply)$/.test(entry.name))
        throw new Error('The offline ZIP contains duplicate or unexpected files.');
      names.add(entry.name);
      const maximum = entry.name === 'manifest.json' ? MAX_MANIFEST_BYTES : entry.name.endsWith('.ply') ? MAX_PLY_BYTES
        : MAX_MATERIAL_BYTES;
      if (!Number.isSafeInteger(entry.originalSize) || entry.originalSize <= 0 || entry.originalSize > maximum
        || (total += entry.originalSize) > MAX_MATERIAL_BYTES + MAX_PLY_BYTES + MAX_MANIFEST_BYTES)
        throw new Error('The offline package exceeds its material size limits.');
      return true;
    } }, (error, result) => error ? reject(error) : resolve(result));
  });
  if (!files['manifest.json']) throw new Error('The offline package is missing manifest.json.');
  const manifest = object(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(files['manifest.json'])));
  if (manifest.packageVersion !== 1 && manifest.packageVersion !== 2) throw new Error('Unsupported offline package version.');
  const source = object(manifest.match), hashes = object(manifest.sha256);
  if (typeof source.id !== 'string' || !Array.isArray(source.assets)) throw new Error('Invalid offline match configuration.');
  const assets: OfflineAsset[] = [];
  for (const raw of source.assets) {
    const asset = object(raw);
    if (typeof asset.id !== 'string' || typeof asset.contentType !== 'string') throw new Error('Invalid offline asset.');
    const path = `assets/${asset.id}${EXTENSIONS[asset.contentType]}`;
    const bytes = files[path], expected = hashes[path];
    if (asset.contentPath !== path || !bytes
      || !Number.isSafeInteger(asset.byteLength) || asset.byteLength !== bytes.length
      || typeof expected !== 'string' || !/^[a-f0-9]{64}$/.test(expected)) throw new Error('An offline asset is missing or has invalid metadata.');
    if (await sha256(bytes) !== expected) throw new Error(`Material checksum failed: ${asset.id}. Export the package again.`);
    checkMaterial(bytes, asset.contentType);
    assets.push({ id: asset.id, contentType: asset.contentType, bytes, sha256: expected });
  }
  const pointCloud = manifest.packageVersion === 2 ? await readPointCloud(source, files, hashes) : undefined;
  const entries = assets.length + (pointCloud ? 1 : 0);
  if (Object.keys(files).length !== entries + 1 || Object.keys(hashes).length !== entries)
    throw new Error('The package contains missing or unreferenced materials.');
  return { source, assets, ...(pointCloud ? { pointCloud } : {}) };
}

/** The point cloud `match.field.pointCloud` names, checked against its byte length and the manifest's SHA-256. */
async function readPointCloud(source: Record<string, unknown>, files: Record<string, Uint8Array>, hashes: Record<string, unknown>) {
  const raw = object(source.field).pointCloud;
  if (raw === undefined || raw === null) return undefined;
  const cloud = object(raw), path = cloud.contentPath;
  if (typeof path !== 'string' || !/^point-cloud\/[A-Za-z0-9][A-Za-z0-9_-]{0,63}\.ply$/.test(path)) throw new Error('Invalid offline point cloud path.');
  const bytes = files[path], expected = hashes[path];
  if (!bytes || typeof cloud.fileName !== 'string' || !cloud.fileName || cloud.fileName.length > 255
    || !Number.isSafeInteger(cloud.byteLength) || cloud.byteLength !== bytes.length
    || typeof expected !== 'string' || !/^[a-f0-9]{64}$/.test(expected)) throw new Error('The offline point cloud is missing or has invalid metadata.');
  if (await sha256(bytes) !== expected) throw new Error('Point cloud checksum failed. Export the package again.');
  return { contentPath: path, fileName: cloud.fileName, sha256: expected, bytes };
}
