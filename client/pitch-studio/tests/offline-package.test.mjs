import assert from 'node:assert/strict';
import { createHash, webcrypto } from 'node:crypto';
import { after, test } from 'node:test';
import { zipSync, strToU8 } from 'fflate';
import { createServer } from 'vite';

globalThis.crypto ??= webcrypto;
const server = await createServer({ configFile: false, server: { middlewareMode: true } });
after(() => server.close());
const { parseOfflinePackage } = await server.ssrLoadModule('/src/matches/offline-package.ts');
const { validateMatch } = await server.ssrLoadModule('/src/matches/match-domain.ts');
const image = new Uint8Array(Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg==', 'base64'));
function glb(uri) {
  const json = Buffer.from(JSON.stringify({ asset: { version: '2.0' }, ...(uri ? { images: [{ uri }] } : {}) }).padEnd(128, ' '));
  const bytes = Buffer.alloc(20 + json.length);
  bytes.write('glTF'); bytes.writeUInt32LE(2, 4); bytes.writeUInt32LE(bytes.length, 8);
  bytes.writeUInt32LE(json.length, 12); bytes.write('JSON', 16); json.copy(bytes, 20);
  return new Uint8Array(bytes);
}
function fixture() {
  const files = { 'assets/ASSET-image.png': image, 'assets/ASSET-model.glb': glb() };
  const placement = { id: 'PLC-one', name: 'Image area', x: -20, y: 29, width: 12, height: 4,
    coordinateSpace: 'pitch', assetIds: ['ASSET-image'], assetRotation: 90 };
  const manifest = { packageVersion: 1, match: {
    schemaVersion: 1, id: 'MATCH-offline', campaignId: 'LIVE-offline', name: 'Offline match', date: '2026-11-14', venue: 'Test stadium', sport: 'football', version: 'saved-version',
    field: { id: 'FIELD-one', name: 'Football', length: 105, width: 68 },
    coordinates: { unit: 'meters', origin: 'field-center', x: 'right', y: 'down-in-top-view', modelUp: 'gltf-y-up' },
    ads: [{ id: 'AD-images', name: 'Images', type: '2D', placements: [placement, { ...placement, id: 'PLC-two', x: 20 }, { ...placement, id: 'PLC-empty', x: 0, assetIds: [] }] },
      { id: 'AD-model', name: 'Model', type: '3D', placements: [{ ...placement, id: 'PLC-model', y: 20, assetIds: ['ASSET-model'], modelPose: { x: 0.4, y: -0.5, scale: 0.8, yaw: 37 } }] }],
    assets: Object.entries(files).map(([path, bytes]) => ({ id: path.endsWith('.png') ? 'ASSET-image' : 'ASSET-model',
      name: path, contentType: path.endsWith('.png') ? 'image/png' : 'model/gltf-binary', byteLength: bytes.length, contentPath: path })),
  }, sha256: Object.fromEntries(Object.entries(files).map(([path, bytes]) => [path, createHash('sha256').update(bytes).digest('hex')])) };
  return { manifest, files };
}
function archive({ manifest, files }) {
  return new Blob([zipSync({ ...files, 'manifest.json': strToU8(JSON.stringify(manifest)) })]);
}

test('whole-match ZIP preserves transforms, shared original materials and unassigned areas without API URLs', async () => {
  const result = await parseOfflinePackage(archive(fixture()));
  assert.equal(result.match.mode, 'offline');
  assert.equal(result.match.placements.length, 4);
  assert.equal(result.assets.size, 2);
  assert.deepEqual(result.match.placements[3].modelPose, { x: 0.4, y: -0.5, scale: 0.8, yaw: 37 });
  assert.equal(result.match.placements[0].assetRotation, 90);
  assert.equal(result.match.placements[0].x, -20);
  assert.equal(result.match.placements[2].asset, undefined);
  assert.equal(result.match.placements[0].asset.url, `/fusion-offline/packages/${result.id}/assets/ASSET-image`);
  assert.deepEqual(new Uint8Array(await result.assets.get('ASSET-image').arrayBuffer()), image);
});
test("each 2D placement keeps its own assetOpacity (#188); an older manifest's default is 1, invalid values are refused", async () => {
  const package_ = fixture(), [one, two] = package_.manifest.match.ads[0].placements;
  one.assetOpacity = 0.25;
  two.assetOpacity = 0.75;
  const { match } = await parseOfflinePackage(archive(package_));
  assert.deepEqual(match.placements.map((slot) => slot.assetOpacity), [0.25, 0.75, 1, undefined]);
  // Stored and read back, as the match library does; records from before #188 read as 1.
  assert.deepEqual(validateMatch(JSON.parse(JSON.stringify(match))).placements.map((slot) => slot.assetOpacity), [0.25, 0.75, 1, undefined]);
  const { assetOpacity, ...older } = match.placements[0];
  assert.equal(validateMatch({ ...match, placements: [older] }).placements[0].assetOpacity, 1);
  assert.throws(() => validateMatch({ ...match, placements: [{ ...older, assetOpacity: 1.2 }] }), /不透明度/);
  one.assetOpacity = 0;
  assert.equal((await parseOfflinePackage(archive(package_))).match.placements[0].assetOpacity, 0);
  for (const value of [35, -0.01, '0.35', null]) {
    one.assetOpacity = value;
    await assert.rejects(parseOfflinePackage(archive(package_)), /PLC-one: assetOpacity must be a number from 0 to 1/);
  }
});
test("a ZIP's model animation (#157) survives import unchanged, and an invalid one names its field", async () => {
  const animated = { x: 0, y: 0, scale: 0.7, yaw: 30, swingAxis: 'z', swingAmplitude: 45, swingPeriod: 2, rotationMode: 'continuous',
    motion: { enabled: true, start: { x: -8, y: 2, z: 0, scale: 0.5 }, end: { x: 16, y: -6, z: -4, scale: 1.5 }, duration: 4, playback: 'ping-pong' } };
  const package_ = fixture();
  package_.manifest.match.ads[1].placements[0].modelPose = animated;
  const result = await parseOfflinePackage(archive(package_));
  assert.deepEqual(result.match.placements[3].modelPose, animated);
  package_.manifest.match.ads[1].placements[0].modelPose = { ...animated, motion: { ...animated.motion, playback: 'bounce' } };
  await assert.rejects(parseOfflinePackage(archive(package_)), /playback/);
});
test('rejects unsupported versions, missing files, bad checksums, invalid metadata and extra files', async () => {
  for (const change of [
    ({ manifest }) => { manifest.packageVersion = 2; },
    ({ files }) => { delete files['assets/ASSET-image.png']; },
    ({ manifest }) => { manifest.sha256['assets/ASSET-image.png'] = '0'.repeat(64); },
    ({ manifest }) => { manifest.match.assets[0].byteLength++; },
    ({ manifest }) => { manifest.match.assets[0].contentPath = 'https://example.test/image.png'; },
    ({ files }) => { files['assets/ASSET-unused.png'] = image; },
    ({ files }) => { files['../outside.png'] = image; },
  ]) {
    const data = fixture(); change(data);
    await assert.rejects(() => parseOfflinePackage(archive(data)));
  }
});
test('rejects externally linked GLB resources even with valid checksums', async () => {
  const data = fixture(), bytes = glb('https://example.test/texture.png'), path = 'assets/ASSET-model.glb';
  data.files[path] = bytes;
  data.manifest.match.assets[1].byteLength = bytes.length;
  data.manifest.sha256[path] = createHash('sha256').update(bytes).digest('hex');
  await assert.rejects(() => parseOfflinePackage(archive(data)), /external files/);
});
test('rejects oversized compressed images before inflation', async () => {
  const data = fixture();
  data.files['assets/ASSET-image.png'] = new Uint8Array(4 * 1024 * 1024 + 1);
  await assert.rejects(() => parseOfflinePackage(archive(data)), /size limits/);
});
