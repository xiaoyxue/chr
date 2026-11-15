import { createHash, randomUUID } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { link, lstat, mkdir, readdir, realpath, rename, rm, stat, writeFile } from 'node:fs/promises';
import type { IncomingMessage } from 'node:http';
import { homedir } from 'node:os';
import { basename, delimiter, dirname, extname, isAbsolute, join, normalize, sep } from 'node:path';
import { pipeline } from 'node:stream/promises';
import type { Connect, Plugin } from 'vite';
import { writeProblem } from './request-guard.ts';

// Lets the browser load files that already sit on the onsite server by absolute path, as if they
// had been selected locally, or learn a file's SHA-256 without transferring it; and write the
// studio's JSON exports there (POST /local-files/write), so nothing needs downloading to the
// operator's computer. Loopback hosts only, limited to the configured roots (writes also to the
// studio data directory) and never hidden files.
const TYPES: Record<string, string> = {
  '.zip': 'application/zip', '.json': 'application/json', '.png': 'image/png', '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg', '.webp': 'image/webp', '.glb': 'model/gltf-binary', '.mp4': 'video/mp4',
  '.webm': 'video/webm', '.mov': 'video/quicktime',
};
const MAX_FOLDER_FILES = 20000;
const MAX_EXPORT_BYTES = 64 * 1024 * 1024;
const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);

class Failure extends Error {
  constructor(readonly status: number, message: string) { super(message); }
}

async function readBody(req: IncomingMessage) {
  if (Number(req.headers['content-length']) > MAX_EXPORT_BYTES) throw new Failure(413, `Exports are limited to ${MAX_EXPORT_BYTES / 1024 / 1024} MB.`);
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req as AsyncIterable<Buffer>) {
    if ((size += chunk.length) > MAX_EXPORT_BYTES) throw new Failure(413, `Exports are limited to ${MAX_EXPORT_BYTES / 1024 / 1024} MB.`);
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString('utf8');
}

const inside = (path: string, roots: string[]) => roots.some((root) => path === root || path.startsWith(root.endsWith(sep) ? root : root + sep));

/** Streams a file through SHA-256; lowercase hex. */
export async function hashFile(path: string) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path) as AsyncIterable<Buffer>) hash.update(chunk);
  return hash.digest('hex');
}

export interface LocalFilesOptions {
  /** Folders writable besides the readable roots, such as the studio data directory. */
  writeRoots?: string[];
  hash?: (path: string) => Promise<string>;
}

