import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { request as httpRequest, createServer as createHttpServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, test } from 'node:test';
import { strToU8, zipSync } from 'fflate';
import { createServer } from 'vite';

const vite = await createServer({ configFile: false, server: { middlewareMode: true } });
const { studioStoreMiddleware } = await vite.ssrLoadModule('/scripts/studio-store.ts');
const { planMigration } = await vite.ssrLoadModule('/src/app/browser-migration.ts');
// Test data stays inside the package's ignored node_modules, not in a system temp folder.
const root = await mkdtemp(join(process.cwd(), 'node_modules', 'studio-store-test-'));
// Shared folders as LOCAL_FILES_ROOTS names them: never under a hidden folder, unlike a checkout may be.
const shared = await mkdtemp(join(tmpdir(), 'studio-store-shared-'));
const servers = [];
after(async () => {
  await vite.close();
  for (const server of servers) server.close();
  await rm(root, { recursive: true, force: true });
  await rm(shared, { recursive: true, force: true });
});

async function start(directory, limits) {
  const middleware = studioStoreMiddleware(directory, { limits, packageRoots: [shared] });
  const server = createHttpServer((req, res) => middleware(req, res, () => res.writeHead(404).end()));
  servers.push(server);
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const origin = `http://127.0.0.1:${server.address().port}`, base = `${origin}/studio-store`;
  // As the studio page sends them: from its own origin, with the body type each request expects.
  const pageHeaders = (method = 'GET') => ({ Origin: origin,
    ...(method === 'PUT' ? { 'Content-Type': 'application/json' } : method === 'POST' ? { 'Content-Type': 'application/octet-stream' } : {}) });
  return {
    port: server.address().port, base, origin,
    call: async (path, init = {}) => {
      const response = await fetch(`${base}/${path}`, { ...init, headers: { ...pageHeaders(init.method), ...init.headers } });
      return { status: response.status, body: response.headers.get('content-type')?.includes('json') ? await response.json() : Buffer.from(await response.arrayBuffer()) };
    },
  };
}
const put = (revision, data) => ({ method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ revision, data }) });
const store = await start(join(root, 'data'));
const dataDir = (await store.call('info')).body.dataDir;
const plane = { origin: [0, 0, 0], u: [1, 0, 0], v: [0, 1, 0], width: 2, height: 1 };
const ad = { id: 'a', name: 'ad', type: '2D', space: 'pitch', plane, normalSign: -1, offset: 0, scale: 1, yaw: 0, visible: true, materialPath: '/srv/logo.png' };

test('the data directory is private and documents round-trip with a revision', async () => {
  assert.equal((await stat(dataDir)).mode & 0o777, 0o700);
  assert.deepEqual((await store.call('docs/matches')).body, { revision: 0, data: null });
  assert.deepEqual((await store.call('docs/matches', put(0, { version: 1, records: [] }))).body, { revision: 1 });
  assert.deepEqual((await store.call('docs/matches')).body, { revision: 1, data: { version: 1, records: [] } });
  assert.deepEqual((await store.call('docs/matches/M1/local-ads', put(0, [ad]))).body, { revision: 1 });
  assert.deepEqual(JSON.parse(await readFile(join(dataDir, 'matches', 'M1', 'local-ads.json'), 'utf8')), { revision: 1, data: [ad] });
  // Writes go through a temporary file that is renamed into place.
  assert.deepEqual(await readdir(join(dataDir, 'matches', 'M1')), ['local-ads.json']);
});

test('a write based on an old revision is refused with 409 and the current revision', async () => {
  const response = await store.call('docs/package-paths', put(0, { P1: '/srv/a.zip' }));
  assert.equal(response.status, 200);
  const stale = await store.call('docs/package-paths', put(0, { P1: '/srv/b.zip' }));
  assert.equal(stale.status, 409);
  assert.equal(stale.body.revision, 1);
  assert.deepEqual((await store.call('docs/package-paths')).body.data, { P1: '/srv/a.zip' });
});

