import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { createServer as createHttpServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { createServer } from 'vite';

const vite = await createServer({ configFile: false, server: { middlewareMode: true } });
const { hashFile, localFilesMiddleware } = await vite.ssrLoadModule('/scripts/local-files.ts');
const base = mkdtempSync(join(tmpdir(), 'local-files-'));
const root = join(base, 'shared'), outside = join(base, 'private'), store = join(base, 'store');
let server, origin;
const hashed = [];

before(async () => {
  mkdirSync(join(root, 'bundle', 'media'), { recursive: true });
  mkdirSync(join(root, 'bundle', '.cache'));
  mkdirSync(outside);
  mkdirSync(store);
  writeFileSync(join(root, 'match.zip'), 'zip-bytes');
  writeFileSync(join(root, 'bundle', 'MANIFEST.json'), '{}');
  writeFileSync(join(root, 'bundle', 'media', 'video.mp4'), 'video');
  writeFileSync(join(root, 'bundle', '.cache', 'skip'), 'x');
  writeFileSync(join(root, '.env.local'), 'FUSION_API_KEY=secret');
  writeFileSync(join(outside, 'secret.txt'), 'secret');
  symlinkSync(join(outside, 'secret.txt'), join(root, 'link.txt'));
  symlinkSync(outside, join(root, 'link-dir'));
  const middleware = localFilesMiddleware([join(base, 'missing-root'), root], { writeRoots: [store], hash: (path) => { hashed.push(path); return hashFile(path); } });
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

test('an export goes to a typed .json file as is, else under its default name in the typed or default folder', async () => {
  const { exportTarget } = await vite.ssrLoadModule('/src/shared/server-files.ts');
  assert.equal(exportTarget('', '/data/matches/M1/exports/', 'scene.json'), '/data/matches/M1/exports/scene.json');
  assert.equal(exportTarget(' "/srv/out//" ', '/data', 'scene.json'), '/srv/out/scene.json');
  assert.equal(exportTarget('/srv/out/final.JSON', '/data', 'scene.json'), '/srv/out/final.JSON');
  assert.throws(() => exportTarget('out/', '/data', 'scene.json'), /绝对路径/);
});

test('hashes a server file without sending it, again only after the file changed', async () => {
  const path = join(root, 'bundle', 'logo.png');
  writeFileSync(path, 'first');
  const sha = async () => (await (await get('sha256', path)).json());
  const first = createHash('sha256').update('first').digest('hex');
  assert.deepEqual(await sha(), { sha256: first, size: 5 });
  assert.deepEqual(await sha(), { sha256: first, size: 5 });
  assert.equal(hashed.filter((item) => item === path).length, 1);
  writeFileSync(path, 'second');
  assert.deepEqual(await sha(), { sha256: createHash('sha256').update('second').digest('hex'), size: 6 });
  assert.equal(hashed.filter((item) => item === path).length, 2);
  // Same size, new content: the modification time tells.
  writeFileSync(path, 'third!');
  assert.equal((await sha()).sha256, createHash('sha256').update('third!').digest('hex'));
  assert.equal((await get('sha256', join(root, '.env.local'))).status, 403);
  assert.equal((await get('sha256', join(root, 'bundle'))).status, 400);
});

test('writes JSON exports inside the shared roots or the store, creating folders, never replacing a file unasked', async () => {
  const write = (path, body = '{"a":1}', { overwrite = false, type = 'application/json', site = origin } = {}) =>
    fetch(`${origin}/local-files/write?path=${encodeURIComponent(path)}${overwrite ? '&overwrite=1' : ''}`,
      { method: 'POST', body, headers: { 'Content-Type': type, ...(site ? { Origin: site } : {}) } });
  const target = join(root, 'exports', 'new', 'scene.json');
  const first = await write(target);
  assert.equal(first.status, 200);
  assert.deepEqual(await first.json(), { path: target });
  assert.equal(readFileSync(target, 'utf8'), '{"a":1}');
  // An existing file is kept unless the page says to replace it.
  assert.equal((await write(target, '{"a":2}')).status, 409);
  assert.equal(readFileSync(target, 'utf8'), '{"a":1}');
  assert.equal((await write(target, '{"a":2}', { overwrite: true })).status, 200);
  assert.equal(readFileSync(target, 'utf8'), '{"a":2}');
  assert.deepEqual(readdirSync(join(root, 'exports', 'new')), ['scene.json']);
  // The studio data directory is writable even outside the shared roots.
  assert.equal((await write(join(store, 'matches', 'M1', 'exports', 'x.json'))).status, 200);
  assert.ok(existsSync(join(store, 'matches', 'M1', 'exports', 'x.json')));
  for (const [path, status] of [[join(outside, 'x.json'), 403], [join(root, 'link-dir', 'x.json'), 403], [`${root}/exports/../../private/x.json`, 403],
    [join(root, '.hidden', 'x.json'), 403], [join(root, '.x.json'), 403], [join(root, 'scene.txt'), 400], ['exports/x.json', 400],
    [join(root, 'bundle', 'MANIFEST.json', 'x.json'), 400], [join(root, 'bundle'), 400]]) {
    assert.equal((await write(path)).status, status, path);
  }
  assert.ok(!existsSync(join(outside, 'x.json')));
  assert.equal((await write(join(root, 'bad.json'), 'not json')).status, 400);
  assert.equal((await write(join(root, 'big.json'), `"${'x'.repeat(65 * 1024 * 1024)}"`)).status, 413);
  // Another site's form or script cannot write.
  assert.equal((await write(join(root, 'form.json'), '{}', { type: 'text/plain' })).status, 415);
  assert.equal((await write(join(root, 'site.json'), '{}', { site: 'http://example.com' })).status, 403);
  assert.equal((await write(join(root, 'none.json'), '{}', { site: null })).status, 403);
  assert.ok(!existsSync(join(root, 'form.json')) && !existsSync(join(root, 'site.json')) && !existsSync(join(root, 'none.json')));
  assert.equal((await fetch(`${origin}/local-files/write?path=${encodeURIComponent(join(root, 'get.json'))}`)).status, 405);
});
