// What a render description needs to know about each material: its SHA-256 and, for a GLB,
// its alpha-blended materials and (for local placements) its bounds. The studio notes these
// when a material is imported or chosen, so building a description downloads no material;
// a file is read only for a record from before this was noted, or one changed on disk since.
import type { LocalAd } from '../placements/local-ad.ts';
import type { Placement } from '../shared/types.ts';
import { blendedMaterials, type DescribedAsset, type DescribedLocal, type GlbBounds } from './render-description.ts';

export const MATERIAL_ENTRY: Record<string, { kind: DescribedAsset['kind']; extension: string }> = {
  'model/gltf-binary': { kind: 'glb', extension: '.glb' },
  'image/png': { kind: 'image_quad', extension: '.png' },
  'image/jpeg': { kind: 'image_quad', extension: '.jpg' },
};

export interface MaterialAccess {
  /** The SHA-256 of the file as the server holds it now, without downloading it. */
  sha256(path: string): Promise<string>;
  read(path: string): Promise<File>;
  bounds(glb: File): Promise<GlbBounds>;
}

async function glbFacts(file: File, access: Pick<MaterialAccess, 'bounds'>) {
  return { bounds: await access.bounds(file), blended: blendedMaterials(new Uint8Array(await file.arrayBuffer())) };
}

/** A local material's facts to note in its placement, from the file itself. */
export async function materialFacts(type: LocalAd['type'], file: File, sha256: string,
  access: Pick<MaterialAccess, 'bounds'>): Promise<Pick<LocalAd, 'materialSha256' | 'materialBounds' | 'materialBlended'>> {
  if (type !== '3D') return { materialSha256: sha256 };
  const { bounds, blended } = await glbFacts(file, access);
  return { materialSha256: sha256, materialBounds: bounds, materialBlended: blended };
}

const maskNote = (name: string, blended: string[]) => blended.length ? [`${name}（${blended.join('、')}）`] : [];

/**
 * The Fusion materials of `slots`, from the offline match's stored files (named by their
 * hash) and the blended materials noted at import. `assetBlended` comes back completed
 * when an older record lacked an entry, so the caller can save it.
 */
export async function fusionAssets(slots: Placement[], assetFiles: Record<string, string>, noted: Record<string, string[]> | undefined,
  access: MaterialAccess) {
  const assets = new Map<string, DescribedAsset>(), masked: string[] = [], assetBlended = { ...noted };
  let completed = false;
  for (const { asset } of slots) {
    if (!asset || assets.has(asset.id)) continue;
    const entry = MATERIAL_ENTRY[asset.contentType];
    if (!entry) throw new Error(`The renderer cannot draw ${asset.contentType} material ${asset.name}.`);
    const path = assetFiles[asset.id];
    if (!path) throw new Error(`离线素材 ${asset.name} 不在服务器存储中：请重新导入原始 Fusion ZIP。`);
    if (entry.kind === 'glb' && !assetBlended[asset.id]) {
      assetBlended[asset.id] = blendedMaterials(new Uint8Array(await (await access.read(path)).arrayBuffer()));
      completed = true;
    }
    const blended = entry.kind === 'glb' ? assetBlended[asset.id] : [];
    masked.push(...maskNote(asset.name, blended));
    assets.set(asset.id, { kind: entry.kind, entry: `assets/${asset.id}${entry.extension}`, sha256: await access.sha256(path),
      ...(blended.length ? { blendAs: 'mask' as const } : {}) });
  }
  return { assets, masked, assetBlended: completed ? assetBlended : undefined };
}

/**
 * Local placements with their materials described: each server file's current hash, and
 * for a GLB the noted bounds and blended materials while the hash still matches the noted one.
 */
export async function localMaterials(ads: LocalAd[], access: MaterialAccess) {
  const locals: DescribedLocal[] = [], masked: string[] = [];
  const hashes = new Map<string, Promise<string>>(), facts = new Map<string, Promise<{ bounds: GlbBounds; blended: string[] }>>();
  for (const ad of ads) {
    try {
      if (!hashes.has(ad.materialPath)) hashes.set(ad.materialPath, access.sha256(ad.materialPath));
      const sha256 = await hashes.get(ad.materialPath);
      if (ad.type !== '3D') { locals.push({ ad, sha256 }); continue; }
      let found: { bounds: GlbBounds; blended: string[] };
      if (ad.materialSha256 === sha256 && ad.materialBounds && ad.materialBlended) found = { bounds: ad.materialBounds, blended: ad.materialBlended };
      else {
        const key = `${sha256}:${ad.materialPath}`;
        if (!facts.has(key)) facts.set(key, access.read(ad.materialPath).then((file) => glbFacts(file, access)));
        found = await facts.get(key);
      }
      masked.push(...maskNote(ad.name, found.blended));
      locals.push({ ad, sha256, bounds: found.bounds, ...(found.blended.length ? { blendAs: 'mask' as const } : {}) });
    } catch (error) {
      throw new Error(`读取“${ad.name}”的素材 ${ad.materialPath} 失败：${error instanceof Error ? error.message : String(error)}`);
    }
  }
  return { locals, masked };
}