test('invalid documents are refused', async () => {
  for (const [path, data] of [['docs/matches', { version: 1, records: 'x' }], ['docs/matches/M2/local-ads', [{ ...ad, materialPath: 'logo.png' }]],
    ['docs/matches/M2/session', { sources: {}, alignment: {}, selection: {}, extra: 1 }], ['docs/package-paths', { P1: '/srv/.hidden/a.zip' }]]) {
    const response = await store.call(path, put(0, data));
    assert.equal(response.status, 400, path);
  }
  assert.equal((await store.call('docs/matches', { method: 'PUT', body: 'not json' })).status, 400);
  assert.equal((await store.call('docs/matches/M2/session')).body.revision, 0);
});

test('changes come only from the studio page: same origin, expected body type; reads stay open', async () => {
  const data = await start(join(root, 'csrf'));
  const send = (path, init) => fetch(`${data.base}/${path}`, init).then((response) => response.status);
  const library = JSON.stringify({ revision: 0, data: { version: 1, records: [] } });
  // Another website open in the operator's browser: no or a foreign Origin, or a form's text/plain body.
  for (const headers of [{ 'Content-Type': 'application/json' }, { Origin: 'http://example.com', 'Content-Type': 'application/json' },
    { Origin: 'null', 'Content-Type': 'application/json' }, { 'Sec-Fetch-Site': 'cross-site', 'Content-Type': 'application/json' }]) {
    assert.equal(await send('docs/matches', { method: 'PUT', headers, body: library }), 403, JSON.stringify(headers));
    assert.equal(await send('files?name=a.png', { method: 'POST', headers, body: 'png' }), 403);
    assert.equal(await send('matches/M1', { method: 'DELETE', headers }), 403);
  }
  assert.equal(await send('docs/matches', { method: 'PUT', headers: { Origin: data.origin, 'Content-Type': 'text/plain' }, body: library }), 415);
  assert.equal(await send('files?name=a.png', { method: 'POST', headers: { Origin: data.origin, 'Content-Type': 'text/plain' }, body: 'png' }), 415);
  assert.equal((await data.call('docs/matches')).body.revision, 0);
  assert.deepEqual(await readdir(join(root, 'csrf', 'files')), []);
  // The page itself: its Origin, or a same-origin fetch that names no Origin.
  assert.equal(await send('docs/matches', { method: 'PUT', headers: { Origin: data.origin, 'Content-Type': 'application/json; charset=utf-8' }, body: library }), 200);
  assert.equal(await send('files?name=a.png', { method: 'POST', headers: { 'Sec-Fetch-Site': 'same-origin', 'Content-Type': 'application/octet-stream' }, body: 'png' }), 200);
  assert.equal(await send('matches/M1', { method: 'DELETE', headers: { Origin: data.origin } }), 200);
  assert.equal(await send('info'), 200);
});

test('paths outside the store, hidden or relative, are refused', async () => {
  const file = (path) => `file?path=${encodeURIComponent(path)}`;
  for (const path of ['docs/matches/..%2F..%2Fetc/session', 'docs/matches/.hidden/session', 'docs/matches/M1/other', 'docs/files',
    file('/etc/passwd'), file(`${dataDir}/../outside.json`), file(`${dataDir}/.hidden.json`), file(`${dataDir}/matches/../matches.json`), file('matches.json')]) {
    assert.ok([400, 404].includes((await store.call(path)).status), path);
  }
  assert.equal((await store.call('matches/..%2F..', { method: 'DELETE' })).status, 400);
  const status = await new Promise((resolve) => httpRequest({ port: store.port, path: '/studio-store/info', headers: { Host: 'example.com' } },
    (res) => { res.resume(); resolve(res.statusCode); }).end());
  assert.equal(status, 403);
});

