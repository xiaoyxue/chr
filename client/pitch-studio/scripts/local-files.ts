import { createReadStream } from 'node:fs';
import { readdir, realpath, stat } from 'node:fs/promises';
import { homedir } from 'node:os';
import { basename, delimiter, extname, isAbsolute, join, sep } from 'node:path';
import { pipeline } from 'node:stream/promises';
import type { Connect, Plugin } from 'vite';

// Lets the browser load files that already sit on the onsite server by absolute path, as if they
// had been selected locally. Read-only, loopback hosts only, limited to the configured roots and
// never hidden files.
const TYPES: Record<string, string> = {
  '.zip': 'application/zip', '.json': 'application/json', '.png': 'image/png', '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg', '.webp': 'image/webp', '.glb': 'model/gltf-binary', '.mp4': 'video/mp4',
  '.webm': 'video/webm', '.mov': 'video/quicktime',
};
const MAX_FOLDER_FILES = 20000;
const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);

class Failure extends Error {
  constructor(readonly status: number, message: string) { super(message); }
}

export function localFilesMiddleware(roots: string[]): Connect.NextHandleFunction {
  const allowed = Promise.all(roots.map((root) => realpath(root).catch(() => {
    console.warn(`[local-files] ignoring missing shared folder ${root}`);
    return '';
  }))).then((paths) => paths.filter(Boolean));
  const resolve = async (raw: string | null) => {
    if (!raw || !isAbsolute(raw)) throw new Failure(400, 'Enter an absolute path on the onsite server, such as /home/user/match.zip.');
    if (raw.split(/[\\/]/).some((part) => part.startsWith('.'))) throw new Failure(403, 'Hidden files and relative segments are not served.');
    let path: string;
    try { path = await realpath(raw); } catch { throw new Failure(404, `Not found on the onsite server: ${raw}`); }
    if (!(await allowed).some((root) => path === root || path.startsWith(root.endsWith(sep) ? root : root + sep))
      || path.split(sep).some((part) => part.startsWith('.'))) throw new Failure(403, `Outside the folders this server shares: ${raw}`);
    return path;
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
    if (req.method !== 'GET') return fail(405, 'Only GET is supported.');
    void (async () => {
      const path = await resolve(url.searchParams.get('path'));
      const info = await stat(path);
      if (url.pathname === '/local-files/file') {
        if (!info.isFile()) throw new Failure(400, `Not a file: ${path}`);
        res.writeHead(200, { ...headers, 'Content-Type': TYPES[extname(path).toLowerCase()] ?? 'application/octet-stream',
          'Content-Length': info.size });
        await pipeline(createReadStream(path), res);
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

export function localFiles(rootList: string): Plugin {
  const roots = (rootList || homedir()).split(delimiter).filter(Boolean);
  return {
    name: 'local-files',
    configureServer(server) { server.middlewares.use(localFilesMiddleware(roots)); },
    configurePreviewServer(server) { server.middlewares.use(localFilesMiddleware(roots)); },
  };
}
