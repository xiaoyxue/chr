// Reads and verifies a Fusion offline ZIP without browser or three.js APIs, so the page and its
// onsite server (scripts/studio-store.ts) check a package by the same rules.
import { MAX_PLY_BYTES } from '../analysis/ply-result.ts';
import { MAX_ASSET_BYTES, MAX_MANIFEST_BYTES, MAX_MATERIAL_BYTES, MAX_PACKAGE_BYTES, formatBytes } from './offline-limits.ts';
import { blobSource, bytesSource, centralDirectory, readEntry, type RandomAccess, type ZipEntry } from './offline-zip.ts';

const ENTRY_NAME = /^(?:manifest\.json|assets\/[A-Za-z0-9][A-Za-z0-9_-]{0,63}\.(?:png|jpg|webp|glb)|point-cloud\/[A-Za-z0-9][A-Za-z0-9_-]{0,63}\.ply)$/;
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

/** The ZIP's entries by name, each within its own limit and the materials within theirs together, before any is unpacked. */
async function packageEntries(archive: RandomAccess) {
  if (!archive.size) throw new Error('Select a nonempty Fusion ZIP package.');
  const entries = new Map<string, ZipEntry>();
  let materials = 0;
  for (const entry of await centralDirectory(archive, 1025)) {
    if (entries.has(entry.name) || !ENTRY_NAME.test(entry.name)) throw new Error('The offline ZIP contains duplicate or unexpected files.');
    const maximum = entry.name === 'manifest.json' ? MAX_MANIFEST_BYTES : entry.name.endsWith('.ply') ? MAX_PLY_BYTES : MAX_ASSET_BYTES;
    if (entry.size <= 0 || entry.size > maximum)
      throw new Error(`The offline package's ${entry.name} is ${formatBytes(entry.size)} (${entry.size} bytes); each may be at most ${formatBytes(maximum)}.`);
    if (entry.name.startsWith('assets/')) materials += entry.size;
    entries.set(entry.name, entry);
  }
  if (materials > MAX_MATERIAL_BYTES)
    throw new Error(`The Fusion materials total ${formatBytes(materials)} (${materials} bytes), over the offline package limit of `
      + `${formatBytes(MAX_MATERIAL_BYTES)} (${MAX_MATERIAL_BYTES} bytes).`);
  if (archive.size > MAX_PACKAGE_BYTES)
    throw new Error(`The offline ZIP is ${formatBytes(archive.size)}, over its ${formatBytes(MAX_PACKAGE_BYTES)} limit `
      + `(${formatBytes(MAX_MATERIAL_BYTES)} of materials, ${formatBytes(MAX_PLY_BYTES)} of point cloud).`);
  return entries;
}

/** A ZIP's bytes: in memory, a file chosen in the browser, or read by offset (the page server's file handle). */
export type OfflinePackageInput = Uint8Array | Blob | RandomAccess;

/**
 * The manifest's match, as Fusion wrote it, and each material, checked against the
 * manifest's metadata and SHA-256 and by its format; a version 2 package may also
 * carry the field's point cloud, checked the same way. Which placements use which
 * material is the caller's check (mapFusionMatch). Materials are unpacked one at a
 * time and handed to `keep`, whose results are returned, so a caller that stores
 * each one (the page server) never holds the package in memory.
 */
export async function readOfflinePackage<T = OfflineAsset>(input: OfflinePackageInput,
  keep: (asset: OfflineAsset) => T | Promise<T> = (asset) => asset as T): Promise<{ source: Record<string, unknown>; assets: T[]; pointCloud?: OfflinePointCloud }> {
  const archive = input instanceof Uint8Array ? bytesSource(input) : input instanceof Blob ? blobSource(input) : input;
  const entries = await packageEntries(archive);
  const manifestEntry = entries.get('manifest.json');
  if (!manifestEntry) throw new Error('The offline package is missing manifest.json.');
  const manifest = object(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(await readEntry(archive, manifestEntry))));
  if (manifest.packageVersion !== 1 && manifest.packageVersion !== 2) throw new Error('Unsupported offline package version.');
  const source = object(manifest.match), hashes = object(manifest.sha256);
  if (typeof source.id !== 'string' || !Array.isArray(source.assets)) throw new Error('Invalid offline match configuration.');
  // Every material's metadata against the archive, before any material is unpacked.
  const declared = source.assets.map((raw) => {
    const asset = object(raw);
    if (typeof asset.id !== 'string' || typeof asset.contentType !== 'string') throw new Error('Invalid offline asset.');
    const path = `assets/${asset.id}${EXTENSIONS[asset.contentType]}`;
    const entry = entries.get(path), expected = hashes[path];
    if (asset.contentPath !== path || !entry || asset.byteLength !== entry.size
      || typeof expected !== 'string' || !/^[a-f0-9]{64}$/.test(expected)) throw new Error('An offline asset is missing or has invalid metadata.');
    return { id: asset.id, contentType: asset.contentType, entry, sha256: expected };
  });
  const cloud = manifest.packageVersion === 2 ? pointCloudEntry(source, entries, hashes) : undefined;
  const referenced = declared.length + (cloud ? 1 : 0);
  if (entries.size !== referenced + 1 || Object.keys(hashes).length !== referenced)
    throw new Error('The package contains missing or unreferenced materials.');
  const assets: T[] = [];
  for (const { id, contentType, entry, sha256: expected } of declared) {
    const bytes = await readEntry(archive, entry);
    if (await sha256(bytes) !== expected) throw new Error(`Material checksum failed: ${id}. Export the package again.`);
    checkMaterial(bytes, contentType);
    assets.push(await keep({ id, contentType, bytes, sha256: expected }));
  }
  if (!cloud) return { source, assets };
  const bytes = await readEntry(archive, cloud.entry);
  if (await sha256(bytes) !== cloud.sha256) throw new Error('Point cloud checksum failed. Export the package again.');
  return { source, assets, pointCloud: { contentPath: cloud.entry.name, fileName: cloud.fileName, sha256: cloud.sha256, bytes } };
}

/** The point cloud `match.field.pointCloud` names, checked against its byte length and the manifest's SHA-256. */
function pointCloudEntry(source: Record<string, unknown>, entries: Map<string, ZipEntry>, hashes: Record<string, unknown>) {
  const raw = object(source.field).pointCloud;
  if (raw === undefined || raw === null) return undefined;
  const cloud = object(raw), path = cloud.contentPath;
  if (typeof path !== 'string' || !/^point-cloud\/[A-Za-z0-9][A-Za-z0-9_-]{0,63}\.ply$/.test(path)) throw new Error('Invalid offline point cloud path.');
  const entry = entries.get(path), expected = hashes[path];
  if (!entry || typeof cloud.fileName !== 'string' || !cloud.fileName || cloud.fileName.length > 255
    || cloud.byteLength !== entry.size
    || typeof expected !== 'string' || !/^[a-f0-9]{64}$/.test(expected)) throw new Error('The offline point cloud is missing or has invalid metadata.');
  return { entry, fileName: cloud.fileName, sha256: expected };
}
