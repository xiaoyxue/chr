import { createHash, randomUUID } from 'node:crypto';
import { createReadStream, createWriteStream } from 'node:fs';
import { mkdir, open, readFile, realpath, rename, rm, stat, writeFile } from 'node:fs/promises';
import type { IncomingMessage, Server } from 'node:http';
import { extname, join } from 'node:path';
import { Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import type { Connect, Plugin } from 'vite';
import { Failure, writeProblem } from './request-guard.ts';
import { realRoots, sharedPath } from './local-files.ts';
import { EXTENSIONS, blendedMaterials, readOfflinePackage } from '../src/matches/offline-reader.ts';
import { formatBytes } from '../src/matches/offline-limits.ts';
import { FILE_LIMITS, MATCH_ID, documentValidator } from '../src/shared/store-documents.ts';

// The studio's data on the onsite server, in one directory (STUDIO_DATA_DIR):
//   matches.json                      the match library and Fusion drafts
//   package-paths.json                offline package → ZIP path chr-server reads
//   matches/<matchId>/local-ads.json  local placements and flags
//   matches/<matchId>/session.json    the operator's session
//   matches/<matchId>/exports/        the default folder for the studio's exports (written via /local-files/write)
//   files/<sha256>.<ext>              uploaded materials and offline ZIPs, and the materials and point
//                                     clouds of offline ZIPs imported by server path (POST /studio-store/packages)
// Documents hold `{ revision, data }`; a write names the revision it was based on
// and is refused (409) if another page saved in between. Loopback hosts only; changes only
// from the studio page itself (see request-guard.ts).
const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);
const MB = 1024 * 1024;
const MAX_DOCUMENT_BYTES = 16 * MB;
const TYPES: Record<string, string> = {
  '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.webp': 'image/webp', '.glb': 'model/gltf-binary', '.zip': 'application/zip',
  '.json': 'application/json',
};

/** Writes through a temporary file renamed into place, so a crash never leaves half a file. */
async function writeAtomically(file: string, text: string) {
  await mkdir(join(file, '..'), { recursive: true, mode: 0o700 });
  const temporary = `${file}.${randomUUID()}.tmp`;
  try {
    await writeFile(temporary, text, { mode: 0o600 });
    await rename(temporary, file);
  } catch (error) { await rm(temporary, { force: true }); throw error; }
}

async function readBody(req: IncomingMessage, limit: number) {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req as AsyncIterable<Buffer>) {
    if ((size += chunk.length) > limit) throw new Failure(413, `请求超过 ${limit / MB} MB。`);
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString('utf8');
}

export interface StudioStoreOptions {
  limits?: Readonly<Record<string, number>>;
  /** Folders offline ZIPs may be imported from by server path (LOCAL_FILES_ROOTS). */
  packageRoots?: string[];
}

