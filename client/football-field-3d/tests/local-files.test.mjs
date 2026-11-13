import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { createServer as createHttpServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { createServer } from 'vite';

const vite = await createServer({ configFile: false, server: { middlewareMode: true } });
const { localFilesMiddleware } = await vite.ssrLoadModule('/scripts/local-files.ts');
const base = mkdtempSync(join(tmpdir(), 'local-files-'));
const root = join(base, 'shared'), outside = join(base, 'private');
let server, origin;

before(async () => {
  mkdirSync(join(root, 'bundle', 'media'), { recursive: true });
  mkdirSync(join(root, 'bundle', '.cache'));
  mkdirSync(outside);
  writeFileSync(join(root, 'match.zip'), 'zip-bytes');
  writeFileSync(join(root, 'bundle', 'MANIFEST.json'), '{}');
  writeFileSync(join(root, 'bundle', 'media', 'video.mp4'), 'video');
  writeFileSync(join(root, 'bundle', '.cache', 'skip'), 'x');
  writeFileSync(join(root, '.env.local'), 'FUSION_API_KEY=secret');
  writeFileSync(join(outside, 'secret.txt'), 'secret');
  symlinkSync(join(outside, 'secret.txt'), join(root, 'link.txt'));
  const middleware = localFilesMiddleware([join(base, 'missing-root'), root]);
  server = createHttpServer((req, res) => middleware(req, res, () => res.writeHead(404).end()));
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  origin = `http://127.0.0.1:${server.address().port}`;
});
after(async () => {
  await new Promise((resolve) => server.close(resolve));
  await vite.close();
  rmSync(base, { recursive: true, force: true });
});
const get = (endpoint, path, init) => fetch(`${origin}/local-files/${endpoint}?path=${encodeURIComponent(path)}`, init);

test('serves files by absolute path and lists folders with folder-relative paths, skipping hidden entries', async () => {
  const file = await get('file', join(root, 'match.zip'));
  assert.equal(file.status, 200);
  assert.equal(file.headers.get('content-type'), 'application/zip');
  assert.equal(await file.text(), 'zip-bytes');
  const folder = await get('folder', join(root, 'bundle'));
  assert.equal(folder.status, 200);
  assert.deepEqual((await folder.json()).files.map((entry) => entry.path).sort(), ['bundle/MANIFEST.json', 'bundle/media/video.mp4']);
});

test('rejects relative, parent, hidden, outside-root, missing and wrong-kind paths', async () => {
  for (const [endpoint, path, status] of [
    ['file', 'match.zip', 400],
    ['file', `${root}/bundle/../match.zip`, 403],
    ['file', join(root, '.env.local'), 403],
    ['file', join(root, 'link.txt'), 403],
    ['file', join(outside, 'secret.txt'), 403],
    ['file', join(root, 'missing.zip'), 404],
    ['file', join(root, 'bundle'), 400],
    ['folder', join(root, 'match.zip'), 400],
  ]) {
    const response = await get(endpoint, path);
    assert.equal(response.status, status, `${endpoint} ${path}`);
    assert.equal(typeof (await response.json()).error, 'string');
  }
});

test('answers only GET requests addressed to a loopback host', async () => {
  assert.equal((await get('file', join(root, 'match.zip'), { method: 'POST' })).status, 405);
  const { request } = await import('node:http');
  const status = await new Promise((resolve, reject) => {
    request(`${origin}/local-files/file?path=${encodeURIComponent(join(root, 'match.zip'))}`,
      { headers: { host: 'attacker.example:80' } }, (res) => { res.resume(); resolve(res.statusCode); }).on('error', reject).end();
  });
  assert.equal(status, 403);
});

test('pasted server paths drop surrounding whitespace and quotes', async () => {
  const { isServerPath, serverPath } = await vite.ssrLoadModule('/src/shared/server-files.ts');
  assert.equal(serverPath('  "/home/user/match.fusion.zip" \n'), '/home/user/match.fusion.zip');
  assert.equal(serverPath("'/home/user/my match.zip'"), '/home/user/my match.zip');
  assert.ok(isServerPath(' "/home/user/match.zip"'));
  assert.ok(!isServerPath('"match.zip"'));
});
