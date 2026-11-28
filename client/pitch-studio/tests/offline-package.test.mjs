import assert from 'node:assert/strict';
import { createHash, webcrypto } from 'node:crypto';
import { crc32 } from 'node:zlib';
import { after, test } from 'node:test';
import { zipSync, strToU8 } from 'fflate';
import { createServer } from 'vite';

globalThis.crypto ??= webcrypto;
const server = await createServer({ configFile: false, server: { middlewareMode: true } });
after(() => server.close());
const { parseOfflinePackage } = await server.ssrLoadModule('/src/matches/offline-package.ts');
const { readOfflinePackage } = await server.ssrLoadModule('/src/matches/offline-reader.ts');
const { validateLibrary, validateMatch } = await server.ssrLoadModule('/src/matches/match-domain.ts');
const { seedTifoFlags, tifoPlane } = await server.ssrLoadModule('/src/placements/tifo.ts');
const { validLocalAd } = await server.ssrLoadModule('/src/placements/local-ad.ts');
const { flagModel } = await server.ssrLoadModule('/src/placements/flag.ts');
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
    ({ manifest }) => { manifest.packageVersion = 3; },
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
test('images over 4 MB, such as a full-size tifo, are read like any material', async () => {
  const data = fixture();
  const bytes = new Uint8Array(4 * 1024 * 1024 + 1);
  data.files['assets/ASSET-image.png'] = bytes;
  data.manifest.match.assets.find((asset) => asset.id === 'ASSET-image').byteLength = bytes.length;
  data.manifest.sha256['assets/ASSET-image.png'] = createHash('sha256').update(bytes).digest('hex');
  await assert.rejects(() => parseOfflinePackage(archive(data)), /Invalid PNG material/);
});

const MiB = 1024 * 1024, GiB = 1024 * MiB;
/**
 * A stored ZIP written with ZIP64 records only, as writers do for archives above 4 GiB. An entry's
 * `size` may claim more than its bytes, to describe a huge package without allocating it.
 */
