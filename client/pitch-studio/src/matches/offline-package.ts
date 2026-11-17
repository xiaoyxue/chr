import { unzip } from 'fflate';
import { mapFusionMatch } from './fusion-client.ts';
import { blendedMaterials } from '../render/render-description.ts';
import { readServerFile, uploadFile } from '../shared/studio-store.ts';
import type { FusionAsset, Match } from '../shared/types.ts';

const MAX_MATERIAL_BYTES = 256 * 1024 * 1024;
const MAX_MANIFEST_BYTES = 1024 * 1024;
const EXTENSIONS: Record<string, string> = {
  'image/png': '.png', 'image/jpeg': '.jpg', 'image/webp': '.webp', 'model/gltf-binary': '.glb',
};
const offlinePath = /^\/fusion-offline\/packages\/([A-Za-z0-9][A-Za-z0-9_-]{0,63})\/assets\/([A-Za-z0-9][A-Za-z0-9_-]{0,63})$/;
export interface OfflinePackage { id: string; match: Match; assets: Map<string, Blob> }

function object(value: unknown): Record<string, unknown> {
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

export async function parseOfflinePackage(file: Blob): Promise<OfflinePackage> {
  if (!file.size || file.size > MAX_MATERIAL_BYTES + 4 * MAX_MANIFEST_BYTES)
    throw new Error('Select a nonempty Fusion ZIP package up to 260 MB (256 MB of materials).');
  const names = new Set<string>();
  let total = 0;
  const bytes = new Uint8Array(await file.arrayBuffer());
  const files = await new Promise<Record<string, Uint8Array>>((resolve, reject) => {
    unzip(bytes, { filter: (entry) => {
      if (names.has(entry.name) || names.size >= 1025
        || !/^(?:manifest\.json|assets\/[A-Za-z0-9][A-Za-z0-9_-]{0,63}\.(?:png|jpg|webp|glb))$/.test(entry.name))
        throw new Error('The offline ZIP contains duplicate or unexpected files.');
      names.add(entry.name);
      const maximum = entry.name === 'manifest.json' ? MAX_MANIFEST_BYTES
        : entry.name.endsWith('.glb') ? MAX_MATERIAL_BYTES : 4 * 1024 * 1024;
      if (!Number.isSafeInteger(entry.originalSize) || entry.originalSize <= 0 || entry.originalSize > maximum
        || (total += entry.originalSize) > MAX_MATERIAL_BYTES + MAX_MANIFEST_BYTES)
        throw new Error('The offline package exceeds its material size limits.');
      return true;
    } }, (error, result) => error ? reject(error) : resolve(result));
  });
  if (!files['manifest.json']) throw new Error('The offline package is missing manifest.json.');
  const manifest = object(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(files['manifest.json'])));
  if (manifest.packageVersion !== 1) throw new Error('Unsupported offline package version.');
  const source = object(manifest.match), hashes = object(manifest.sha256);
  if (typeof source.id !== 'string' || !Array.isArray(source.assets)) throw new Error('Invalid offline match configuration.');
  const id = crypto.randomUUID();
  const match = mapFusionMatch(source, source.id, id);
  const assigned = new Set(match.placements.flatMap((slot) => slot.asset ? [slot.asset.id] : []));
  const assets = new Map<string, Blob>();
  for (const raw of source.assets) {
    const asset = object(raw);
    if (typeof asset.id !== 'string' || typeof asset.contentType !== 'string') throw new Error('Invalid offline asset.');
    const path = `assets/${asset.id}${EXTENSIONS[asset.contentType]}`;
    const bytes = files[path], expected = hashes[path];
    if (asset.contentPath !== path || !assigned.has(asset.id) || !bytes
      || !Number.isSafeInteger(asset.byteLength) || asset.byteLength !== bytes.length
      || typeof expected !== 'string' || !/^[a-f0-9]{64}$/.test(expected)) throw new Error('An offline asset is missing or has invalid metadata.');
    const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', new Uint8Array(bytes).buffer));
    if (Array.from(hash, (byte) => byte.toString(16).padStart(2, '0')).join('') !== expected)
      throw new Error(`Material checksum failed: ${asset.id}. Export the package again.`);
    checkMaterial(bytes, asset.contentType);
    assets.set(asset.id, new Blob([new Uint8Array(bytes).buffer], { type: asset.contentType }));
  }
  if (assets.size !== assigned.size || Object.keys(files).length !== assets.size + 1
    || Object.keys(hashes).length !== assets.size) throw new Error('The package contains missing or unreferenced materials.');
  return { id, match, assets };
}

/** The stored file of an offline package's material, from the match library; set by the page. */
let storedAsset: (packageId: string, assetId: string) => string | undefined = () => undefined;
export function setOfflineAssetFiles(lookup: typeof storedAsset) { storedAsset = lookup; }

/**
 * Stores each material of a parsed package on the server, content-addressed, so a stored
 * file's name is the SHA-256 the package declared; also notes each GLB's alpha-blended
 * materials, so a send needs neither file again.
 */
export async function storeOfflineAssets(packageData: OfflinePackage) {
  const assetFiles: Record<string, string> = {}, assetBlended: Record<string, string[]> = {};
  for (const [id, blob] of packageData.assets) {
    assetFiles[id] = await uploadFile(blob, `${id}${EXTENSIONS[blob.type]}`);
    if (blob.type === 'model/gltf-binary') assetBlended[id] = blendedMaterials(new Uint8Array(await blob.arrayBuffer()));
  }
  return { assetFiles, assetBlended };
}

/** An offline match's material, read back from the studio store. */
async function offlineAsset(asset: FusionAsset): Promise<Blob> {
  const path = offlinePath.exec(asset.url);
  if (!path || path[2] !== asset.id) throw new Error('Invalid offline material reference.');
  const file = storedAsset(path[1], path[2]);
  if (!file) throw new Error('离线素材不在服务器存储中：请重新导入原始 Fusion ZIP。');
  const blob = await readServerFile(file, asset.name, asset.contentType);
  if (!blob.size) throw new Error('An offline material is missing or damaged. Import the original Fusion ZIP again.');
  return blob;
}

export async function resolveFusionAsset(asset: FusionAsset): Promise<{ url: string; release: () => void }> {
  if (asset.url.startsWith('/fusion-api/matches/')) return { url: asset.url, release: () => {} };
  const url = URL.createObjectURL(await offlineAsset(asset));
  return { url, release: () => URL.revokeObjectURL(url) };
}