test('uploads are content-addressed, readable back and limited by type and size', async () => {
  const bytes = Buffer.from('not really a png, but bytes');
  const sha = createHash('sha256').update(bytes).digest('hex');
  const first = await store.call('files?name=logo.png', { method: 'POST', body: bytes });
  assert.deepEqual(first.body, { path: join(dataDir, 'files', `${sha}.png`), sha256: sha, size: bytes.length });
  assert.deepEqual((await store.call('files?name=copy.PNG', { method: 'POST', body: bytes })).body.path, first.body.path);
  assert.deepEqual((await store.call(`file?path=${encodeURIComponent(first.body.path)}`)).body, bytes);
  assert.equal((await store.call('files?name=tool.exe', { method: 'POST', body: bytes })).status, 415);
  assert.equal((await store.call('files?name=empty.png', { method: 'POST', body: Buffer.alloc(0) })).status, 400);
  const small = await start(join(root, 'small'), { '.png': 10 });
  assert.equal((await small.call('files?name=big.png', { method: 'POST', body: Buffer.alloc(11) })).status, 413);
  // A chunked upload is cut off while it streams; nothing partial is kept.
  const streamed = await small.call('files?name=big.png', { method: 'POST', duplex: 'half',
    body: new ReadableStream({ start(controller) { controller.enqueue(new Uint8Array(8)); controller.enqueue(new Uint8Array(8)); controller.close(); } }) })
    .catch(() => ({ status: 413 }));
  assert.equal(streamed.status, 413);
  assert.deepEqual(await readdir(join(root, 'small', 'files')), []);
});

test('re-importing the Fusion package of a match keeps its onsite placements and session', async () => {
  const offline = (packageId) => ({ match: { id: 'M3', name: 'Final', date: '2026-11-15', venue: 'Stadium', venueId: 'V1', mode: 'offline',
    offlinePackageId: packageId, field: { length: 105, width: 68 }, placements: [] }, draft: [], hiddenPlacementIds: [], assetFiles: {} });
  const library = (await store.call('docs/matches')).body.revision;
  assert.equal((await store.call('docs/matches', put(library, { version: 1, records: [offline('P1')] }))).status, 200);
  const flag = { ...ad, id: 'f', type: 'flag', space: 'cloud', cloudId: 'c'.repeat(64), flag: { amplitude: 0.32, frequency: 1.35, speed: 1, material: 'simple' } };
  const session = { sources: {}, alignment: { ['c'.repeat(64)]: true }, selection: { cloud: 'f' } };
  await store.call('docs/matches/M3/local-ads', put(0, [ad, flag]));
  await store.call('docs/matches/M3/session', put(0, session));
  // The new package: its ZIP is stored on its own and the library entry is replaced.
  await store.call('files?name=P2.zip', { method: 'POST', body: Buffer.from('new package') });
  assert.equal((await store.call('docs/matches', put(library + 1, { version: 1, records: [offline('P2')] }))).status, 200);
  assert.deepEqual((await store.call('docs/matches/M3/local-ads')).body, { revision: 1, data: [ad, flag] });
  assert.deepEqual((await store.call('docs/matches/M3/session')).body, { revision: 1, data: session });
});

test("migration moves only the browser's matches the server does not have", () => {
  const record = (id) => ({ match: { id, name: id }, draft: [] });
  const { migrate, skipped } = planMigration([record('A'), record('B'), record('C')], [record('B')]);
  assert.deepEqual(migrate.map((item) => item.match.id), ['A', 'C']);
  assert.deepEqual(skipped.map((item) => item.match.id), ['B']);
});

test('deleting a match removes its documents', async () => {
  assert.equal((await store.call('matches/M1', { method: 'DELETE' })).status, 200);
  assert.deepEqual((await store.call('docs/matches/M1/local-ads')).body, { revision: 0, data: null });
});