export function studioStoreMiddleware(directory: string, { limits = FILE_LIMITS, packageRoots = [] }: StudioStoreOptions = {}): Connect.NextHandleFunction {
  const ready = mkdir(join(directory, 'files'), { recursive: true, mode: 0o700 }).then(() => realpath(directory));
  const readableRoots = realRoots(packageRoots);
  // An unusable directory is reported by every request rather than ending the page server.
  ready.catch((error: unknown) => console.error(`[studio-store] cannot use ${directory}:`, error));
  const locks = new Map<string, Promise<unknown>>();
  /** Runs one document's read-check-write steps after the previous ones. */
  const locked = <T>(key: string, task: () => Promise<T>) => {
    const run = (locks.get(key) ?? Promise.resolve()).then(task, task);
    locks.set(key, run.catch((): undefined => undefined));
    return run;
  };

  const readDocument = async (file: string): Promise<{ revision: number; data: unknown }> => {
    try { return JSON.parse(await readFile(file, 'utf8')) as { revision: number; data: unknown }; }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return { revision: 0, data: null };
      throw error;
    }
  };

  /**
   * Stored files are named by their content, so a path to one in another data directory, such as a
   * store copied from another server, names this directory's file of the same name when it has one.
   */
  const rebased = async (value: unknown): Promise<unknown> => {
    if (Array.isArray(value)) return Promise.all(value.map(rebased));
    if (value && typeof value === 'object')
      return Object.fromEntries(await Promise.all(Object.entries(value).map(async ([key, item]) => [key, await rebased(item)])));
    const stored = typeof value === 'string' ? /^\/.+\/files\/([0-9a-f]{64}\.[a-z0-9]+)$/.exec(value) : null;
    if (!stored) return value;
    const own = join(await ready, 'files', stored[1]);
    return own !== value && await stat(own).then((info) => info.isFile(), () => false) ? own : value;
  };

  const document = async (req: IncomingMessage, name: string) => {
    const validate = documentValidator(name);
    if (!validate) throw new Failure(404, `没有这个存储文档：${name}`);
    const file = join(await ready, `${name}.json`);
    if (req.method === 'GET') {
      const { revision, data } = await readDocument(file);
      return { revision, data: await rebased(data) };
    }
    if (req.method !== 'PUT') throw new Failure(405, 'Only GET and PUT are supported.');
    let body: { revision?: unknown; data?: unknown };
    try { body = JSON.parse(await readBody(req, MAX_DOCUMENT_BYTES)) as typeof body; }
    catch (error) { throw error instanceof Failure ? error : new Failure(400, '请求不是有效的 JSON。'); }
    if (!Number.isSafeInteger(body?.revision)) throw new Failure(400, '缺少文档版本号（revision）。');
    let data: unknown;
    try { data = validate(body.data); }
    catch (error) { throw new Failure(400, error instanceof Error ? error.message : String(error)); }
    return locked(file, async () => {
      const current = await readDocument(file);
      if (body.revision !== current.revision)
        throw new Failure(409, '另一个页面已修改了这份数据。', { revision: current.revision });
      const revision = current.revision + 1;
      await writeAtomically(file, JSON.stringify({ revision, data }));
      return { revision };
    });
  };

  /** Streams an upload to a temporary file while hashing it, then keeps it under its SHA-256. */
  const upload = async (req: IncomingMessage, name: string | null) => {
    const extension = extname(name ?? '').toLowerCase(), limit = limits[extension];
    if (!limit) throw new Failure(415, '只能上传 PNG / JPEG / WebP 图片、GLB 模型、离线 ZIP 或其 PLY 点云。');
    if (Number(req.headers['content-length']) > limit) throw new Failure(413, `文件超过 ${formatBytes(limit)} 上限。`);
    const files = join(await ready, 'files'), temporary = join(files, `upload-${randomUUID()}.part`);
    const hash = createHash('sha256');
    let size = 0;
    try {
      await pipeline(req, new Transform({
        transform(chunk: Buffer, _encoding, callback) {
          if ((size += chunk.length) > limit) { callback(new Failure(413, `文件超过 ${formatBytes(limit)} 上限。`)); return; }
          hash.update(chunk);
          callback(null, chunk);
        },
      }), createWriteStream(temporary, { mode: 0o600 }));
      if (!size) throw new Failure(400, '文件为空。');
      const sha256 = hash.digest('hex'), path = join(files, `${sha256}${extension}`);
      // The same content keeps one file.
      await rename(temporary, path);
      return { path, sha256, size };
    } finally { await rm(temporary, { force: true }); }
  };

  /** Keeps `bytes` under their SHA-256 (already verified), unless the store has them. */
  const keep = async (bytes: Uint8Array, sha256: string, extension: string) => {
    const files = join(await ready, 'files'), path = join(files, `${sha256}${extension}`);
    if (await stat(path).then((info) => info.size === bytes.length, () => false)) return path;
    const temporary = join(files, `import-${randomUUID()}.part`);
    try {
      await writeFile(temporary, bytes, { mode: 0o600 });
      await rename(temporary, path);
    } finally { await rm(temporary, { force: true }); }
    return path;
  };

  /**
   * Reads an offline ZIP by its server path, verifies it as the page does
   * (readOfflinePackage) and stores its materials and point cloud, so none crosses the network.
   * The ZIP is read by offset and each material stored once verified, so a package of several
   * GiB never sits in memory. Answers with the manifest's match, which the page maps and checks.
   */
  const importPackage = async (req: IncomingMessage) => {
    let body: { path?: unknown };
    try { body = JSON.parse(await readBody(req, MAX_DOCUMENT_BYTES)) as typeof body; }
    catch (error) { throw error instanceof Failure ? error : new Failure(400, '请求不是有效的 JSON。'); }
    if (typeof body?.path !== 'string' || extname(body.path).toLowerCase() !== '.zip')
      throw new Failure(400, '请给出现场服务器上离线 ZIP 的绝对路径。');
    const path = await sharedPath(body.path, [...await readableRoots, await ready]);
    if (!(await stat(path)).isFile()) throw new Failure(400, `不是可导入的离线 ZIP：${body.path}`);
    const file = await open(path);
    try {
      const { size } = await file.stat();
      const archive = { size, read: async (position: number, length: number) => {
        const buffer = Buffer.allocUnsafe(length);
        const { bytesRead } = await file.read(buffer, 0, length, position);
        return new Uint8Array(buffer.buffer, buffer.byteOffset, bytesRead);
      } };
      // A stored material that fails is the server's fault (500); anything else the reader refused is the package's (400).
      const store = (bytes: Uint8Array, sha256: string, extension: string) => keep(bytes, sha256, extension).catch((error: unknown) => {
        throw new Failure(500, `现场服务器存储失败：${error instanceof Error ? error.message : String(error)}`);
      });
      const read = await readOfflinePackage(archive, async (asset) => ({ id: asset.id,
        path: await store(asset.bytes, asset.sha256, EXTENSIONS[asset.contentType]),
        ...(asset.contentType === 'model/gltf-binary' ? { blended: blendedMaterials(asset.bytes) } : {}) }))
        .catch((error: unknown) => {
          throw error instanceof Failure ? error : new Failure(400, error instanceof Error ? error.message : String(error));
        });
      const cloud = read.pointCloud;
      return { source: read.source, assets: read.assets,
        ...(cloud ? { pointCloud: { path: await store(cloud.bytes, cloud.sha256, '.ply'), sha256: cloud.sha256 } } : {}) };
    } finally { await file.close(); }
  };

  /** Any regular file inside the data directory, by absolute path; never a hidden one. */
  const readStored = async (raw: string | null) => {
    const root = await ready;
    if (!raw?.startsWith(`${root}/`) || raw.slice(root.length + 1).split('/').some((part) => !part || part.startsWith('.')))
      throw new Failure(400, `不是服务器存储中的文件：${raw}`);
    const path = await realpath(raw).catch((): undefined => undefined);
    const info = path && path.startsWith(`${root}/`) ? await stat(path) : undefined;
    if (!info?.isFile()) throw new Failure(404, `存储中没有这个文件：${raw}`);
    return { path, size: info.size };
  };

  return (req, res, next) => {
    const url = new URL(req.url, 'http://localhost');
    if (!url.pathname.startsWith('/studio-store/')) return next();
    const headers = { 'Cache-Control': 'private, no-store', 'X-Content-Type-Options': 'nosniff' };
    const reply = (status: number, body: unknown) =>
      res.writeHead(status, { ...headers, 'Content-Type': 'application/json' }).end(JSON.stringify(body));
    const host = (req.headers.host ?? '').replace(/:\d+$/, '');
    if (!LOOPBACK.has(host)) return reply(403, { error: 'The studio store is only available through localhost.' });
    const route = url.pathname.slice('/studio-store/'.length);
    if (req.method !== 'GET' && req.method !== 'HEAD') {
      const problem = writeProblem(req, route === 'files' ? 'application/octet-stream' : req.method === 'DELETE' ? undefined : 'application/json');
      if (problem) return reply(problem.status, { error: problem.message });
    }
    void (async () => {
      let segments: string[];
      try { segments = route.split('/').map(decodeURIComponent); }
      catch { throw new Failure(400, 'Invalid path.'); }
      if (route === 'info' && req.method === 'GET') return reply(200, { dataDir: await ready });
      if (segments[0] === 'docs') return reply(200, await document(req, segments.slice(1).join('/')));
      if (segments[0] === 'matches' && segments.length === 2 && req.method === 'DELETE') {
        if (!MATCH_ID.test(segments[1])) throw new Failure(400, '场次 ID 无效。');
        const folder = join(await ready, 'matches', segments[1]);
        await locked(folder, () => rm(folder, { recursive: true, force: true }));
        return reply(200, {});
      }
      if (route === 'files' && req.method === 'POST') return reply(200, await upload(req, url.searchParams.get('name')));
      if (route === 'packages' && req.method === 'POST') return reply(200, await importPackage(req));
      if (route === 'file' && req.method === 'GET') {
        const { path, size } = await readStored(url.searchParams.get('path'));
        // Stored files are named by their content and never change: the browser may keep them.
        const cache = /\/files\/[0-9a-f]{64}\.[a-z]+$/.test(path) ? 'private, max-age=31536000, immutable' : headers['Cache-Control'];
        res.writeHead(200, { ...headers, 'Cache-Control': cache,
          'Content-Type': TYPES[extname(path).toLowerCase()] ?? 'application/octet-stream', 'Content-Length': size });
        return pipeline(createReadStream(path), res);
      }
      throw new Failure(404, 'Unknown studio store request.');
    })().catch((error: unknown) => {
      if (res.headersSent) res.destroy();
      else if (error instanceof Failure) reply(error.status, { error: error.message, ...error.extra });
      else reply(500, { error: `现场服务器存储失败：${error instanceof Error ? error.message : String(error)}` });
    });
  };
}

/**
 * Node ends a request still arriving after 5 minutes (`requestTimeout`); an offline ZIP of
 * several GiB uploaded from the operator's computer, often through an SSH tunnel, may take longer.
 */
function allowLongUploads(server: Server | null) {
  if (server) server.requestTimeout = 0;
}

export function studioStore(directory: string, packageRoots: string[]): Plugin {
  return {
    name: 'studio-store',
    configureServer(server) {
      allowLongUploads(server.httpServer as Server | null);
      server.middlewares.use(studioStoreMiddleware(directory, { packageRoots }));
    },
    configurePreviewServer(server) {
      allowLongUploads(server.httpServer as Server);
      server.middlewares.use(studioStoreMiddleware(directory, { packageRoots }));
    },
  };
}
