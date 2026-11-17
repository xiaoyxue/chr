import { validateModelPose } from '../placements/model-motion.ts';
import type { FusionAdapter, Match, Placement, FusionAsset } from '../shared/types.ts';
import { validateMatch } from './match-domain.ts';

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid Fusion response object.');
  return value as Record<string, unknown>;
}
function text(value: unknown): string {
  if (typeof value !== 'string') throw new Error('Invalid Fusion response text.');
  return value;
}
function id(value: unknown): string {
  const result = text(value);
  if (!/^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(result)) throw new Error('Invalid Fusion identifier.');
  return result;
}
function number(value: unknown): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) throw new Error('Invalid Fusion coordinate.');
  return value;
}
function array(value: unknown): unknown[] {
  if (!Array.isArray(value)) throw new Error('Invalid Fusion response list.');
  return value;
}

export function mapFusionMatch(value: unknown, requestedId: string, offlinePackageId?: string): Match {
  const data = object(value), field = object(data.field), coordinates = object(data.coordinates);
  const matchId = id(data.id), campaignId = id(data.campaignId);
  if (![matchId, campaignId].includes(requestedId)) throw new Error('Fusion returned a different match.');
  if (data.schemaVersion !== 1 || data.sport !== 'football' || coordinates.unit !== 'meters'
    || coordinates.origin !== 'field-center' || coordinates.x !== 'right' || coordinates.y !== 'down-in-top-view'
    || coordinates.modelUp !== 'gltf-y-up') throw new Error('Unsupported Fusion sport or coordinate convention.');
  const assets = new Map<string, FusionAsset>();
  for (const raw of array(data.assets)) {
    const asset = object(raw), assetId = id(asset.id);
    if (assets.has(assetId)) throw new Error('Duplicate Fusion asset identifier.');
    const contentType = text(asset.contentType);
    if (!['image/png', 'image/jpeg', 'image/webp', 'model/gltf-binary'].includes(contentType))
      throw new Error('Unsupported Fusion material format.');
    assets.set(assetId, { id: assetId, name: text(asset.name), contentType,
      url: offlinePackageId ? `/fusion-offline/packages/${offlinePackageId}/assets/${assetId}`
        : `/fusion-api/matches/${matchId}/assets/${assetId}` });
  }
  const placements: Placement[] = [];
  const adIds = new Set<string>();
  for (const rawAd of array(data.ads)) {
    const ad = object(rawAd), adId = id(ad.id), adName = text(ad.name);
    if (adIds.has(adId)) throw new Error('Duplicate Fusion Ad identifier.');
    adIds.add(adId);
    if (ad.type !== '2D' && ad.type !== '3D') throw new Error('Unsupported Fusion Ad type.');
    for (const raw of array(ad.placements)) {
      const source = object(raw), sourceId = id(source.id);
      if (source.coordinateSpace !== 'pitch') throw new Error(`Placement ${sourceId} uses a legacy canvas. Configure a field placement in Fusion first.`);
      const assetIds = array(source.assetIds).map(id);
      if (assetIds.length > 1) throw new Error(`Placement ${sourceId} has multiple legacy assets. Assign one asset in Fusion.`);
      const asset = assetIds.length ? assets.get(assetIds[0]) : undefined;
      if (assetIds.length && !asset) throw new Error(`Assigned asset for ${sourceId} is missing.`);
      if (asset && (asset.contentType === 'model/gltf-binary') !== (ad.type === '3D'))
        throw new Error('Fusion placement material does not match its Ad type.');
      const common = { id: sourceId, name: text(source.name), adId, adName, asset,
        x: number(source.x), y: number(source.y), z: 0, rx: 0, ry: 0, rz: 0 };
      const width = number(source.width), height = number(source.height);
      if (width <= 0 || height <= 0) throw new Error('Invalid Fusion placement dimensions.');
      if (ad.type === '2D') {
        const rotation = number(source.assetRotation);
        if (![0, 90, 180, 270].includes(rotation)) throw new Error('Invalid image rotation.');
        placements.push({ ...common, type: '2D', sizeX: width, sizeY: height, assetRotation: rotation });
      } else {
        let modelPose;
        try { modelPose = validateModelPose(source.modelPose); }
        catch (error) { throw new Error(`Placement ${sourceId}: ${(error as Error).message}`); }
        placements.push({ ...common, type: '3D', region: { width, height }, modelPose, scaleX: 1, scaleY: 1, scaleZ: 1 });
      }
    }
  }
  return validateMatch({ id: requestedId, name: text(data.name), date: text(data.date),
    venue: text(data.venue) || text(field.name), venueId: id(field.id),
    mode: offlinePackageId ? 'offline' : 'api', offlinePackageId,
    field: { length: number(field.length), width: number(field.width) }, placements });
}

export class HttpFusionAdapter implements FusionAdapter {
  async lookupMatch(matchId: string): Promise<Match> {
    if (!/^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(matchId)) throw new Error('Enter a valid MATCH or CMP identifier.');
    const response = await fetch(`/fusion-api/matches/${matchId}`, { cache: 'no-store', signal: AbortSignal.timeout(120000) });
    if (!response.ok) {
      const error: unknown = await response.json();
      throw new Error(text(object(error).error));
    }
    return mapFusionMatch(await response.json(), matchId);
  }
  async submitScan(): Promise<{ taskId: string }> { throw new Error('Scan submission is not connected. No video was uploaded.'); }
  async getAnalysis(): Promise<unknown> { throw new Error('Analysis retrieval is not connected.'); }
  async savePlacements(): Promise<unknown> { throw new Error('This integration is read-only. Save local drafts instead.'); }
}
