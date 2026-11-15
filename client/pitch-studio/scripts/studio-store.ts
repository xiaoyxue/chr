import { createHash, randomUUID } from 'node:crypto';
import { createReadStream, createWriteStream } from 'node:fs';
import { mkdir, readFile, realpath, rename, rm, stat, writeFile } from 'node:fs/promises';
import type { IncomingMessage } from 'node:http';
import { extname, join } from 'node:path';
import { Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import type { Connect, Plugin } from 'vite';
import { FILE_LIMITS, MATCH_ID, documentValidator } from '../src/shared/store-documents.ts';
import { validateLocalPlacementsFile } from '../src/placements/local-export.ts';

// The studio's data on the onsite server, in one directory (STUDIO_DATA_DIR):
//   matches.json                      the match library and Fusion drafts
//   package-paths.json                offline package → ZIP path chr-server reads
//   matches/<matchId>/local-ads.json  local placements and flags
//   matches/<matchId>/session.json    the operator's session
//   matches/<matchId>/exports/*.json  exported onsite placements (chr-studio-local-placements-v1)
//   files/<sha256>.<ext>              uploaded materials and offline ZIPs
// Documents hold `{ revision, data }`; a write names the revision it was based on
// and is refused (409) if another page saved in between. Loopback hosts only.
const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);
const MB = 1024 * 1024;
const MAX_DOCUMENT_BYTES = 16 * MB;
const TYPES: Record<string, string> = {
  '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.webp': 'image/webp', '.glb': 'model/gltf-binary', '.zip': 'application/zip',
  '.json': 'application/json',
};

class Failure extends Error {
  constructor(readonly status: number, message: string, readonly extra: Record<string, unknown> = {}) { super(message); }
}

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

export function studioStoreMiddleware(directory: string, limits: Readonly<Record<string, number>> = FILE_LIMITS): Connect.NextHandleFunction {
  const ready = mkdir(join(directory, 'files'), { recursive: true, mode: 0o700 }).then(() => realpath(directory));
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

  const document = async (req: IncomingMessage, name: string) => {
    const validate = documentValidator(name);
    if (!validate) throw new Failure(404, `没有这个存储文档：${name}`);
    const file = join(await ready, `${name}.json`);
    if (req.method === 'GET') return readDocument(file);
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
    if (!limit) throw new Failure(415, '只能上传 PNG / JPEG / WebP 图片、GLB 模型或离线 ZIP。');
    if (Number(req.headers['content-length']) > limit) throw new Failure(413, `文件超过 ${limit / MB} MB 上限。`);
    const files = join(await ready, 'files'), temporary = join(files, `upload-${randomUUID()}.part`);
    const hash = createHash('sha256');
    let size = 0;
    try {
      await pipeline(req, new Transform({
        transform(chunk: Buffer, _encoding, callback) {
          if ((size += chunk.length) > limit) { callback(new Failure(413, `文件超过 ${limit / MB} MB 上限。`)); return; }
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

  /** Keeps an onsite-placements export under the match; never replaces an earlier one. */
  const exportPlacements = async (req: IncomingMessage, matchId: string) => {
    if (!MATCH_ID.test(matchId)) throw new Failure(400, '场次 ID 无效。');
    let file: unknown;
    try { file = validateLocalPlacementsFile(JSON.parse(await readBody(req, MAX_DOCUMENT_BYTES))); }
    catch (error) { throw error instanceof Failure ? error : new Failure(400, error instanceof Error ? error.message : String(error)); }
    const path = join(await ready, 'matches', matchId, 'exports', `${new Date().toISOString().replace(/[:.]/g, '-')}-${randomUUID().slice(0, 8)}.json`);
    await writeAtomically(path, JSON.stringify(file, null, 2));
    return { path };
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
    void (async () => {
      const route = url.pathname.slice('/studio-store/'.length);
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
      if (segments[0] === 'matches' && segments[2] === 'exports' && segments.length === 3 && req.method === 'POST')
        return reply(200, await exportPlacements(req, segments[1]));
      if (route === 'file' && req.method === 'GET') {
        const { path, size } = await readStored(url.searchParams.get('path'));
        res.writeHead(200, { ...headers, 'Content-Type': TYPES[extname(path).toLowerCase()] ?? 'application/octet-stream', 'Content-Length': size });
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

export function studioStore(directory: string): Plugin {
  return {
    name: 'studio-store',
    configureServer(server) { server.middlewares.use(studioStoreMiddleware(directory)); },
    configurePreviewServer(server) { server.middlewares.use(studioStoreMiddleware(directory)); },
  };
}
