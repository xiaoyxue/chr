import { validateModelPose } from '../placements/model-motion.ts';
import type { ArtworkRotation, FusionAdapter, FusionTifo, Match, Placement, FusionAsset } from '../shared/types.ts';
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
/**
 * A 2D placement's `assetOpacity` (#188): a number from 0 to 1, 1 when an older manifest
 * lacks it. Percentages such as 35 are refused, not clamped.
 */
export function assetOpacity(value: unknown, placementId: string): number {
  if (value === undefined) return 1;
  if (typeof value !== 'number' || !Number.isFinite(value) || value < 0 || value > 1)
    throw new Error(`Placement ${placementId}: assetOpacity must be a number from 0 to 1 (35% is 0.35), not ${JSON.stringify(value)}.`);
  return value;
}
function array(value: unknown): unknown[] {
  if (!Array.isArray(value)) throw new Error('Invalid Fusion response list.');
  return value;
}

/** What schema version 2 states explicitly about the coordinates that version 1 implied. */
const V2_COORDINATES: Record<string, string> = { z: 'down', swingSpace: 'field', motionSpace: 'field',
  motionOrigin: 'neutral-model-bottom-center', motionScale: 'neutral-pose-multiplier' };

/**
 * A Tifo placement (#210): one PNG / JPEG on the rectangle of four point-cloud corners,
 * opaque, revealed from its centre line. validateMatch checks its geometry.
 */
function mapTifo(raw: unknown, adId: string, adName: string, assets: Map<string, FusionAsset>): FusionTifo {
  const source = object(raw), sourceId = id(source.id);
  try {
    if (source.coordinateSpace !== 'point-cloud') throw new Error('coordinateSpace must be "point-cloud".');
    const assetIds = array(source.assetIds).map(id), asset = assetIds.length === 1 ? assets.get(assetIds[0]) : undefined;
    if (!asset || !['image/png', 'image/jpeg'].includes(asset.contentType)) throw new Error('needs exactly one PNG or JPEG image.');
    if (source.assetOpacity !== undefined && source.assetOpacity !== 1)
      throw new Error(`assetOpacity ${JSON.stringify(source.assetOpacity)} is not supported: chr flags are opaque, so it must be 1 or absent.`);
    const reveal = object(source.tifoReveal);
    if (reveal.direction !== 'center-horizontal') throw new Error(`unsupported tifoReveal direction ${JSON.stringify(reveal.direction)}.`);
    const corners = array(source.corners).map((corner): [number, number, number] => {
      const point = object(corner);
      return [number(point.x), number(point.y), number(point.z)];
    });
    return { id: sourceId, name: text(source.name), adId, adName, asset, corners, width: number(source.width), height: number(source.height),
      assetRotation: number(source.assetRotation) as ArtworkRotation, reveal: { direction: 'center_horizontal', duration: number(reveal.duration) } };
  } catch (error) { throw new Error(`Tifo placement ${sourceId}: ${(error as Error).message}`); }
}

/**
 * The match a Fusion manifest describes. `pointCloudSha256` is the SHA-256 of an
 * offline package's point cloud, the cloud its Tifo placements are drawn on.
 */
export function mapFusionMatch(value: unknown, requestedId: string, offlinePackageId?: string, pointCloudSha256?: string): Match {
  const data = object(value), field = object(data.field), coordinates = object(data.coordinates);
  const matchId = id(data.id), campaignId = id(data.campaignId);
  if (![matchId, campaignId].includes(requestedId)) throw new Error('Fusion returned a different match.');
  if ((data.schemaVersion !== 1 && data.schemaVersion !== 2) || data.sport !== 'football' || coordinates.unit !== 'meters'
    || coordinates.origin !== 'field-center' || coordinates.x !== 'right' || coordinates.y !== 'down-in-top-view'
    || coordinates.modelUp !== 'gltf-y-up'
    || (data.schemaVersion === 2 && Object.entries(V2_COORDINATES).some(([key, expected]) => coordinates[key] !== expected)))
    throw new Error('Unsupported Fusion sport or coordinate convention.');
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
  const placements: Placement[] = [], tifos: FusionTifo[] = [];
  const adIds = new Set<string>();
  for (const rawAd of array(data.ads)) {
    const ad = object(rawAd), adId = id(ad.id), adName = text(ad.name);
    if (adIds.has(adId)) throw new Error('Duplicate Fusion Ad identifier.');
    adIds.add(adId);
    if (ad.type === 'Tifo') {
      tifos.push(...array(ad.placements).map((raw) => mapTifo(raw, adId, adName, assets)));
      continue;
    }
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
        placements.push({ ...common, type: '2D', sizeX: width, sizeY: height, assetRotation: rotation,
          assetOpacity: assetOpacity(source.assetOpacity, sourceId) });
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
    field: { length: number(field.length), width: number(field.width) }, placements,
    ...(pointCloudSha256 ? { pointCloud: { sha256: pointCloudSha256, fileName: text(object(field.pointCloud).fileName) } } : {}),
    ...(tifos.length ? { tifos } : {}) });
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
