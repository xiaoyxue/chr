// The browser side of the studio store (scripts/studio-store.ts): every match,
// placement, session and stored file lives on the onsite server, in one data
// directory; the browser keeps none of it. Documents carry a revision, so a write
// based on stale data is refused instead of overwriting another page's changes.
import { serverPath } from './server-files.ts';
import { FILE_LIMITS } from './store-documents.ts';

/** Another page saved the document first; reload it before changing it again. */
export class StoreConflict extends Error {}

let dataDir: string;

async function request(url: string, init?: RequestInit) {
  let response: Response;
  try { response = await fetch(url, { cache: 'no-store', ...init }); }
  catch { throw new Error('无法连接现场服务器的数据存储（studio store）：请确认页面服务仍在运行，SSH 隧道未断开；数据未保存。'); }
  if (response.ok) return response;
  const body: unknown = await response.json().catch((): null => null);
  const detail = body && typeof body === 'object' && 'error' in body ? String(body.error) : `HTTP ${response.status}`;
  if (response.status === 409) throw new StoreConflict(detail);
  throw new Error(`现场服务器的数据存储拒绝了请求：${detail}`);
}

/** Checks that the store answers and learns its data directory; call once before anything else. */
export async function connectStore() {
  ({ dataDir } = await (await request('/studio-store/info')).json() as { dataDir: string });
  return dataDir;
}

/** The store's data directory on the onsite server. */
export const storeDirectory = () => dataDir;

/**
 * Writes a JSON export to a server path (see scripts/local-files.ts); resolves with the
 * written file's path, or undefined when the file exists and `overwrite` was not given.
 */
export async function writeServerJson(path: string, text: string, overwrite = false): Promise<string | undefined> {
  let response: Response;
  try {
    response = await fetch(`/local-files/write?path=${encodeURIComponent(path)}${overwrite ? '&overwrite=1' : ''}`,
      { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: text });
  } catch { throw new Error('无法连接现场服务器：请确认页面服务仍在运行，SSH 隧道未断开；文件未导出。'); }
  if (response.status === 409) return undefined;
  const body = await response.json().catch((): null => null) as { path?: string; error?: string } | null;
  if (!response.ok) throw new Error(`现场服务器未写入 ${path}：${body?.error ?? `HTTP ${response.status}`}`);
  return body.path;
}

const isStoredFile = (path: string) => Boolean(dataDir) && path.startsWith(`${dataDir}/`);

/** A file of the studio store, or any other server file (through /local-files), as a browser File. */
export async function readServerFile(path: string, name = path.split('/').pop(), type?: string, signal?: AbortSignal): Promise<File> {
  path = serverPath(path);
  const url = `${isStoredFile(path) ? '/studio-store/file' : '/local-files/file'}?path=${encodeURIComponent(path)}`;
  const blob = await (await request(url, { signal })).blob();
  return new File([blob], name, { type: type ?? blob.type });
}

/**
 * A server file's SHA-256 without downloading it: a stored file is named by its hash; any
 * other file is hashed by the page server, which hashes it again whenever it changes on disk.
 */
export async function serverSha256(path: string): Promise<string> {
  path = serverPath(path);
  const stored = path.startsWith(`${dataDir}/files/`) && /\/([0-9a-f]{64})\.[a-z0-9]+$/.exec(path);
  if (stored) return stored[1];
  const { sha256 } = await (await request(`/local-files/sha256?path=${encodeURIComponent(path)}`)).json() as { sha256: unknown };
  if (typeof sha256 !== 'string' || !/^[0-9a-f]{64}$/.test(sha256)) throw new Error(`服务器没有给出 ${path} 的 SHA-256。`);
  return sha256;
}

/** Stores a file chosen in the browser on the server, content-addressed; resolves with its absolute server path. */
export async function uploadFile(file: Blob, name: string): Promise<string> {
  const limit = FILE_LIMITS[name.slice(name.lastIndexOf('.')).toLowerCase()];
  if (!limit) throw new Error(`不能存储 ${name}：只接受 PNG / JPEG / WebP 图片、GLB 模型、离线 ZIP 或其 PLY 点云。`);
  if (file.size > limit) throw new Error(`${name} 超过 ${limit / 1024 / 1024} MB 上限，未上传。`);
  const response = await request(`/studio-store/files?name=${encodeURIComponent(name)}`, { method: 'POST', body: file,
    headers: { 'Content-Type': 'application/octet-stream' } });
  return ((await response.json()) as { path: string }).path;
}

/** A stored file's address for the browser, which may cache it: a stored file never changes. */
export const storedFileUrl = (path: string) => `/studio-store/file?path=${encodeURIComponent(serverPath(path))}`;

/** What the page server read from a Fusion ZIP on the onsite server, its materials and point cloud now in the store. */
export interface StoredPackageImport {
  source: Record<string, unknown>;
  assets: { id: string; path: string; blended?: string[] }[];
  pointCloud?: { path: string; sha256: string };
}

/** Has the page server read, verify and store an offline ZIP by its server path (see scripts/studio-store.ts). */
export async function importStoredPackage(path: string): Promise<StoredPackageImport> {
  const response = await request('/studio-store/packages', { method: 'POST',
    headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ path: serverPath(path) }) });
  return await response.json() as StoredPackageImport;
}

/** Removes a match's local placements and session; its stored files stay, as other matches may share them. */
export async function removeMatchDocuments(matchId: string) {
  await request(`/studio-store/matches/${encodeURIComponent(matchId)}`, { method: 'DELETE' });
}

/** One JSON document of the store and the revision this page last read or wrote. */
export class StoredDocument<T> {
  revision = 0;
  data: T | null = null;
  private queue: Promise<unknown> = Promise.resolve();
  constructor(readonly name: string, private validate: (data: unknown) => T) {}

  async load() {
    const { revision, data } = await (await request(`/studio-store/docs/${this.name}`)).json() as { revision: number; data: unknown };
    this.revision = revision;
    this.data = data === null ? null : this.validate(data);
    return this.data;
  }

  /**
   * Saves `change` applied to this page's copy, after any earlier save of this page.
   * A StoreConflict means another page saved first; the caller reloads and says so.
   */
  save(change: (data: T | null) => T) {
    return this.serial(() => this.put(change(this.data)));
  }

  /**
   * Applies `change` to the stored data. If another page saved first, the latest
   * version is loaded and `change` applied to it once more, so neither is lost.
   * For remembered state such as the session, where both changes should stay.
   */
  update(change: (data: T | null) => T) {
    return this.serial(async () => {
      try { await this.put(change(this.data)); }
      catch (error) {
        if (!(error instanceof StoreConflict)) throw error;
        await this.load();
        await this.put(change(this.data));
      }
    });
  }

  private async put(data: T) {
    const response = await request(`/studio-store/docs/${this.name}`, { method: 'PUT', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ revision: this.revision, data }) });
    this.revision = ((await response.json()) as { revision: number }).revision;
    this.data = data;
  }

  private serial(task: () => Promise<void>): Promise<void> {
    const run = this.queue.then(task, task);
    this.queue = run.catch((): undefined => undefined);
    return run;
  }
}