function zip64(entries) {
  const parts = [], central = [];
  let offset = 0;
  const u64 = (value) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(value)); return b; };
  for (const { name, bytes, size = bytes.length } of entries) {
    const nameBytes = Buffer.from(name), crc = crc32(bytes);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(45, 4); local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(0xffffffff, 18); local.writeUInt32LE(0xffffffff, 22);
    local.writeUInt16LE(nameBytes.length, 26); local.writeUInt16LE(20, 28);
    const localExtra = Buffer.concat([Buffer.from([1, 0, 16, 0]), u64(size), u64(bytes.length)]);
    const record = Buffer.alloc(46);
    record.writeUInt32LE(0x02014b50, 0); record.writeUInt16LE(45, 4); record.writeUInt16LE(45, 6); record.writeUInt32LE(crc, 16);
    record.writeUInt32LE(0xffffffff, 20); record.writeUInt32LE(0xffffffff, 24); record.writeUInt16LE(nameBytes.length, 28);
    record.writeUInt16LE(28, 30); record.writeUInt32LE(0xffffffff, 42);
    central.push(record, nameBytes, Buffer.concat([Buffer.from([1, 0, 24, 0]), u64(size), u64(bytes.length), u64(offset)]));
    parts.push(local, nameBytes, localExtra, bytes);
    offset += 30 + nameBytes.length + 20 + bytes.length;
  }
  const directory = Buffer.concat(central);
  const end64 = Buffer.concat([Buffer.from([0x50, 0x4b, 6, 6]), u64(44), Buffer.from([45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    u64(entries.length), u64(entries.length), u64(directory.length), u64(offset)]);
  const locator = Buffer.concat([Buffer.from([0x50, 0x4b, 6, 7, 0, 0, 0, 0]), u64(offset + directory.length), Buffer.from([1, 0, 0, 0])]);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(0xffff, 8); end.writeUInt16LE(0xffff, 10);
  end.writeUInt32LE(0xffffffff, 12); end.writeUInt32LE(0xffffffff, 16);
  return new Uint8Array(Buffer.concat([...parts, directory, end64, locator, end]));
}
const packageEntries = ({ manifest, files }) => [...Object.entries(files).map(([name, bytes]) => ({ name, bytes })),
  { name: 'manifest.json', bytes: strToU8(JSON.stringify(manifest)) }];
/** Reads by offset, as the page server does, noting each range read. */
function tracked(bytes) {
  const reads = [];
  return { reads, source: { size: bytes.length, read: async (offset, length) => { reads.push([offset, length]); return bytes.subarray(offset, offset + length); } } };
}

test('a ZIP64 package, as written above 4 GiB, reads like any other', async () => {
  const result = await parseOfflinePackage(new Blob([zip64(packageEntries(fixture()))]));
  assert.equal(result.match.placements.length, 4);
  assert.deepEqual(new Uint8Array(await result.assets.get('ASSET-image').arrayBuffer()), image);
  const damaged = zip64(packageEntries(fixture()));
  damaged[80] ^= 1; // inside the first entry's bytes, after its 72-byte local header
  await assert.rejects(parseOfflinePackage(new Blob([damaged])), /damaged/);
});

test('materials above 5 GiB together, or 256 MiB alone, are refused from the ZIP directory before anything is unpacked (#214)', async () => {
  const data = fixture(), model = 'assets/ASSET-model.glb';
  const models = Array.from({ length: 21 }, (_, i) => ({ name: `assets/ASSET-m${i}.glb`, bytes: data.files[model], size: 255 * MiB }));
  const { reads, source } = tracked(zip64([...packageEntries(data), ...models]));
  await assert.rejects(readOfflinePackage(source),
    new RegExp(`materials total 5\\.2 GiB \\(${21 * 255 * MiB + image.length + data.files[model].length} bytes\\), over the offline package limit of 5 GiB \\(${5 * GiB} bytes\\)`));
  assert.equal(reads.length, 3, 'only the end records and the central directory are read');
  const large = tracked(zip64(packageEntries(data).map((entry) => entry.name === model ? { ...entry, size: 256 * MiB + 1 } : entry)));
  await assert.rejects(readOfflinePackage(large.source), new RegExp(`ASSET-model\\.glb is 256 MiB \\(${256 * MiB + 1} bytes\\); each may be at most 256 MiB`));
  assert.equal(large.reads.length, 3);
});

test('a package of more than 256 MiB of materials, such as a multi-ad 3D campaign, is read (#214)', async () => {
  const data = fixture(), models = ['ASSET-big-a', 'ASSET-big-b'];
  for (const id of models) {
    const bytes = new Uint8Array(130 * MiB), path = `assets/${id}.glb`;
    bytes.set(glb(), 0);
    new DataView(bytes.buffer).setUint32(8, bytes.length, true);
    data.files[path] = bytes;
    data.manifest.sha256[path] = createHash('sha256').update(bytes).digest('hex');
    data.manifest.match.assets.push({ id, name: id, contentType: 'model/gltf-binary', byteLength: bytes.length, contentPath: path });
  }
  const { assets } = await readOfflinePackage(new Blob([zipSync({ ...data.files, 'manifest.json': strToU8(JSON.stringify(data.manifest)) }, { level: 0 })]),
    (asset) => ({ id: asset.id, size: asset.bytes.length }));
  assert.deepEqual(assets.slice(2), models.map((id) => ({ id, size: 130 * MiB })));
});

const sha = (bytes) => createHash('sha256').update(bytes).digest('hex');
const ply = strToU8('ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nend_header\n0 0 0\n');
// The sample package's tifo (#210): a rectangle raked like a stand, top edge highest.
const corners = [[77.44936752319336, 58.89107131958008, -9.244537353515625], [49.511077880859375, 76.05392456054688, -9.966133117675781],
  [41.86606604495751, 63.909530554114426, -2.820715678007554], [69.80435568729149, 46.74667731314763, -2.0991199138473977]];
/** A Fusion v2 package: the v1 fixture plus the field's point cloud and a Tifo Ad with its own image. */
function fixtureV2() {
  const data = fixture(), { manifest, files } = data, match = manifest.match;
  manifest.packageVersion = 2;
  match.schemaVersion = 2;
  Object.assign(match.coordinates, { z: 'down', swingSpace: 'field', motionSpace: 'field', motionOrigin: 'neutral-model-bottom-center', motionScale: 'neutral-pose-multiplier' });
  files['point-cloud/field.ply'] = ply;
  manifest.sha256['point-cloud/field.ply'] = sha(ply);
  match.field.pointCloud = { fileName: 'stadium.ply', byteLength: ply.length, pointCount: 1, contentPath: 'point-cloud/field.ply' };
  files['assets/ASSET-tifo.png'] = image;
  manifest.sha256['assets/ASSET-tifo.png'] = sha(image);
  match.assets.push({ id: 'ASSET-tifo', name: 'Demo Ground 01 BLUE', contentType: 'image/png', byteLength: image.length, contentPath: 'assets/ASSET-tifo.png' });
  match.ads.push({ id: 'AD-tifo', name: 'tifo', type: 'Tifo', placements: [{ id: 'PLC-tifo', name: '2', coordinateSpace: 'point-cloud',
    x: 59.65771678407543, y: 61.40030093684726, width: 32.79683307816114, height: 16.03089211933219, assetRotation: 90, assetOpacity: 1,
    modelPose: null, assetIds: ['ASSET-tifo'], corners: corners.map(([x, y, z]) => ({ x, y, z })), tifoReveal: { direction: 'center-horizontal', duration: 2 } }] });
  return data;
}
const tifoPlacement = ({ manifest }) => manifest.match.ads[2].placements[0];

test('a v2 package brings its verified point cloud and maps its Tifo placements; 2D / 3D stay as in v1', async () => {
  const v1 = await parseOfflinePackage(archive(fixture()));
  const { id, match, pointCloud } = await parseOfflinePackage(archive(fixtureV2()));
  assert.equal(v1.match.pointCloud, undefined);
  assert.equal(v1.match.tifos, undefined);
  assert.deepEqual(match.placements.map(({ asset, ...slot }) => slot), v1.match.placements.map(({ asset, ...slot }) => slot));
  assert.deepEqual(new Uint8Array(await pointCloud.arrayBuffer()), ply);
  assert.deepEqual(match.pointCloud, { sha256: sha(ply), fileName: 'stadium.ply' });
  assert.deepEqual(match.tifos, [{ id: 'PLC-tifo', name: '2', adId: 'AD-tifo', adName: 'tifo',
    asset: { id: 'ASSET-tifo', name: 'Demo Ground 01 BLUE', contentType: 'image/png', url: `/fusion-offline/packages/${id}/assets/ASSET-tifo` },
    corners, width: 32.79683307816114, height: 16.03089211933219, assetRotation: 90, reveal: { direction: 'center_horizontal', duration: 2 } }]);
  // Stored and read back, as the match library does; libraries from before #210 have neither.
  assert.deepEqual(validateMatch(JSON.parse(JSON.stringify(match))), match);
  const record = { match, draft: match.placements, assetFiles: { 'ASSET-image': '/data/files/a.png', 'ASSET-model': '/data/files/m.glb', 'ASSET-tifo': '/data/files/t.png' },
    pointCloudFile: `/data/files/${sha(ply)}.ply` };
  assert.equal(validateLibrary({ version: 1, records: [record] })[0].pointCloudFile, record.pointCloudFile);
  assert.throws(() => validateLibrary({ version: 1, records: [{ ...record, assetFiles: { ...record.assetFiles, 'ASSET-tifo': undefined } }] }), /离线素材文件记录无效/);
  assert.throws(() => validateLibrary({ version: 1, records: [{ ...record, match: v1.match, draft: v1.match.placements, assetFiles: undefined }] }), /离线点云文件记录无效/);
});

test("a v2 package's point cloud is checked like a material, and v1 packages accept none of v2", async () => {
  for (const [change, error] of [
    [({ manifest }) => { manifest.sha256['point-cloud/field.ply'] = '0'.repeat(64); }, /Point cloud checksum failed/],
    [({ manifest }) => { manifest.match.field.pointCloud.byteLength++; }, /point cloud is missing or has invalid metadata/],
    [({ manifest }) => { manifest.match.field.pointCloud.contentPath = 'point-cloud/other.ply'; }, /point cloud is missing/],
    [({ manifest }) => { delete manifest.match.field.pointCloud; }, /unreferenced materials/],
    [({ files }) => { files['point-cloud/extra.ply'] = ply; }, /unreferenced materials/],
    [({ manifest }) => { manifest.match.coordinates.z = 'up'; }, /coordinate convention/],
    [({ manifest }) => { manifest.packageVersion = 1; }, /unreferenced materials/],
  ]) {
    const data = fixtureV2(); change(data);
    await assert.rejects(parseOfflinePackage(archive(data)), error);
  }
  const v1 = fixture();
  v1.manifest.match.ads.push(fixtureV2().manifest.match.ads[2]);
  await assert.rejects(parseOfflinePackage(archive(v1)), /Tifo placement PLC-tifo/);
});

test('a Tifo must be an opaque PNG / JPEG on a rectangle of its stated size, with a known reveal direction and a positive duration', async () => {
  for (const [change, error] of [
    [(data) => { tifoPlacement(data).corners[2].z += 0.01; }, /PLC-tifo: corners do not form a rectangle/],
    [(data) => { tifoPlacement(data).width += 0.01; }, /PLC-tifo: width × height/],
    [(data) => { tifoPlacement(data).assetOpacity = 0.5; }, /PLC-tifo: assetOpacity 0.5 is not supported/],
    [(data) => { tifoPlacement(data).assetRotation = 45; }, /PLC-tifo: assetRotation/],
    [(data) => { tifoPlacement(data).coordinateSpace = 'pitch'; }, /PLC-tifo: coordinateSpace/],
    [(data) => { tifoPlacement(data).tifoReveal.direction = 'diagonal'; }, /PLC-tifo: unsupported tifoReveal direction "diagonal"; expected one of left-to-right, /],
    [(data) => { tifoPlacement(data).tifoReveal.direction = 'left_to_right'; }, /PLC-tifo: unsupported tifoReveal direction "left_to_right"/],
    [(data) => { delete tifoPlacement(data).tifoReveal.direction; }, /PLC-tifo: unsupported tifoReveal direction undefined/],
    [(data) => { tifoPlacement(data).tifoReveal.duration = 0; }, /PLC-tifo: tifoReveal duration 0 must be a finite number of seconds above 0/],
    [(data) => { tifoPlacement(data).tifoReveal.duration = -1; }, /PLC-tifo: tifoReveal duration -1/],
    [(data) => { tifoPlacement(data).tifoReveal.duration = '2'; }, /PLC-tifo: tifoReveal duration "2"/],
    [(data) => { tifoPlacement(data).tifoReveal = 'left-to-right'; }, /PLC-tifo: tifoReveal must be an object or null/],
    [(data) => { tifoPlacement(data).assetIds = ['ASSET-model']; }, /PLC-tifo: needs exactly one PNG or JPEG/],
    [({ manifest, files }) => { delete manifest.match.field.pointCloud; delete manifest.sha256['point-cloud/field.ply']; delete files['point-cloud/field.ply']; },
      /Tifo placements need the offline package's point cloud/],
  ]) {
    const data = fixtureV2(); change(data);
    await assert.rejects(parseOfflinePackage(archive(data)), error);
  }
  const data = fixtureV2();
  delete tifoPlacement(data).assetOpacity;
  assert.equal((await parseOfflinePackage(archive(data))).match.tifos.length, 1);
});

test('each Fusion reveal direction keeps its direction and duration from the package to the seeded flag; no reveal stays static (#215)', async () => {
  const materials = { 'ASSET-tifo': { path: `/data/files/${sha(image)}.png`, sha256: sha(image) } };
  const imported = async (change) => {
    const data = fixtureV2(); change(tifoPlacement(data));
    const { match } = await parseOfflinePackage(archive(data));
    // Stored and read back as the match library does, then seeded as the import does.
    assert.deepEqual(validateMatch(JSON.parse(JSON.stringify(match))), match);
    const [seeded] = seedTifoFlags([], match, materials);
    assert.ok(validLocalAd(seeded));
    return [match.tifos[0], seeded];
  };
  for (const [fusion, direction, duration] of [['left-to-right', 'left_to_right', 2], ['right-to-left', 'right_to_left', 0.25],
    ['top-to-bottom', 'top_to_bottom', 90], ['bottom-to-top', 'bottom_to_top', 3600], ['center-horizontal', 'center_horizontal', 2.5],
    ['center-vertical', 'center_vertical', 7]]) {
    const [tifo, seeded] = await imported((placement) => { placement.tifoReveal = { direction: fusion, duration }; });
    assert.deepEqual(tifo.reveal, { direction, duration }, fusion);
    assert.deepEqual(seeded.reveal, { direction, duration, start: 'load' }, fusion);
    assert.equal(seeded.artworkRotation, 90, fusion);
  }
  for (const change of [(placement) => { delete placement.tifoReveal; }, (placement) => { placement.tifoReveal = null; }]) {
    const [tifo, seeded] = await imported(change);
    assert.equal('reveal' in tifo, false);
    assert.equal('reveal' in seeded, false);
  }
  for (const duration of [undefined, null]) {
    const [tifo] = await imported((placement) => { placement.tifoReveal = { direction: 'top-to-bottom', duration }; });
    assert.deepEqual(tifo.reveal, { direction: 'top_to_bottom', duration: 2 }, `duration ${duration}`);
  }
});

test("a tifo's corners give the flag plane directly: u along the top edge, v down, u x v away from its audience", () => {
  const plane = tifoPlane(corners);
  const cross = ([a, b, c], [d, e, f]) => [b * f - c * e, c * d - a * f, a * e - b * d];
  const close = (actual, expected, tolerance = 1e-9) => actual.forEach((value, i) => assert.ok(Math.abs(value - expected[i]) < tolerance, `${actual} vs ${expected}`));
  assert.deepEqual(plane.origin, corners[0]);
  close([plane.width, plane.height], [32.79683307816114, 16.03089211933219], 1e-6);
  close(plane.u.map((x, i) => corners[0][i] + x * plane.width), corners[1], 1e-6);
  close(plane.v.map((x, i) => corners[0][i] + x * plane.height), corners[3], 1e-6);
  close(cross(plane.u, plane.v), [0.217, 0.390, 0.895], 1e-3);
  // The flag's bottom-right corner lands on c2, half a metre in front of the stand (towards -normal).
  const rows = flagModel(plane, -0.5), n = cross(plane.u, plane.v);
  close([0, 1, 2].map((i) => rows[i][0] * plane.width + rows[i][1] * plane.height + rows[i][3]), corners[2].map((x, i) => x - 0.5 * n[i]), 1e-6);
});

test('importing seeds one editable flag per tifo, and importing again keeps the edited flag', async () => {
  const { match } = await parseOfflinePackage(archive(fixtureV2()));
  const materials = { 'ASSET-tifo': { path: `/data/files/${sha(image)}.png`, sha256: sha(image) } };
  const other = { id: 'mine', name: 'mine', type: '2D', space: 'pitch', plane: { origin: [0, 0, 0], u: [1, 0, 0], v: [0, 1, 0], width: 2, height: 1 },
    normalSign: -1, offset: 0, scale: 1, yaw: 0, visible: true };
  const [kept, seeded] = seedTifoFlags([other], match, materials);
  assert.equal(kept, other);
  assert.deepEqual({ ...seeded, plane: undefined }, { id: 'tifo-PLC-tifo', name: 'Tifo 2', type: 'flag', space: 'cloud', cloudId: sha(ply),
    plane: undefined, normalSign: -1, offset: -1.53 * 1.1, scale: 1, yaw: 0, visible: true,
    materialPath: materials['ASSET-tifo'].path, materialName: 'Demo Ground 01 BLUE', materialSha256: sha(image),
    flag: { amplitude: 1.1, frequency: 1.35, speed: 5.5, material: 'cloth' }, artworkRotation: 90,
    reveal: { direction: 'center_horizontal', duration: 2, start: 'load' }, aspectLocked: false, fusionTifoId: 'PLC-tifo' });
  assert.deepEqual(seeded.plane, tifoPlane(corners));
  assert.ok(validLocalAd(seeded));
  const edited = { ...seeded, name: 'Home end', plane: { ...seeded.plane, width: 20 }, reveal: { ...seeded.reveal, start: 'cue' } };
  assert.deepEqual(seedTifoFlags([other, edited], match, materials), [other, edited]);
  // A new tifo of a later package is added next to the edited one.
  const later = { ...match, tifos: [...match.tifos, { ...match.tifos[0], id: 'PLC-new' }] };
  assert.deepEqual(seedTifoFlags([other, edited], later, materials).map((ad) => ad.id), ['mine', 'tifo-PLC-tifo', 'tifo-PLC-new']);
});
