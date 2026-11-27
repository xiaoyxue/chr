import { validateDimensions } from '../scene/pitch/dimensions.ts';
import { isPlacementInside } from '../scene/pitch/pitch-layout.ts';
import { validateModelPose } from '../placements/model-motion.ts';
import { isRevealDirection, validRevealDuration } from '../placements/reveal.ts';
import { rectangleProblem } from '../placements/tifo.ts';
import type { FusionAdapter, FusionTifo, Match, MatchRecord, Placement, FieldDimensions, FusionAsset } from '../shared/types.ts';

export const MATCH_STORAGE_KEY = 'liveshow.onsite.matches.v1';

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function hasNumbers<K extends string>(value: Record<string, unknown>, keys: K[]): value is Record<string, unknown> & Record<K, number> {
  return keys.every((key) => typeof value[key] === 'number' && Number.isFinite(value[key]));
}

export function validatePlacement(slot: unknown, field: FieldDimensions): Placement {
  if (!isRecord(slot) || typeof slot.id !== 'string' || !slot.id || typeof slot.name !== 'string' || (slot.type !== '2D' && slot.type !== '3D')) throw new Error('广告位标识或类型无效。');
  const { id, name, type } = slot;
  if (!hasNumbers(slot, ['x', 'y', 'z', 'rx', 'ry', 'rz'])) throw new Error('位置和旋转必须是有效数字。');
  if (Math.abs(slot.x) > field.length / 2 || Math.abs(slot.y) > field.width / 2 || Math.abs(slot.z) > 30
    || Math.abs(slot.rx) > 180 || Math.abs(slot.ry) > 180 || Math.abs(slot.rz) > 180) throw new Error('位置超出球场，或高度 / 旋转超出可调整范围。');
  let asset: FusionAsset;
  if (slot.asset !== undefined) {
    const source = slot.asset;
    if (!isRecord(source) || typeof source.id !== 'string' || typeof source.name !== 'string'
      || typeof source.url !== 'string'
      || !/^\/(?:fusion-api\/matches|fusion-offline\/packages)\/[A-Za-z0-9][A-Za-z0-9_-]{0,63}\/assets\/[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(source.url)
      || source.url.split('/').pop() !== source.id
      || typeof source.contentType !== 'string'
      || !(type === '3D' ? source.contentType === 'model/gltf-binary' : ['image/png', 'image/jpeg', 'image/webp'].includes(source.contentType)))
      throw new Error('Invalid Fusion material reference.');
    asset = { id: source.id, name: source.name, url: source.url, contentType: source.contentType };
  }
  if ((slot.adId !== undefined && typeof slot.adId !== 'string') || (slot.adName !== undefined && typeof slot.adName !== 'string')
    || (slot.assetRotation !== undefined && (typeof slot.assetRotation !== 'number' || ![0, 90, 180, 270].includes(slot.assetRotation))))
    throw new Error('Invalid Fusion placement metadata.');
  const pose = { x: slot.x, y: slot.y, z: slot.z, rx: slot.rx, ry: slot.ry, rz: slot.rz, asset,
    adId: typeof slot.adId === 'string' ? slot.adId : undefined, adName: typeof slot.adName === 'string' ? slot.adName : undefined,
    assetRotation: typeof slot.assetRotation === 'number' ? slot.assetRotation : undefined };
  if (type === '2D') {
    if (!hasNumbers(slot, ['sizeX', 'sizeY']) || slot.z !== 0 || slot.rx !== 0 || slot.ry !== 0 || slot.sizeX < 0.1 || slot.sizeY < 0.1 || !isPlacementInside(slot, field)) throw new Error('2D 广告必须贴地，尺寸至少 0.1 m，旋转后的完整区域也必须位于球场内。');
    // Records stored before #188 have no opacity: Fusion's default is 1.
    const assetOpacity = slot.assetOpacity === undefined ? 1 : slot.assetOpacity;
    if (typeof assetOpacity !== 'number' || !(assetOpacity >= 0 && assetOpacity <= 1))
      throw new Error('不透明度必须是 0 到 1 之间的数（35% 即 0.35）。');
    return { ...pose, id, name, type, sizeX: slot.sizeX, sizeY: slot.sizeY, assetOpacity };
  }
  const { assetRole } = slot;
  if (!hasNumbers(slot, ['scaleX', 'scaleY', 'scaleZ'])
    || ![slot.scaleX, slot.scaleY, slot.scaleZ].every((value) => value >= 0.1 && value <= 5)) {
    throw new Error('3D 素材或缩放无效，缩放范围为 0.1–5 倍。');
  }
  const scales = { scaleX: slot.scaleX, scaleY: slot.scaleY, scaleZ: slot.scaleZ };
  if (slot.region !== undefined || slot.modelPose !== undefined) {
    const region = slot.region, modelPose = slot.modelPose;
    if (!isRecord(region) || !hasNumbers(region, ['width', 'height']) || region.width <= 0 || region.height <= 0
      || slot.scaleX !== slot.scaleY || slot.scaleX !== slot.scaleZ
      || slot.z !== 0 || slot.rx !== 0 || slot.ry !== 0
      || !isPlacementInside({ x: slot.x, y: slot.y, sizeX: region.width, sizeY: region.height }, field))
      throw new Error('Invalid bounded Fusion model pose or region.');
    return { ...pose, ...scales, id, name, type, region: { width: region.width, height: region.height },
      modelPose: validateModelPose(modelPose) };
  }
  if (assetRole !== 'car' && assetRole !== 'bank-left' && assetRole !== 'bank-right') throw new Error('Invalid model role.');
  return { ...pose, ...scales, id, name, type, assetRole };
}

const SHA256 = /^[0-9a-f]{64}$/;

/** A Tifo placement of an offline package: its image, rectangle, artwork rotation and reveal. */
function validateTifo(tifo: unknown, offlinePackageId: string): FusionTifo {
  if (!isRecord(tifo) || typeof tifo.id !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(tifo.id)) throw new Error('Invalid Tifo placement.');
  const fail = (text: string) => new Error(`Tifo placement ${tifo.id}: ${text}`);
  const { asset, corners, width, height, assetRotation, reveal } = tifo;
  if (typeof tifo.name !== 'string' || typeof tifo.adId !== 'string' || typeof tifo.adName !== 'string') throw fail('invalid name or Ad.');
  if (!isRecord(asset) || typeof asset.id !== 'string' || typeof asset.name !== 'string' || !['image/png', 'image/jpeg'].includes(asset.contentType as string)
    || asset.url !== `/fusion-offline/packages/${offlinePackageId}/assets/${asset.id}`) throw fail('needs one PNG or JPEG image of its package.');
  if (!Array.isArray(corners) || corners.length !== 4
    || !corners.every((corner) => Array.isArray(corner) && corner.length === 3 && corner.every(Number.isFinite))) throw fail('needs four corners.');
  if (typeof width !== 'number' || typeof height !== 'number' || !(width >= 0.01 && width <= 1000 && height >= 0.01 && height <= 1000))
    throw fail('width and height must be 0.01 to 1000 m.');
  const problem = rectangleProblem(corners as [number, number, number][], width, height);
  if (problem) throw fail(problem);
  if (typeof assetRotation !== 'number' || ![0, 90, 180, 270].includes(assetRotation)) throw fail('assetRotation must be 0, 90, 180 or 270.');
  if (reveal !== undefined && (!isRecord(reveal) || !isRevealDirection(reveal.direction) || !validRevealDuration(reveal.duration)))
    throw fail('tifoReveal needs a known direction and a finite duration over 0 s.');
  const checked = reveal as FusionTifo['reveal'];
  return { id: tifo.id, name: tifo.name, adId: tifo.adId, adName: tifo.adName,
    asset: { id: asset.id, name: asset.name, url: asset.url, contentType: asset.contentType as string },
    corners: corners.map((corner) => [...corner]) as FusionTifo['corners'], width, height, assetRotation: assetRotation as FusionTifo['assetRotation'],
    ...(checked ? { reveal: { direction: checked.direction, duration: checked.duration } } : {}) };
}

export function validateMatch(match: unknown): Match {
  if (!isRecord(match) || typeof match.id !== 'string' || !match.id.trim() || match.id.length > 80
    || typeof match.name !== 'string' || !match.name.trim()
    || typeof match.date !== 'string' || !match.date.trim()
    || typeof match.venue !== 'string' || !match.venue.trim()
    || typeof match.venueId !== 'string' || !match.venueId.trim()
    || (match.mode !== 'api' && match.mode !== 'offline') || !Array.isArray(match.placements)) throw new Error('场次数据不完整，无法打开。');
  if (match.mode === 'offline' && (typeof match.offlinePackageId !== 'string'
    || !/^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(match.offlinePackageId))) throw new Error('Invalid offline package identifier.');
  if (!isRecord(match.field) || !hasNumbers(match.field, ['length', 'width'])) throw new Error('请输入有效的球场长、宽。');
  const field = validateDimensions(match.field.length, match.field.width);
  const slots = match.placements.map((slot) => validatePlacement(slot, field));
  if (slots.some((slot) => slot.asset && (match.mode === 'offline'
    ? !slot.asset.url.startsWith(`/fusion-offline/packages/${match.offlinePackageId}/assets/`)
    : !slot.asset.url.startsWith('/fusion-api/matches/')))) throw new Error('Material source does not match the match configuration.');
  if (new Set(slots.map((slot) => slot.id)).size !== slots.length
    || new Set(slots.filter((slot) => slot.type === '3D' && slot.assetRole).map((slot) => slot.assetRole)).size !== slots.filter((slot) => slot.type === '3D' && slot.assetRole).length) throw new Error('场次广告位 ID 或模型角色重复。');
  let pointCloud: Match['pointCloud'];
  if (match.pointCloud !== undefined) {
    const { sha256, fileName } = isRecord(match.pointCloud) ? match.pointCloud : {} as Record<string, unknown>;
    if (match.mode !== 'offline' || typeof sha256 !== 'string' || !SHA256.test(sha256) || typeof fileName !== 'string' || !fileName || fileName.length > 255)
      throw new Error('Invalid offline point cloud.');
    pointCloud = { sha256, fileName };
  }
  let tifos: FusionTifo[] | undefined;
  if (match.tifos !== undefined) {
    if (!Array.isArray(match.tifos) || !pointCloud) throw new Error("Tifo placements need the offline package's point cloud.");
    tifos = match.tifos.map((tifo) => validateTifo(tifo, match.offlinePackageId as string));
    if (new Set(tifos.map((tifo) => tifo.id)).size !== tifos.length) throw new Error('Duplicate Tifo placement identifier.');
  }
  return { ...match, id: match.id, name: match.name, date: match.date, venue: match.venue, venueId: match.venueId,
    mode: match.mode, field, placements: slots,
    ...(pointCloud ? { pointCloud } : {}),
    ...(tifos ? { tifos } : {}) };
}

export function validateScanFile<T extends Pick<File, 'name' | 'size'>>(file: T): T {
  if (!file || !Number.isFinite(file.size) || file.size <= 0 || typeof file.name !== 'string'
    || !/\.(mp4|webm|mov)$/i.test(file.name)) throw new Error('请选择非空的 MP4、WebM 或 MOV 视频文件。');
  return file;
}

// Future HTTP adapters normalize their responses here, not in the page or renderer.
export class FusionGateway {
  constructor(private readonly adapter: FusionAdapter | null = null) {}
  get connected() { return Boolean(this.adapter); }
  async lookupMatch(id: string) {
    const key = id.trim();
    if (!key || key.length > 80) throw new Error('请输入有效的比赛场次 ID。');
    if (!this.adapter) throw new Error('比赛查询接口未接入，无法获取场次配置。');
    const result = await this.adapter.lookupMatch(key);
    const match = validateMatch(result);
    if (match.id !== key) throw new Error('接口返回的比赛 ID 与请求不一致。');
    return match;
  }
  async submitScan({ match, file }: { match: Match; file: File }) {
    validateScanFile(file);
    if (!this.adapter) throw new Error('Fusion 上传接口未接入，视频没有上传，也没有创建分析任务。');
    return this.adapter.submitScan({ matchId: match.id, venueId: match.venueId, file });
  }
  async getAnalysis(taskId: string) {
    if (!this.adapter) throw new Error('Fusion 分析结果接口未接入。');
    return this.adapter.getAnalysis(taskId);
  }
  async savePlacements(match: Match, placements: Placement[]) {
    const validated = placements.map((slot) => validatePlacement(slot, match.field));
    if (!this.adapter) throw new Error('Fusion 保存位置接口未接入；请先保存本地草稿。');
    return this.adapter.savePlacements({ matchId: match.id, placements: validated });
  }
}

/**
 * The match library as stored: `{ version: 1, records }`. Every record's match,
 * draft and enabled state is validated; an offline match also maps each of its
 * materials to the stored file the studio reads it from. Throws on anything else.
 * Demo matches and analyses of earlier versions are dropped.
 */
export function validateLibrary(data: unknown): MatchRecord[] {
  if (!isRecord(data) || data.version !== 1 || !Array.isArray(data.records)) throw new Error('比赛记录版本无效，未覆盖原记录。');
  const retained = data.records.filter((record: unknown) => !(isRecord(record) && isRecord(record.match) && record.match.mode === 'demo'));
  const records = retained.map((record: unknown): MatchRecord => {
    if (!isRecord(record)) throw new Error('比赛记录无效，未覆盖原记录。');
    const match = validateMatch(record.match);
    if (!Array.isArray(record.draft)) throw new Error('广告位草稿与场次配置不一致。');
    const draft = record.draft.map((slot: unknown) => validatePlacement(slot, match.field));
    if (draft.length !== match.placements.length
      || new Set(draft.map((slot) => slot.id)).size !== match.placements.length
      || draft.some((slot) => !match.placements.some((original) => original.id === slot.id && original.type === slot.type && original.assetRole === slot.assetRole))) throw new Error('广告位草稿与场次配置不一致。');
    if (record.analysis != null && (!isRecord(record.analysis) || record.analysis.mode !== 'demo')) throw new Error('分析记录无效，未加载未经验证的相机数据。');
    if (record.updatedAt !== undefined && typeof record.updatedAt !== 'string') throw new Error('草稿保存时间无效，未覆盖原记录。');
    const hiddenPlacementIds: string[] = [];
    if (record.hiddenPlacementIds !== undefined) {
      if (!Array.isArray(record.hiddenPlacementIds)) throw new Error('广告位显示设置无效，未覆盖原记录。');
      for (const id of record.hiddenPlacementIds) {
        if (typeof id !== 'string') throw new Error('广告位显示设置无效，未覆盖原记录。');
        if (match.placements.some((slot) => slot.id === id)) hiddenPlacementIds.push(id);
      }
    }
    const assetFiles = record.assetFiles;
    if (assetFiles !== undefined) {
      const used = new Set([...match.placements.flatMap((slot) => slot.asset ? [slot.asset.id] : []), ...(match.tifos ?? []).map((tifo) => tifo.asset.id)]);
      if (match.mode !== 'offline' || !isRecord(assetFiles)
        || Object.entries(assetFiles).some(([id, path]) => !used.has(id) || typeof path !== 'string' || !path.startsWith('/') || path.split('/').some((part) => part.startsWith('.')))
        || [...used].some((id) => !(id in assetFiles))) throw new Error('离线素材文件记录无效。');
    }
    const assetBlended = record.assetBlended;
    if (assetBlended !== undefined && (!assetFiles || !isRecord(assetBlended) || Object.entries(assetBlended).some(([id, names]) => !(id in (assetFiles as object))
      || !Array.isArray(names) || names.length > 1000 || names.some((name) => typeof name !== 'string' || name.length > 255)))) throw new Error('离线素材的半透明材质记录无效。');
    const { pointCloudFile } = record;
    if (pointCloudFile !== undefined && (!match.pointCloud || typeof pointCloudFile !== 'string' || !pointCloudFile.startsWith('/')
      || pointCloudFile.split('/').some((part) => part.startsWith('.')))) throw new Error('离线点云文件记录无效。');
    return { match, draft, hiddenPlacementIds: [...new Set(hiddenPlacementIds)], ...(pointCloudFile ? { pointCloudFile: pointCloudFile as string } : {}),
      updatedAt: typeof record.updatedAt === 'string' ? record.updatedAt : undefined, ...(assetFiles ? { assetFiles: assetFiles as Record<string, string> } : {}),
      ...(assetBlended ? { assetBlended: assetBlended as Record<string, string[]> } : {}) };
  });
  if (new Set(records.map((record) => record.match.id)).size !== records.length) throw new Error('比赛记录中有重复的场次。');
  return records;
}
