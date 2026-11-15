import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readdir, readFile, rm, stat } from 'node:fs/promises';
import { request as httpRequest, createServer as createHttpServer } from 'node:http';
import { join } from 'node:path';
import { after, test } from 'node:test';
import { createServer } from 'vite';

const vite = await createServer({ configFile: false, server: { middlewareMode: true } });
const { studioStoreMiddleware } = await vite.ssrLoadModule('/scripts/studio-store.ts');
const { planMigration } = await vite.ssrLoadModule('/src/app/browser-migration.ts');
// Test data stays inside the package's ignored node_modules, not in a system temp folder.
const root = await mkdtemp(join(process.cwd(), 'node_modules', 'studio-store-test-'));
const servers = [];
after(async () => {
  await vite.close();
  for (const server of servers) server.close();
  await rm(root, { recursive: true, force: true });
});

async function start(directory, limits) {
  const middleware = studioStoreMiddleware(directory, limits);
  const server = createHttpServer((req, res) => middleware(req, res, () => res.writeHead(404).end()));
  servers.push(server);
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}/studio-store`;
  return {
    port: server.address().port,
    call: async (path, init) => {
      const response = await fetch(`${base}/${path}`, init);
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

test('an export of onsite placements is validated and kept under its match, never overwritten', async () => {
  const exported = { format: 'chr-studio-local-placements-v1', matchId: 'M4', exportedAt: '2026-11-15T12:00:00.000Z', placements: [ad], clouds: [] };
  const first = await store.call('matches/M4/exports', { method: 'POST', body: JSON.stringify(exported) });
  const second = await store.call('matches/M4/exports', { method: 'POST', body: JSON.stringify(exported) });
  assert.match(first.body.path, new RegExp(`^${dataDir}/matches/M4/exports/.+\\.json$`));
  assert.notEqual(first.body.path, second.body.path);
  assert.deepEqual((await store.call(`file?path=${encodeURIComponent(first.body.path)}`)).body, exported);
  // A Fusion library, or a file of another format, is not an export.
  for (const bad of [{ version: 1, records: [] }, { ...exported, format: 'other' }, { ...exported, match: {} }]) {
    assert.equal((await store.call('matches/M4/exports', { method: 'POST', body: JSON.stringify(bad) })).status, 400);
  }
  assert.equal((await store.call('matches/..%2F..%2Fx/exports', { method: 'POST', body: JSON.stringify(exported) })).status, 400);
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
