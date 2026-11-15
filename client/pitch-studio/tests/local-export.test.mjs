import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { createServer } from 'vite';

// The module has no dependencies to pre-bundle; skip the scan that would outlive the server.
const server = await createServer({ configFile: false, server: { middlewareMode: true }, optimizeDeps: { noDiscovery: true } });
after(() => server.close());
const { exportLocalPlacements, importedAlignment, mergeLocalPlacements, parseLocalPlacements, waitingForCloud } = await server.ssrLoadModule('/src/placements/local-export.ts');

const cloudA = 'a'.repeat(64), cloudB = 'b'.repeat(64);
const plane = { origin: [0, 0, 0], u: [1, 0, 0], v: [0, 1, 0], width: 2, height: 1 };
const base = { normalSign: -1, offset: 0, scale: 1, yaw: 0, visible: true, plane };
const pitch = { ...base, id: 'p1', name: '球场 2D', type: '2D', space: 'pitch', materialPath: '/srv/files/logo.png', materialName: 'logo.png' };
const flag = { ...base, id: 'f1', name: '旗帜', type: 'flag', space: 'cloud', cloudId: cloudA, offset: -0.5, materialPath: '/srv/files/flag.png',
  flag: { amplitude: 0.5, frequency: 2, speed: 1.5, material: 'pbr' } };
const model = { ...base, id: 'm1', name: '模型', type: '3D', space: 'cloud', cloudId: cloudB, scale: 1.2, yaw: 30 };

test('an export round-trips through JSON with its clouds, without loaded files or Fusion data', () => {
  const loaded = { ...pitch, material: new File([new Uint8Array(1)], 'logo.png') };
  const file = exportLocalPlacements('M1', [loaded, flag, model], { [cloudA]: true }, { [cloudA]: '/srv/cloud.ply' }, new Date('2026-11-15T12:00:00Z'));
  assert.deepEqual(Object.keys(file), ['format', 'matchId', 'exportedAt', 'placements', 'clouds']);
  assert.equal(file.format, 'chr-studio-local-placements-v1');
  assert.equal('material' in file.placements[0], false);
  assert.deepEqual(file.clouds, [{ id: cloudA, aligned: true, path: '/srv/cloud.ply' }, { id: cloudB, aligned: false }]);
  const parsed = parseLocalPlacements(JSON.parse(JSON.stringify(file)));
  assert.deepEqual(parsed, { placements: [pitch, flag, model], clouds: file.clouds, matchId: 'M1', droppedPitchFlags: 0 });
});

test("the store's own local-ads.json imports too; anything else is refused", () => {
  assert.deepEqual(parseLocalPlacements({ revision: 3, data: [pitch] }).placements, [pitch]);
  assert.deepEqual(parseLocalPlacements([flag]).placements, [flag]);
  const file = exportLocalPlacements('M1', [pitch], {});
  for (const bad of [{ version: 1, records: [] }, { ...file, format: 'other' }, { ...file, assets: {} },
    { ...file, placements: [{ ...pitch, materialPath: 'logo.png' }] }, { ...file, clouds: [{ id: 'not-a-sha', aligned: true }] },
    { ...file, clouds: [{ id: cloudA }] }, [{ ...pitch, id: 1 }], 'text']) {
    assert.throws(() => parseLocalPlacements(bad), undefined, JSON.stringify(bad).slice(0, 80));
  }
  // Pitch flags of earlier versions are dropped and counted.
  const pitchFlag = { ...flag, id: 'old', space: 'pitch', cloudId: undefined };
  delete pitchFlag.cloudId;
  assert.deepEqual([parseLocalPlacements([pitch, pitchFlag]).placements, parseLocalPlacements([pitch, pitchFlag]).droppedPitchFlags], [[pitch], 1]);
});

test('merging replaces placements with the same id and keeps the others; replacing keeps only the imported', () => {
  const existing = [pitch, { ...model, name: '旧模型' }];
  const imported = [{ ...model, name: '新模型' }, flag];
  assert.deepEqual(mergeLocalPlacements(existing, imported, 'merge').map((ad) => ad.name), ['球场 2D', '新模型', '旗帜']);
  assert.deepEqual(mergeLocalPlacements(existing, imported, 'replace').map((ad) => ad.name), ['新模型', '旗帜']);
  const many = Array.from({ length: 200 }, (_, i) => ({ ...pitch, id: `p${i + 2}` }));
  assert.throws(() => mergeLocalPlacements(many, [pitch], 'merge'), /超过 200 个/);
});

test("imported clouds keep the match's own answers; placements of another cloud wait for it", () => {
  // The file's answers fill in clouds the match has none for; the operator's answer stays.
  assert.deepEqual(importedAlignment({ [cloudA]: false }, [{ id: cloudA, aligned: true }, { id: cloudB, aligned: true }]), { [cloudA]: false, [cloudB]: true });
  // Loaded, or confirmed: nothing waits. Neither: the placement waits for that same PLY.
  assert.deepEqual(waitingForCloud([pitch, flag, model], cloudA, {}), [model]);
  assert.deepEqual(waitingForCloud([pitch, flag, model], undefined, { [cloudB]: true }), [flag]);
  assert.deepEqual(waitingForCloud([pitch, flag, model], cloudA, { [cloudB]: true }), []);
});
