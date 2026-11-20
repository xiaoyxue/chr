import { mapFusionMatch } from './fusion-client.ts';
import { EXTENSIONS, blendedMaterials, readOfflinePackage } from './offline-reader.ts';
import { importStoredPackage, storedFileUrl, uploadFile } from '../shared/studio-store.ts';
import type { FusionAsset, Match } from '../shared/types.ts';

const offlinePath = /^\/fusion-offline\/packages\/([A-Za-z0-9][A-Za-z0-9_-]{0,63})\/assets\/([A-Za-z0-9][A-Za-z0-9_-]{0,63})$/;
export interface OfflinePackage { id: string; match: Match; assets: Map<string, Blob> }
/** A package whose materials are already in the studio store, by asset ID. */
export interface StoredPackage { id: string; match: Match; assetFiles: Record<string, string>; assetBlended: Record<string, string[]> }

/** The match of a package's manifest, under a new package ID; each material must serve a placement. */
function packageMatch(source: Record<string, unknown>, assetIds: string[]) {
  const id = crypto.randomUUID();
  const match = mapFusionMatch(source, source.id as string, id);
  const assigned = new Set(match.placements.flatMap((slot) => slot.asset ? [slot.asset.id] : []));
  if (assetIds.length !== assigned.size || assetIds.some((asset) => !assigned.has(asset)))
    throw new Error('The package contains missing or unreferenced materials.');
  return { id, match };
}

/** A ZIP chosen on this computer: read and verified here, its materials stored by storeOfflineAssets. */
export async function parseOfflinePackage(file: Blob): Promise<OfflinePackage> {
  const { source, assets } = await readOfflinePackage(new Uint8Array(await file.arrayBuffer()));
  const { id, match } = packageMatch(source, assets.map((asset) => asset.id));
  return { id, match, assets: new Map(assets.map((asset) => [asset.id, new Blob([new Uint8Array(asset.bytes).buffer], { type: asset.contentType })])) };
}

/**
 * A ZIP on the onsite server: the page server reads, verifies and stores it (the same
 * checks as parseOfflinePackage) and returns only its manifest, so no material crosses
 * the network.
 */
export async function importServerPackage(path: string): Promise<StoredPackage> {
  const { source, assets } = await importStoredPackage(path);
  const { id, match } = packageMatch(source, assets.map((asset) => asset.id));
  return {
    id, match,
    assetFiles: Object.fromEntries(assets.map((asset) => [asset.id, asset.path])),
    assetBlended: Object.fromEntries(assets.flatMap((asset) => asset.blended ? [[asset.id, asset.blended]] : [])),
  };
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

/**
 * Where the page loads an offline match's material: its file in the studio store,
 * which the browser caches (stored files never change), so a preview downloads it once.
 */
export async function resolveFusionAsset(asset: FusionAsset): Promise<{ url: string; cached: boolean; release: () => void }> {
  if (asset.url.startsWith('/fusion-api/matches/')) return { url: asset.url, cached: false, release: () => {} };
  const path = offlinePath.exec(asset.url);
  if (!path || path[2] !== asset.id) throw new Error('Invalid offline material reference.');
  const file = storedAsset(path[1], path[2]);
  if (!file) throw new Error('离线素材不在服务器存储中：请重新导入原始 Fusion ZIP。');
  return { url: storedFileUrl(file), cached: true, release: () => {} };
}