export function localFilesMiddleware(roots: string[], { writeRoots = [], hash = hashFile }: LocalFilesOptions = {}): Connect.NextHandleFunction {
  // A file is hashed again whenever its size, modification time or inode changes.
  const hashes = new Map<string, { key: string; sha256: Promise<string> }>();
  const sha256 = async (path: string) => {
    const info = await stat(path, { bigint: true });
    const key = `${info.size}:${info.mtimeNs}:${info.ino}`, cached = hashes.get(path);
    if (cached?.key === key) return { sha256: await cached.sha256, size: Number(info.size) };
    const pending = hash(path);
    hashes.set(path, { key, sha256: pending });
    pending.catch(() => { if (hashes.get(path)?.sha256 === pending) hashes.delete(path); });
    return { sha256: await pending, size: Number(info.size) };
  };
  const allowed = Promise.all(roots.map((root) => realpath(root).catch(() => {
    console.warn(`[local-files] ignoring missing shared folder ${root}`);
    return '';
  }))).then((paths) => paths.filter(Boolean));
  const resolve = async (raw: string | null) => {
    if (!raw || !isAbsolute(raw)) throw new Failure(400, 'Enter an absolute path on the onsite server, such as /home/user/match.zip.');
    if (raw.split(/[\\/]/).some((part) => part.startsWith('.'))) throw new Failure(403, 'Hidden files and relative segments are not served.');
    let path: string;
    try { path = await realpath(raw); } catch { throw new Failure(404, `Not found on the onsite server: ${raw}`); }
    if (!inside(path, await allowed) || path.split(sep).some((part) => part.startsWith('.'))) throw new Failure(403, `Outside the folders this server shares: ${raw}`);
    return path;
  };
  /**
   * Writes a JSON export to an absolute `.json` path whose folder lies inside a writable root,
   * creating missing folders there. The file appears whole or not at all; an existing file is
   * replaced only with `overwrite`.
   */
  const write = async (req: IncomingMessage, raw: string | null, overwrite: boolean) => {
    if (!raw || !isAbsolute(raw) || !/\.json$/i.test(raw)) throw new Failure(400, 'Export to an absolute .json path on the onsite server, such as /home/user/exports/scene.json.');
    if (raw.split(/[\\/]/).some((part) => part.startsWith('.'))) throw new Failure(403, 'Hidden files and relative segments are not written.');
    const target = normalize(raw), missing: string[] = [];
    let folder = dirname(target);
    for (;;) {
      try { folder = await realpath(folder); break; }
      catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
        missing.unshift(basename(folder));
        folder = dirname(folder);
      }
    }
    const writable = (await Promise.all([...roots, ...writeRoots].map((root) => realpath(root).catch(() => '')))).filter(Boolean);
    if (!inside(folder, writable) || folder.split(sep).some((part) => part.startsWith('.'))) throw new Failure(403, `Outside the folders this server may write: ${raw}`);
    if (!(await stat(folder)).isDirectory()) throw new Failure(400, `Not a folder: ${folder}`);
    const text = await readBody(req);
    try { JSON.parse(text); } catch { throw new Failure(400, 'The export is not valid JSON.'); }
    folder = join(folder, ...missing);
    await mkdir(folder, { recursive: true });
    const file = join(folder, basename(target)), existing = await lstat(file).catch((): undefined => undefined);
    if (existing && !existing.isFile()) throw new Failure(400, `Not a regular file: ${file}`);
    if (existing && !overwrite) throw new Failure(409, `The file already exists: ${file}`);
    const temporary = join(folder, `.${basename(target)}.${randomUUID()}.tmp`);
    try {
      await writeFile(temporary, text);
      // Without overwrite, a link fails rather than replace a file created meanwhile.
      if (overwrite) await rename(temporary, file);
      else {
        try { await link(temporary, file); }
        catch (error) {
          if ((error as NodeJS.ErrnoException).code === 'EEXIST') throw new Failure(409, `The file already exists: ${file}`);
          throw error;
        }
      }
    } finally { await rm(temporary, { force: true }); }
    return { path: file };
  };
  const list = async (folder: string) => {
    const files: { path: string; size: number }[] = [];
    const walk = async (directory: string, prefix: string) => {
      for (const entry of await readdir(directory, { withFileTypes: true })) {
        if (entry.name.startsWith('.')) continue;
        const path = join(directory, entry.name), relative = `${prefix}/${entry.name}`;
        if (entry.isDirectory()) await walk(path, relative);
        else if (entry.isFile()) {
          if (files.length >= MAX_FOLDER_FILES) throw new Failure(413, `Folder has more than ${MAX_FOLDER_FILES} files.`);
          files.push({ path: relative, size: (await stat(path)).size });
        }
      }
    };
    await walk(folder, basename(folder));
    return files;
  };
  return (req, res, next) => {
    const url = new URL(req.url, 'http://localhost');
    if (!url.pathname.startsWith('/local-files/')) return next();
    const headers = { 'Cache-Control': 'private, no-store', 'X-Content-Type-Options': 'nosniff' };
    const fail = (status: number, error: string) => {
      res.writeHead(status, { ...headers, 'Content-Type': 'application/json' }).end(JSON.stringify({ error }));
    };
    const host = (req.headers.host ?? '').replace(/:\d+$/, '');
    if (!LOOPBACK.has(host)) return fail(403, 'Server paths are only available through localhost.');
    if (url.pathname === '/local-files/write') {
      if (req.method !== 'POST') return fail(405, 'Exports are written with POST.');
      const problem = writeProblem(req, 'application/json');
      if (problem) return fail(problem.status, problem.message);
      void write(req, url.searchParams.get('path'), url.searchParams.get('overwrite') === '1')
        .then((body) => res.writeHead(200, { ...headers, 'Content-Type': 'application/json' }).end(JSON.stringify(body)))
        .catch((error: unknown) => {
          if (error instanceof Failure) fail(error.status, error.message);
          else fail(500, `The onsite server could not write the export: ${error instanceof Error ? error.message : String(error)}`);
        });
      return;
    }
    if (req.method !== 'GET') return fail(405, 'Only GET is supported.');
    void (async () => {
      const path = await resolve(url.searchParams.get('path'));
      const info = await stat(path);
      if (url.pathname === '/local-files/file') {
        if (!info.isFile()) throw new Failure(400, `Not a file: ${path}`);
        res.writeHead(200, { ...headers, 'Content-Type': TYPES[extname(path).toLowerCase()] ?? 'application/octet-stream',
          'Content-Length': info.size });
        await pipeline(createReadStream(path), res);
      } else if (url.pathname === '/local-files/sha256') {
        if (!info.isFile()) throw new Failure(400, `Not a file: ${path}`);
        const body = JSON.stringify(await sha256(path));
        res.writeHead(200, { ...headers, 'Content-Type': 'application/json' }).end(body);
      } else if (url.pathname === '/local-files/folder') {
        if (!info.isDirectory()) throw new Failure(400, `Not a folder: ${path}`);
        const files = await list(path);
        res.writeHead(200, { ...headers, 'Content-Type': 'application/json' }).end(JSON.stringify({ files }));
      } else throw new Failure(404, 'Unknown server path request.');
    })().catch((error: unknown) => {
      if (res.headersSent) res.destroy();
      else if (error instanceof Failure) fail(error.status, error.message);
      else fail(500, `The onsite server could not read the path: ${error instanceof Error ? error.message : String(error)}`);
    });
  };
}

/** `dataDir`, the studio store, is writable for exports even when the shared roots do not include it. */
export function localFiles(rootList: string, dataDir: string): Plugin {
  const roots = (rootList || homedir()).split(delimiter).filter(Boolean);
  return {
    name: 'local-files',
    configureServer(server) { server.middlewares.use(localFilesMiddleware(roots, { writeRoots: [dataDir] })); },
    configurePreviewServer(server) { server.middlewares.use(localFilesMiddleware(roots, { writeRoots: [dataDir] })); },
  };
}
