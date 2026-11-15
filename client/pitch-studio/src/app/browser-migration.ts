// Earlier versions kept the studio's data in this browser. On the first load
// against the studio store, whatever the server does not have yet is moved there:
// matches and drafts, offline materials, local placements with their material
// files, ZIP paths and sessions. The browser data stays untouched as a backup and
// is no longer used; a marker stops this from running again in this browser.
import { validateLibrary } from '../matches/match-domain.ts';
import { storedLocalAd, upgradeLocalAd, validLocalAd, validateStoredLocalAds, type LocalAd } from '../placements/local-ad.ts';
import { blendedMaterials } from '../render/render-description.ts';
import { StoredDocument, uploadFile } from '../shared/studio-store.ts';
import { validatePackagePaths, type MatchLibrary } from '../shared/store-documents.ts';
import { SESSION_KEY, parseSessions, validateSession } from './session.ts';
import type { MatchRecord } from '../shared/types.ts';

export const LEGACY_MATCHES_KEY = 'liveshow.onsite.matches.v1';
export const LEGACY_PACKAGE_PATHS_KEY = 'liveshow.onsite.package-paths.v1';
export const MIGRATED_KEY = 'liveshow.onsite.server-migrated.v1';
const EXTENSIONS: Record<string, string> = { 'image/png': '.png', 'image/jpeg': '.jpg', 'image/webp': '.webp', 'model/gltf-binary': '.glb' };

/** Which browser matches move to the server: those it does not have. The server's copy of any other is kept. */
export function planMigration(browser: readonly MatchRecord[], server: readonly MatchRecord[]) {
  const known = new Set(server.map((record) => record.match.id));
  return { migrate: browser.filter((record) => !known.has(record.match.id)), skipped: browser.filter((record) => known.has(record.match.id)) };
}

/** Every entry of an old IndexedDB object store, or none if this browser never created the database. */
async function legacyEntries(database: string, store: string): Promise<Map<string, unknown>> {
  const entries = new Map<string, unknown>();
  if (!(await indexedDB.databases()).some((info) => info.name === database)) return entries;
  const db = await new Promise<IDBDatabase>((resolve, reject) => {
    const request = indexedDB.open(database);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(new Error(`无法读取浏览器中的旧数据 ${database}：${request.error?.message}`));
  });
  try {
    if (!db.objectStoreNames.contains(store)) return entries;
    await new Promise<void>((resolve, reject) => {
      const request = db.transaction(store).objectStore(store).openCursor();
      request.onerror = () => reject(new Error(`无法读取浏览器中的旧数据 ${database}：${request.error?.message}`));
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) { resolve(); return; }
        entries.set(String(cursor.key), cursor.value);
        cursor.continue();
      };
    });
  } finally { db.close(); }
  return entries;
}

function legacyJson(key: string): unknown {
  const text = localStorage.getItem(key);
  return text === null ? undefined : JSON.parse(text);
}

/**
 * Moves this browser's old studio data to the store once; resolves with a
 * message for the operator, or undefined when there was nothing to move.
 */
export async function migrateBrowserData(library: StoredDocument<MatchLibrary>, packagePaths: StoredDocument<Record<string, string>>): Promise<string | undefined> {
  if (localStorage.getItem(MIGRATED_KEY)) return undefined;
  const oldLibrary = legacyJson(LEGACY_MATCHES_KEY);
  const browser = oldLibrary === undefined ? [] : validateLibrary(oldLibrary);
  if (!browser.length) { localStorage.setItem(MIGRATED_KEY, new Date().toISOString()); return undefined; }
  const [localAds, assets] = await Promise.all([legacyEntries('liveshow.local-advertising.v1', 'matches'),
    legacyEntries('liveshow.fusion-offline.v1', 'assets')]);
  const oldPaths = legacyJson(LEGACY_PACKAGE_PATHS_KEY), oldSessions = parseSessions(localStorage.getItem(SESSION_KEY));
  await library.load();
  const { migrate, skipped } = planMigration(browser, library.data?.records ?? []);
  const migrated: MatchRecord[] = [], failures: string[] = [];
  let placements = 0;
  for (const record of migrate) {
    const { match } = record;
    try {
      let next = record;
      if (match.mode === 'offline') {
        const assetFiles: Record<string, string> = {}, assetBlended: Record<string, string[]> = {};
        for (const slot of match.placements) {
          if (!slot.asset || assetFiles[slot.asset.id]) continue;
          const blob = assets.get(`${match.offlinePackageId}/${slot.asset.id}`);
          if (!(blob instanceof Blob) || !blob.size) throw new Error(`离线素材 ${slot.asset.name} 不在浏览器中`);
          assetFiles[slot.asset.id] = await uploadFile(blob, `${slot.asset.id}${EXTENSIONS[slot.asset.contentType]}`);
          if (slot.asset.contentType === 'model/gltf-binary') assetBlended[slot.asset.id] = blendedMaterials(new Uint8Array(await blob.arrayBuffer()));
        }
        next = { ...record, assetFiles, assetBlended };
      }
      const ads = localAds.get(match.id);
      if (Array.isArray(ads) && ads.length) {
        const stored: LocalAd[] = [];
        for (const ad of ads.filter(validLocalAd).map(upgradeLocalAd)) {
          // A material chosen locally now lives in the store, where chr-server can read it.
          const materialPath = ad.materialPath ?? (ad.material ? await uploadFile(ad.material, ad.material.name) : undefined);
          stored.push(storedLocalAd({ ...ad, ...(materialPath ? { materialPath } : {}) }));
        }
        const document = new StoredDocument(`matches/${encodeURIComponent(match.id)}/local-ads`, validateStoredLocalAds);
        if (!(await document.load())?.length) { await document.save(() => stored); placements += stored.length; }
      }
      if (oldSessions[match.id]) {
        const document = new StoredDocument(`matches/${encodeURIComponent(match.id)}/session`, validateSession);
        if (!(await document.load())) await document.save(() => oldSessions[match.id]);
      }
      migrated.push(next);
    } catch (error) { failures.push(`${match.name}（${error instanceof Error ? error.message : String(error)}）`); }
  }
  if (migrated.length) await library.save((data) => ({ version: 1, records: [...(data?.records ?? []), ...migrated] }));
  const packages = new Set(migrated.flatMap((record) => record.match.offlinePackageId ? [record.match.offlinePackageId] : []));
  const paths = oldPaths === undefined ? {} : validatePackagePaths(oldPaths);
  const moved = Object.fromEntries(Object.entries(paths).filter(([id]) => packages.has(id)));
  // The server's own paths win over the browser's.
  if (Object.keys(moved).length) await packagePaths.update((data) => ({ ...moved, ...data }));
  // A failed match is tried again on the next load; the rest are on the server and skipped then.
  if (!failures.length) localStorage.setItem(MIGRATED_KEY, new Date().toISOString());
  return [`已把本机浏览器中的 ${migrated.length} 场比赛、${placements} 个本地广告位迁移到服务器；浏览器中的旧数据保留作备份，不再使用。`,
    skipped.length ? `服务器已有、保留服务器版本：${skipped.map((record) => record.match.name).join('、')}。` : '',
    failures.length ? `未能迁移：${failures.join('；')}；刷新页面会再次尝试。` : ''].filter(Boolean).join(' ');
}
