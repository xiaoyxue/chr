import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { createServer } from 'vite';

// The module has no dependencies to pre-bundle; skip the scan that would outlive the server.
const server = await createServer({ configFile: false, server: { middlewareMode: true }, optimizeDeps: { noDiscovery: true } });
after(() => server.close());
const { SESSION_KEY, SessionStore, cloudAligned, emptySession, parseSessions, restorePlan, withAlignment } = await server.ssrLoadModule('/src/app/session.ts');

const cloudId = 'a'.repeat(64);

test('a stored session is validated; malformed parts are dropped, not trusted', () => {
  assert.deepEqual(parseSessions('not json'), {});
  assert.deepEqual(parseSessions('[]'), {});
  const sessions = parseSessions(JSON.stringify({
    M1: {
      sources: { cloud: { path: '/srv/cloud.ply', name: 'cloud.ply' }, result: { path: 'relative/result', name: 'result' }, video: { name: 'scan.mp4' }, extra: { name: 'x' } },
      alignment: { [cloudId]: true, 'not-a-sha': true, ['b'.repeat(64)]: 'yes' },
      selection: { fusion: 'PLC-1', pitch: 7 },
    },
    M2: 'broken',
  }));
  assert.deepEqual(sessions, { M1: {
    sources: { cloud: { path: '/srv/cloud.ply', name: 'cloud.ply' }, video: { name: 'scan.mp4' } },
    alignment: { [cloudId]: true },
    selection: { fusion: 'PLC-1' },
  } });
});

test('a reload re-reads server paths (point cloud first) and names local files to choose again', () => {
  const session = { ...emptySession(), sources: {
    video: { name: 'scan.mp4' }, result: { path: '/srv/result', name: 'result' }, cloud: { path: '/srv/cloud.ply', name: 'cloud.ply' },
  } };
  assert.deepEqual(restorePlan(session), {
    load: [{ kind: 'cloud', path: '/srv/cloud.ply' }, { kind: 'result', path: '/srv/result' }],
    reselect: [{ kind: 'video', name: 'scan.mp4' }],
  });
  // What the page still holds (switching back to a match) needs nothing.
  assert.deepEqual(restorePlan(session, new Set(['cloud', 'video'])), { load: [{ kind: 'result', path: '/srv/result' }], reselect: [] });
});

test("a cloud keeps the operator's alignment answer, else what the file declares", () => {
  const session = emptySession();
  assert.equal(cloudAligned(session, cloudId, false), false);
  assert.equal(cloudAligned(withAlignment(session, cloudId, true), cloudId, false), true);
  // Un-ticking a cloud that declares pitch coordinates stays un-ticked.
  assert.equal(cloudAligned(withAlignment(session, cloudId, false), cloudId, true), false);
  let many = session;
  for (let i = 0; i < 25; i++) many = withAlignment(many, i.toString(16).padStart(64, '0'), true);
  many = withAlignment(many, (5).toString(16).padStart(64, '0'), false);
  assert.equal(Object.keys(many.alignment).length, 20);
  assert.equal(Object.keys(many.alignment).at(-1), (5).toString(16).padStart(64, '0'));
});

test('the session store writes only changes and forgets a deleted match', () => {
  const items = new Map(), writes = [];
  const storage = { getItem: (key) => items.get(key) ?? null, setItem: (key, value) => { writes.push(key); items.set(key, value); } };
  const store = new SessionStore(storage);
  store.update('M1', (session) => withAlignment(session, cloudId, true));
  store.update('M1', (session) => withAlignment(session, cloudId, true));
  assert.deepEqual(writes, [SESSION_KEY]);
  assert.equal(new SessionStore(storage).get('M1').alignment[cloudId], true);
  store.remove('M1');
  assert.deepEqual(new SessionStore(storage).get('M1'), emptySession());
});
