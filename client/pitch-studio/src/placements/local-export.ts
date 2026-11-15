// A match's onsite placements (pitch 2D/3D and point-cloud 2D/3D/flags) as a
// standalone file, for moving them between matches, sessions or studios. It holds
// the placements with their material server paths, each point cloud's identity
// (the PLY's SHA-256) with its same-coordinates answer and, if known, the PLY's
// server path — never any Fusion package data.
import { MAX_LOCAL_ADS, storedLocalAd, upgradeLocalAd, validateStoredLocalAds, withoutPitchFlags, type LocalAd } from './local-ad.ts';

export const LOCAL_PLACEMENTS_FORMAT = 'chr-studio-local-placements-v1';
export interface ExportedCloud { id: string; aligned: boolean; path?: string }
export interface LocalPlacementsFile {
  format: typeof LOCAL_PLACEMENTS_FORMAT;
  matchId: string;
  exportedAt: string;
  placements: LocalAd[];
  clouds: ExportedCloud[];
}
export type ImportMode = 'merge' | 'replace';

const isObject = (value: unknown): value is Record<string, unknown> => Boolean(value) && typeof value === 'object' && !Array.isArray(value);
const SHA256 = /^[0-9a-f]{64}$/;

/** The file for `ads`, with each of their clouds' saved answer and, where known, PLY path. */
export function exportLocalPlacements(matchId: string, ads: readonly LocalAd[], alignment: Readonly<Record<string, boolean>>,
  cloudPaths: Readonly<Record<string, string>> = {}, now = new Date()): LocalPlacementsFile {
  const clouds = [...new Set(ads.flatMap((ad) => ad.space !== 'pitch' && ad.cloudId ? [ad.cloudId] : []))]
    .map((id) => ({ id, aligned: alignment[id] === true, ...(cloudPaths[id] ? { path: cloudPaths[id] } : {}) }));
  return { format: LOCAL_PLACEMENTS_FORMAT, matchId, exportedAt: now.toISOString(), placements: ads.map(storedLocalAd), clouds };
}

function validateClouds(value: unknown): ExportedCloud[] {
  if (!Array.isArray(value) || !value.every((cloud) => isObject(cloud) && typeof cloud.id === 'string' && SHA256.test(cloud.id)
    && typeof cloud.aligned === 'boolean' && (cloud.path === undefined || (typeof cloud.path === 'string' && cloud.path.startsWith('/')))))
    throw new Error('点云记录无效：每个点云需要 PLY 的 SHA-256 和同坐标确认。');
  return value as ExportedCloud[];
}

/** An export file, validated strictly; the store refuses to keep anything else as one. */
export function validateLocalPlacementsFile(data: unknown): LocalPlacementsFile {
  if (!isObject(data) || data.format !== LOCAL_PLACEMENTS_FORMAT || typeof data.matchId !== 'string' || !data.matchId
    || typeof data.exportedAt !== 'string' || Object.keys(data).some((key) => !['format', 'matchId', 'exportedAt', 'placements', 'clouds'].includes(key)))
    throw new Error(`不是现场广告位文件（${LOCAL_PLACEMENTS_FORMAT}）。`);
  return { ...data as unknown as LocalPlacementsFile, placements: validateStoredLocalAds(data.placements), clouds: validateClouds(data.clouds) };
}

/**
 * Placements read from a file: an export file, or the store's own
 * `local-ads.json` (`{ revision, data }`) or its bare list. Anything else,
 * such as a Fusion package or match library, is refused. Pitch flags of earlier
 * versions are dropped and counted.
 */
export function parseLocalPlacements(data: unknown): { placements: LocalAd[]; clouds: ExportedCloud[]; matchId?: string; droppedPitchFlags: number } {
  let file: Pick<LocalPlacementsFile, 'placements' | 'clouds'> & { matchId?: string };
  if (isObject(data) && 'format' in data) file = validateLocalPlacementsFile(data);
  else if (isObject(data) && Number.isSafeInteger(data.revision) && Array.isArray(data.data)) file = { placements: validateStoredLocalAds(data.data), clouds: [] };
  else if (Array.isArray(data)) file = { placements: validateStoredLocalAds(data), clouds: [] };
  else throw new Error(`不是现场广告位文件：需要 ${LOCAL_PLACEMENTS_FORMAT} 导出文件或服务器存储中的 local-ads.json；Fusion 离线包和比赛库不能导入为现场广告位。`);
  const { kept, removed } = withoutPitchFlags(file.placements.map(upgradeLocalAd));
  return { placements: kept, clouds: file.clouds, ...(file.matchId ? { matchId: file.matchId } : {}), droppedPitchFlags: removed };
}

/**
 * The match's placements after an import. Merging replaces the placements with an
 * imported id and keeps the others; replacing keeps only the imported ones.
 */
export function mergeLocalPlacements(existing: readonly LocalAd[], imported: readonly LocalAd[], mode: ImportMode): LocalAd[] {
  const byId = new Map(imported.map((ad) => [ad.id, ad]));
  const next = mode === 'replace' ? [...imported]
    : [...existing.map((ad) => byId.get(ad.id) ?? ad), ...imported.filter((ad) => !existing.some((item) => item.id === ad.id))];
  if (next.length > MAX_LOCAL_ADS) throw new Error(`导入后会超过 ${MAX_LOCAL_ADS} 个本地广告位，未导入。`);
  return next;
}

/** The match's same-coordinates answers after an import: the file fills in clouds the match has no answer for yet. */
export function importedAlignment(current: Readonly<Record<string, boolean>>, clouds: readonly ExportedCloud[]): Record<string, boolean> {
  return { ...Object.fromEntries(clouds.map((cloud) => [cloud.id, cloud.aligned])), ...current };
}

/** Imported point-cloud placements whose cloud is neither loaded nor confirmed: they show and send only once it is. */
export function waitingForCloud(imported: readonly LocalAd[], loadedId: string | undefined, alignment: Readonly<Record<string, boolean>>) {
  return imported.filter((ad) => ad.space !== 'pitch' && ad.cloudId !== loadedId && alignment[ad.cloudId] !== true);
}