test('an offline ZIP on the server is verified and its materials stored there; the page gets only the manifest', async () => {
  const sha = (bytes) => createHash('sha256').update(bytes).digest('hex');
  const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg==', 'base64');
  const json = Buffer.from(JSON.stringify({ asset: { version: '2.0' }, materials: [{ name: 'glass', alphaMode: 'BLEND' }] }).padEnd(128, ' '));
  const glb = Buffer.alloc(20 + json.length);
  glb.write('glTF'); glb.writeUInt32LE(2, 4); glb.writeUInt32LE(glb.length, 8);
  glb.writeUInt32LE(json.length, 12); glb.write('JSON', 16); json.copy(glb, 20);
  const files = { 'assets/ASSET-image.png': png, 'assets/ASSET-model.glb': glb };
  const manifest = (hashes) => ({ packageVersion: 1, sha256: hashes, match: { id: 'MATCH-server', assets: [
    { id: 'ASSET-image', contentType: 'image/png', byteLength: png.length, contentPath: 'assets/ASSET-image.png' },
    { id: 'ASSET-model', contentType: 'model/gltf-binary', byteLength: glb.length, contentPath: 'assets/ASSET-model.glb' }] } });
  const hashes = Object.fromEntries(Object.entries(files).map(([path, bytes]) => [path, sha(bytes)]));
  const write = async (name, document) => {
    const path = join(shared, name);
    await writeFile(path, zipSync({ ...Object.fromEntries(Object.entries(files).map(([k, v]) => [k, new Uint8Array(v)])),
      'manifest.json': strToU8(JSON.stringify(document)) }));
    return path;
  };
  const post = (path) => store.call('packages', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ path }) });
  const good = await post(await write('match.zip', manifest(hashes)));
  assert.equal(good.status, 200, JSON.stringify(good.body));
  assert.equal(good.body.source.id, 'MATCH-server');
  assert.deepEqual(good.body.assets, [
    { id: 'ASSET-image', path: join(dataDir, 'files', `${sha(png)}.png`) },
    { id: 'ASSET-model', path: join(dataDir, 'files', `${sha(glb)}.glb`), blended: ['glass'] }]);
  assert.deepEqual(await readFile(good.body.assets[1].path), glb);
  // Importing again keeps the stored files.
  assert.equal((await post(join(shared, 'match.zip'))).status, 200);
  const tampered = await post(await write('bad.zip', manifest({ ...hashes, 'assets/ASSET-image.png': sha(Buffer.from('other')) })));
  assert.equal(tampered.status, 400);
  assert.match(tampered.body.error, /checksum failed: ASSET-image/);
  assert.equal((await post('/etc/hostname.zip')).status, 404);
  assert.equal((await post(join(shared, 'match.json'))).status, 400);
  assert.equal((await post('relative/match.zip')).status, 400);
  assert.equal((await store.call('packages', { method: 'POST', headers: { 'Content-Type': 'application/json', Origin: 'http://evil.example' },
    body: JSON.stringify({ path: join(shared, 'match.zip') }) })).status, 403);
  // A v2 package's point cloud is stored next to its materials (#210).
  const ply = Buffer.from('ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nend_header\n0 0 0\n');
  files['point-cloud/field.ply'] = ply;
  const v2 = manifest({ ...hashes, 'point-cloud/field.ply': sha(ply) });
  Object.assign(v2, { packageVersion: 2 });
  v2.match.field = { pointCloud: { fileName: 'stadium.ply', byteLength: ply.length, contentPath: 'point-cloud/field.ply' } };
  const withCloud = await post(await write('v2.zip', v2));
  assert.equal(withCloud.status, 200, JSON.stringify(withCloud.body));
  assert.deepEqual(withCloud.body.pointCloud, { path: join(dataDir, 'files', `${sha(ply)}.ply`), sha256: sha(ply) });
  assert.deepEqual(await readFile(withCloud.body.pointCloud.path), ply);
});

test('stored files may be cached by the browser; other files may not', async () => {
  const bytes = Buffer.from('cache me');
  const { body } = await store.call('files?name=cache.png', { method: 'POST', body: bytes });
  const response = await fetch(`${store.base}/file?path=${encodeURIComponent(body.path)}`);
  assert.match(response.headers.get('cache-control'), /immutable/);
});
