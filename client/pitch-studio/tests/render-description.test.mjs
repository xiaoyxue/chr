import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { createServer } from 'vite';

const server = await createServer({ configFile: false, server: { middlewareMode: true } });
after(() => server.close());
const { RpcError, blendedMaterials, describeScene, sendDescription, glbModel, localGlbModel, localQuadModel, quadModel, rendererFailure, trackerProblem, workerProblem } = await server.ssrLoadModule('/src/render/render-description.ts');
const { fitFusionModel } = await server.ssrLoadModule('/src/placements/fusion-creatives.ts');
const { flagDraft, flagDraftChanged, flagModel, orientFlagPlane, parseFlagDraft } = await server.ssrLoadModule('/src/placements/flag.ts');
const { localRow, sendScope } = await server.ssrLoadModule('/src/placements/placement-list.ts');
const { fusionAssets, localMaterials } = await server.ssrLoadModule('/src/render/send-materials.ts');

const apply = (rows, [x, y, z]) => rows.slice(0, 3).map((row) => row[0] * x + row[1] * y + row[2] * z + row[3]);
const close = (actual, expected) => actual.forEach((value, i) => assert.ok(Math.abs(value - expected[i]) < 1e-9, `${actual} vs ${expected}`));
const pose = { x: 0, y: 0, z: 0, rx: 0, ry: 0, rz: 0 };

test('image quads fit their area like the preview, image top towards -Y', () => {
  const slot = { ...pose, id: 'ad', name: 'ad', type: '2D', x: 20, y: -5, sizeX: 14.7, sizeY: 21.03, assetRotation: 90 };
  const rows = quadModel(slot, { width: 100, height: 50 });
  // Rotated by 90 degrees, the image's 100-pixel width spans the 21.03 m side.
  close(apply(rows, [0.5, 0, 0]), [20, -5 + 21.03 / 2, -0.02]);
  close(apply(rows, [0, -0.5, 0]), [20 + 21.03 / 4, -5, -0.02]);
  close(apply(quadModel({ ...slot, assetRotation: 0 }, { width: 100, height: 50 }), [-0.5, -0.5, 0]), [20 - 7.35, -5 - 3.675, -0.02]);
});

test('a GLB stands on its fitted ground point, upright, turned by its yaw', () => {
  const slot = { ...pose, id: 'car', name: 'car', type: '3D', y: -0.085, region: { width: 20, height: 19 },
    modelPose: { x: 0, y: 0, scale: 0.7, yaw: 90 }, scaleX: 1, scaleY: 1, scaleZ: 1 };
  const bounds = { min: [-1, 0.5, -2], max: [1, 2, 2] };
  const fit = fitFusionModel(2, 4, slot);
  const rows = glbModel(slot, bounds);
  // glTF (x, y, z) arrives in chr as (x, z, -y) from the importer.
  close(apply(rows, [0, 0, -0.5]), [fit.x, fit.y, -0.015]);
  close(apply(rows, [0, 0, -2]), [fit.x, fit.y, -0.015 - 1.5 * fit.scale]);
  close(apply(rows, [1, 0, -0.5]), [fit.x, fit.y + fit.scale, -0.015]);
});

test("the studio's scale and rotation act about the neutral bottom centre without refitting", () => {
  const slot = { ...pose, id: 'car', name: 'car', type: '3D', x: 3, y: 1, region: { width: 20, height: 19 },
    modelPose: { x: 0.5, y: -0.5, scale: 0.7, yaw: 90 }, scaleX: 1, scaleY: 1, scaleZ: 1 };
  const bounds = { min: [-1, 0.5, -2], max: [1, 2, 2] };
  const neutral = fitFusionModel(2, 4, slot);
  const edited = { ...slot, rz: 45, scaleX: 3, scaleY: 3, scaleZ: 3 };
  const fit = fitFusionModel(2, 4, edited);
  // Larger than the region now, centred where Fusion put it, turned without shrinking.
  close([fit.x, fit.y, fit.scale, fit.yaw], [neutral.x, neutral.y, neutral.scale * 3, Math.PI * 3 / 4]);
  const rows = glbModel(edited, bounds);
  close(apply(rows, [0, 0, -0.5]), [neutral.x, neutral.y, -0.015]);
  close(apply(rows, [0, 0, -2]), [neutral.x, neutral.y, -0.015 - 1.5 * fit.scale]);
});

test('placements share their asset and keep their visibility', () => {
  const ad = { id: 'A1', name: 'ad', contentType: 'image/png', url: '/x' };
  const slots = ['p1', 'p2', 'p3'].map((id, i) => ({ ...pose, id, name: id, type: '2D', x: i, sizeX: 2, sizeY: 1, assetRotation: 0,
    assetOpacity: [0.25, 0.75, 1][i], asset: i < 2 ? ad : undefined }));
  const scene = describeScene({
    field: { length: 105, width: 68 }, slots, hidden: new Set(['p2']),
    geometry: new Map(slots.map((slot) => [slot.id, { kind: 'image', width: 2, height: 1 }])),
    assets: new Map([['A1', { kind: 'image_quad', entry: 'assets/A1.png', sha256: 'ab' }]]),
    packagePath: '/srv/match.zip',
  });
  assert.deepEqual(scene.ground, { pitch_m: [105, 68] });
  assert.deepEqual(scene.assets, [{ id: 'A1', kind: 'image_quad', source: { package: '/srv/match.zip', entry: 'assets/A1.png' }, sha256: 'ab' }]);
  assert.deepEqual(scene.instances.map(({ id, asset, visible, opacity }) => [id, asset, visible, opacity]), [['p1', 'A1', true, 0.25], ['p2', 'A1', false, 0.75]]);
});

test('alpha-blended GLB materials are found and opted into masks', () => {
  const json = Buffer.from(JSON.stringify({ materials: [{ name: 'paint' }, { name: 'badge', alphaMode: 'BLEND' }] }).padEnd(80, ' '));
  const glb = new Uint8Array(20 + json.length);
  new DataView(glb.buffer).setUint32(12, json.length, true);
  glb.set(json, 20);
  assert.deepEqual(blendedMaterials(glb), ['badge']);
  const car = { id: 'A2', name: 'car', contentType: 'model/gltf-binary', url: '/x' };
  const slot = { ...pose, id: 'c', name: 'c', type: '3D', region: { width: 10, height: 10 },
    modelPose: { x: 0, y: 0, scale: 1, yaw: 0 }, scaleX: 1, scaleY: 1, scaleZ: 1, asset: car };
  const scene = describeScene({
    field: { length: 105, width: 68 }, slots: [slot], hidden: new Set(),
    geometry: new Map([['c', { kind: 'glb', bounds: { min: [-1, 0, -1], max: [1, 1, 1] } }]]),
    assets: new Map([['A2', { kind: 'glb', entry: 'assets/A2.glb', sha256: 'cd', blendAs: 'mask' }]]),
    packagePath: '/srv/match.zip',
  });
  assert.equal(scene.assets[0].blend_as, 'mask');
});

// A wall at X = 50 seen from the pitch side (-X): screen right is +Y, screen down is +Z.
const wall = { origin: [50, -2, -3], u: [0, 1, 0], v: [0, 0, 1], width: 4, height: 2 };
const flagSettings = { amplitude: 0.32, frequency: 1.35, speed: 1, material: 'simple' };
const local = { name: 'ad', space: 'pitch', normalSign: -1, offset: 0, scale: 1, yaw: 0, visible: true };
// Flags exist on point-cloud planes only.
const onCloud = { space: 'cloud', cloudId: 'c1' }, confirmed = { loadedId: 'c1', alignment: { c1: true } }, noCloud = { alignment: {} };
const options = { field: { length: 105, width: 68 }, slots: [], hidden: new Set(), geometry: new Map(), assets: new Map(), packagePath: '/srv/match.zip' };

test('a picked flag plane stands upright and reads unmirrored from the picking side', () => {
  // Picked bottom-left, bottom-right, then above: v points up and u x v (-X) towards the viewer.
  const upwards = orientFlagPlane({ origin: [50, -2, -1], u: [0, 1, 0], v: [0, 0, -1], width: 4, height: 2 }, 1);
  // Picked top-left, top-right, then below: already oriented, u x v (+X) away from the viewer.
  for (const plane of [upwards, orientFlagPlane(wall, -1)]) {
    close(plane.origin, wall.origin);
    close(plane.u, wall.u);
    close(plane.v, wall.v);
    close([plane.width, plane.height], [4, 2]);
  }
});

test('seen from its audience, a flag runs u to the right and v down, with u x v pointing away', () => {
  // On the plane y = 0 viewed from -Y (up -Z), the viewer's right is (0, 1, 0) x (0, 0, -1) = -X.
  const expected = { origin: [3, 0, -3], u: [-1, 0, 0], v: [0, 0, 1] };
  // Picked right-to-left then upwards (u x v = -Y faces the viewer), and left-to-right then upwards (u x v = +Y).
  for (const [picked, normalSign] of [[{ origin: [3, 0, -1], u: [-1, 0, 0], v: [0, 0, -1] }, 1], [{ origin: [-1, 0, -1], u: [1, 0, 0], v: [0, 0, -1] }, -1]]) {
    const plane = orientFlagPlane({ ...picked, width: 4, height: 2 }, normalSign);
    for (const key of ['origin', 'u', 'v']) close(plane[key], expected[key]);
    const rows = flagModel(plane, -0.5);
    close(rows.slice(0, 3).map((row) => row[2]), [0, 1, 0]);
    // A negative offset moves the flag towards its audience at -Y.
    close(rows.slice(0, 3).map((row) => row[3]), [3, -0.5, -3]);
  }
});

test('a flag model is rigid: columns u, v, u x v and the offset top-left corner', () => {
  const rows = flagModel(wall, -0.5);
  const column = (j) => rows.slice(0, 3).map((row) => row[j]);
  close(column(0), [0, 1, 0]);
  close(column(1), [0, 0, 1]);
  close(column(2), [1, 0, 0]);
  close(column(3), [49.5, -2, -3]);
  assert.deepEqual(rows[3], [0, 0, 0, 1]);
  // Flag-local metres, Y down the flag: the bottom-right corner, 1 m above the ground.
  close(apply(rows, [4, 2, 0]), [49.5, 2, -1]);
});

test('flags with the same artwork share one asset but keep their own size and wave', () => {
  const sha = 'ab'.repeat(32);
  const ad = { ...local, ...onCloud, id: 'f1', type: 'flag', plane: wall, offset: -0.5, flag: flagSettings, materialPath: '/srv/flags/a.png' };
  const other = { ...ad, id: 'f2', visible: false, plane: { ...wall, width: 6, height: 3 },
    flag: { ...flagSettings, amplitude: 0.6, frequency: 2.5, speed: 0.4, material: 'pbr' } };
  const flags = [ad, other].map((item) => ({ ad: item, sha256: sha }));
  const scene = describeScene({ ...options, locals: flags });
  assert.deepEqual(scene.assets, [{ id: 'FLAG-abababababababab', kind: 'flag', source: { path: '/srv/flags/a.png' }, sha256: sha }]);
  assert.deepEqual(scene.instances.map(({ id, asset, visible, flag }) => [id, asset, visible, flag]), [
    ['flag-f1', 'FLAG-abababababababab', true, { width_m: 4, height_m: 2, amplitude_m: 0.32, frequency: 1.35, speed: 1, material: 'simple' }],
    ['flag-f2', 'FLAG-abababababababab', false, { width_m: 6, height_m: 3, amplitude_m: 0.6, frequency: 2.5, speed: 0.4, material: 'pbr' }],
  ]);
  assert.deepEqual(scene.instances[0].model, flagModel(wall, -0.5));
  const many = Array.from({ length: 64 }, (_, i) => ({ ad: { ...ad, id: `f${i}` }, sha256: sha }));
  assert.throws(() => describeScene({ ...options, locals: many }), /最多放置 63 个实例/);
});

test('a local 2D ad is the image quad stretched over its rectangle, top-left texel at the rectangle origin', () => {
  // On the pitch: u = +X, v = +Y (image top towards -Y as in the top view), lifted 15 mm up like the preview.
  const pitch = { ...local, id: 'p', type: '2D', plane: { origin: [10, 5, 0], u: [1, 0, 0], v: [0, 1, 0], width: 4, height: 2 } };
  const rows = localQuadModel(pitch);
  const column = (j) => rows.slice(0, 3).map((row) => row[j]);
  close(column(0), [4, 0, 0]);
  close(column(1), [0, 2, 0]);
  close(column(2), [0, 0, 1]);
  close(column(3), [12, 6, -0.015]);
  close(apply(rows, [-0.5, -0.5, 0]), [10, 5, -0.015]);
  // On a wall picked from -X with an offset: the quad's -Z side (the normal's back) faces the viewer.
  const wallAd = { ...local, id: 'w', type: '2D', space: 'cloud', cloudId: 'c', plane: wall, offset: -0.5 };
  const onWall = localQuadModel(wallAd);
  close(apply(onWall, [-0.5, -0.5, 0]), [49.485, -2, -3]);
  close(apply(onWall, [0.5, 0.5, 0]), [49.485, 2, -1]);
});

test('a local 3D model stands on its rectangle like the preview: centred, upright, fitted and turned by yaw', () => {
  const bounds = { min: [-1, 0, -0.5], max: [1, 3, 0.5] };
  const pitch = { ...local, id: 'm', type: '3D', plane: { origin: [0, 0, 0], u: [1, 0, 0], v: [0, 1, 0], width: 4, height: 2 } };
  // Footprint 2 × 1 m in a 4 × 2 m rectangle: scale 2. chr asset (x, y, z) is glTF (x, -z, y).
  const rows = localGlbModel(pitch, bounds);
  close(apply(rows, [0, 0, 0]), [2, 1, -0.015]); // the bottom centre of the footprint
  close(apply(rows, [0, 0, -3]), [2, 1, -0.015 - 6]); // glTF top (y = 3) is 6 m up
  close(apply(rows, [1, 0, 0]), [4, 1, -0.015]); // glTF +X along u
  close(apply(rows, [0, 0.5, 0]), [2, 2, -0.015]); // glTF +Z along v
  // Turned 90° about its own up axis it stays standing; glTF +X now runs along -v.
  const turned = localGlbModel({ ...pitch, yaw: 90 }, bounds);
  close(apply(turned, [1, 0, 0]), [2, -1, -0.015]);
  close(apply(turned, [0, 0, -3]), [2, 1, -0.015 - 6]);
  // On a wall picked from its +normal side, the model stands out of the wall towards the viewer.
  const wallModel = localGlbModel({ ...pitch, space: 'cloud', cloudId: 'c', plane: wall, normalSign: 1 }, bounds);
  close(apply(wallModel, [0, 0, -1]), [50 + 0.015 + 2, 0, -2]);
});

test('local materials become one asset per kind and content, read from their server paths', () => {
  const plane = { origin: [0, 0, 0], u: [1, 0, 0], v: [0, 1, 0], width: 2, height: 1 };
  const image = { ...local, id: 'i1', type: '2D', plane, materialPath: '/srv/a.png' };
  const model = { ...local, id: 'm1', type: '3D', plane, materialPath: '/srv/car.glb' };
  const flag = { ...local, ...onCloud, id: 'f1', type: 'flag', plane, flag: flagSettings, materialPath: '/srv/a.png' };
  const sha = 'cd'.repeat(32), glb = 'ef'.repeat(32), bounds = { min: [-1, 0, -1], max: [1, 1, 1] };
  const scene = describeScene({ ...options, locals: [
    { ad: { ...image, opacity: 0 }, sha256: sha }, { ad: { ...image, id: 'i2', materialPath: '/srv/copy-of-a.png' }, sha256: sha },
    { ad: model, sha256: glb, bounds, blendAs: 'mask' }, { ad: flag, sha256: sha },
  ] });
  assert.deepEqual(scene.assets, [
    { id: 'LOCAL-cdcdcdcdcdcdcdcd', kind: 'image_quad', source: { path: '/srv/a.png' }, sha256: sha },
    { id: 'LOCAL-efefefefefefefef', kind: 'glb', source: { path: '/srv/car.glb' }, sha256: glb, blend_as: 'mask' },
    { id: 'FLAG-cdcdcdcdcdcdcdcd', kind: 'flag', source: { path: '/srv/a.png' }, sha256: sha },
  ]);
  // Images carry their own opacity, 0.5 (#163) when they have none; models and flags none.
  assert.deepEqual(scene.instances.map(({ id, asset, opacity }) => [id, asset, opacity]), [
    ['local-i1', 'LOCAL-cdcdcdcdcdcdcdcd', 0], ['local-i2', 'LOCAL-cdcdcdcdcdcdcdcd', 0.5],
    ['local-m1', 'LOCAL-efefefefefefefef', undefined], ['flag-f1', 'FLAG-cdcdcdcdcdcdcdcd', undefined],
  ]);
});

/** Material access that answers hashes by path and records every download. */
function materialAccess(hashes, files = {}) {
  const reads = [];
  return { reads, sha256: async (path) => hashes[path], bounds: async () => ({ min: [-2, 0, -2], max: [2, 3, 2] }),
    read: async (path) => { reads.push(path); return new File([files[path]], path.split('/').pop()); } };
}
const blendedGlb = (() => {
  const json = new TextEncoder().encode(JSON.stringify({ materials: [{ name: 'glass', alphaMode: 'BLEND' }] }));
  const glb = new Uint8Array(20 + json.length);
  new DataView(glb.buffer).setUint32(12, json.length, true);
  glb.set(json, 20);
  return glb;
})();

test('a send describes Fusion materials from their stored hashes and noted blending, reading no file', async () => {
  const logo = { id: 'A1', name: 'logo', contentType: 'image/png', url: '/x' }, car = { id: 'A2', name: 'car', contentType: 'model/gltf-binary', url: '/y' };
  const slots = [{ ...pose, id: 'p1', asset: logo }, { ...pose, id: 'p2', asset: car }, { ...pose, id: 'p3', asset: logo }];
  const files = { A1: '/data/files/' + 'a1'.repeat(32) + '.png', A2: '/data/files/' + 'b2'.repeat(32) + '.glb' };
  const hashes = { [files.A1]: 'a1'.repeat(32), [files.A2]: 'b2'.repeat(32) }, access = materialAccess(hashes);
  const sent = await fusionAssets(slots, files, { A2: ['glass'] }, access);
  assert.deepEqual(access.reads, []);
  assert.equal(sent.assetBlended, undefined);
  assert.deepEqual([...sent.assets], [['A1', { kind: 'image_quad', entry: 'assets/A1.png', sha256: 'a1'.repeat(32) }],
    ['A2', { kind: 'glb', entry: 'assets/A2.glb', sha256: 'b2'.repeat(32), blendAs: 'mask' }]]);
  assert.deepEqual(sent.masked, ['car（glass）']);
  // A match imported before blending was noted reads its GLB once and returns the note to keep.
  const older = materialAccess(hashes, { [files.A2]: blendedGlb });
  const filled = await fusionAssets(slots, files, undefined, older);
  assert.deepEqual(older.reads, [files.A2]);
  assert.deepEqual(filled.assetBlended, { A2: ['glass'] });
});

test('a send describes local materials by their current server hash and reuses noted GLB facts only while it matches', async () => {
  const plane = { origin: [0, 0, 0], u: [1, 0, 0], v: [0, 1, 0], width: 2, height: 1 };
  const image = { ...local, id: 'i1', type: '2D', plane, materialPath: '/srv/a.png' };
  const noted = { materialSha256: 'cd'.repeat(32), materialBounds: { min: [-1, 0, -1], max: [1, 1, 1] }, materialBlended: [] };
  const model = { ...local, id: 'm1', type: '3D', plane, materialPath: '/srv/car.glb', ...noted };
  const access = materialAccess({ '/srv/a.png': 'ab'.repeat(32), '/srv/car.glb': 'cd'.repeat(32) }, { '/srv/car.glb': blendedGlb });
  const { locals } = await localMaterials([image, model, { ...image, id: 'i2' }], access);
  assert.deepEqual(access.reads, []);
  assert.deepEqual(locals.map(({ ad, ...facts }) => [ad.id, facts]), [['i1', { sha256: 'ab'.repeat(32) }],
    ['m1', { sha256: 'cd'.repeat(32), bounds: noted.materialBounds }], ['i2', { sha256: 'ab'.repeat(32) }]]);
  // The GLB changed on disk since it was chosen: its facts come from the file as it is now.
  const changed = materialAccess({ '/srv/car.glb': 'ee'.repeat(32) }, { '/srv/car.glb': blendedGlb });
  const after = await localMaterials([model, { ...model, id: 'm2' }], changed);
  assert.deepEqual(changed.reads, ['/srv/car.glb']);
  assert.deepEqual(after.locals[1], { ad: { ...model, id: 'm2' }, sha256: 'ee'.repeat(32), bounds: { min: [-2, 0, -2], max: [2, 3, 2] }, blendAs: 'mask' });
  assert.deepEqual(after.masked, ['ad（glass）', 'ad（glass）']);
});

test('the overall list summarises each local placement and says why it cannot be sent', () => {
  const flag = { ...local, ...onCloud, id: 'f1', name: '主旗', type: 'flag', plane: wall, flag: flagSettings, materialPath: '/srv/flags/a.png' };
  assert.deepEqual(localRow(flag, confirmed), { id: 'f1', space: 'cloud', type: 'flag', badge: '旗帜', name: '主旗', visible: true, place: '点云', size: '4 × 2 m',
    detail: '振幅 0.32 m · 频率 1.35 · 速度 1× · 简易光照', material: '/srv/flags/a.png', problem: undefined });
  assert.equal(localRow({ ...flag, materialPath: undefined }, confirmed).problem, '未设置图片');
  const model = { ...local, id: 'm', type: '3D', plane: wall, scale: 1.5, yaw: 30, materialPath: '/srv/car.glb' };
  assert.deepEqual([localRow(model, noCloud).size, localRow(model, noCloud).detail], ['底面 4 × 2 m', '缩放 ×1.5 · 朝向 30°']);
  // A material chosen on this computer has no server path for chr-server.
  const chosen = { ...local, id: 'i', type: '2D', plane: wall, material: new File([new Uint8Array(1)], 'logo.png') };
  assert.equal(localRow(chosen, noCloud).problem, '未设置服务器路径');
  assert.equal(localRow(chosen, noCloud).material, 'logo.png（未设置服务器路径）');
  assert.equal(localRow({ ...chosen, materialPath: '/srv/logo.webp' }, noCloud).problem, '渲染器只读取 PNG / JPEG 图片');
});

test("a point-cloud placement is sendable once its cloud was confirmed, without loading the cloud", () => {
  const cloud = { ...local, ...onCloud, id: 'm', type: '3D', plane: wall, materialPath: '/srv/car.glb' };
  // Loaded and confirmed: sendable, editable here.
  assert.deepEqual([localRow(cloud, confirmed).problem, localRow(cloud, confirmed).note], [undefined, undefined]);
  // Confirmed earlier (saved with the session), not loaded now: still sendable, with a note.
  const saved = localRow(cloud, { alignment: { c1: true } });
  assert.deepEqual([saved.problem, saved.note], [undefined, '点云已确认同坐标（未载入，可发送；编辑需载入点云）']);
  // Never confirmed, un-ticked, or loaded but not confirmed: not sendable.
  assert.equal(localRow(cloud, noCloud).problem, '所在点云未确认与球场同坐标（请载入该点云并确认）');
  assert.equal(localRow(cloud, { alignment: { c1: false } }).problem, '点云未确认与球场同坐标');
  assert.equal(localRow(cloud, { loadedId: 'c1', alignment: {} }).problem, '点云未确认与球场同坐标');
  // Another cloud's answer does not count.
  assert.ok(localRow(cloud, { loadedId: 'c2', alignment: { c2: true } }).problem);
});

test('local placements can be sent without an offline ZIP; Fusion placements need it', () => {
  assert.deepEqual(sendScope({ placements: 3, locals: 2 }), { fusion: true, note: '' });
  const alone = sendScope({ fusionProblem: '未填写离线 ZIP 路径', placements: 3, locals: 2 });
  assert.equal(alone.fusion, false);
  assert.equal(alone.blocker, undefined);
  assert.match(alone.note, /只发送 2 个本地广告位 \/ 旗帜，未发送 3 个 Fusion 广告位（未填写离线 ZIP 路径）.*替换 chr-server 的整个场景/);
  assert.match(sendScope({ fusionProblem: '未填写离线 ZIP 路径', placements: 3, locals: 0 }).blocker, /未填写离线 ZIP 路径/);
  // A ZIP match whose materials are still loading waits rather than dropping its placements.
  assert.match(sendScope({ waiting: '等待加载', placements: 3, locals: 2 }).blocker, /等待加载/);
});

test('typed flag fields are unsaved until they match the flag, and are range-checked on save', () => {
  const draft = flagDraft(flagSettings);
  assert.equal(flagDraftChanged(draft, flagSettings), false);
  assert.equal(flagDraftChanged({ ...draft, speed: '1.5' }, flagSettings), true);
  assert.equal(flagDraftChanged({ ...draft, amplitude: '' }, flagSettings), true);
  assert.equal(flagDraftChanged({ ...draft, material: 'pbr' }, flagSettings), true);
  assert.deepEqual(parseFlagDraft({ ...draft, speed: '1.5', material: 'pbr' }), { wave: { amplitude: 0.32, frequency: 1.35, speed: 1.5, material: 'pbr' } });
  assert.deepEqual(parseFlagDraft({ ...draft, amplitude: '0.9' }), { error: '振幅应介于 0 – 0.7。' });
  assert.deepEqual(parseFlagDraft({ ...draft, frequency: '' }), { error: '频率应介于 0.3 – 3。' });
});

test('sending is refused with a reason until the render worker is ready', () => {
  const status = { state: 'ready', pid: 7, restarts: 0, deadline_misses: 0, last_error: null };
  assert.equal(workerProblem(status), undefined);
  assert.match(workerProblem({ ...status, state: 'starting' }), /正在启动/);
  assert.match(workerProblem({ ...status, state: 'backoff', restarts: 3, last_error: 'device lost' }), /已重启 3 次.*device lost/);
});

test('renderer errors name the missing chr-server capability', () => {
  assert.match(rendererFailure('render.get_worker_status', new RpcError(-32601, 'Method not found')), /没有运行渲染 worker/);
  assert.match(rendererFailure('description.load', new RpcError(-32601, 'Method not found')), /--description 和 --asset-root/);
  assert.match(rendererFailure('description.load', new RpcError(-32602, 'asset outside root')), /错误 -32602.*asset outside root/);
  assert.equal(rendererFailure('sdi.get_status', new Error('timeout')), 'timeout');
});

test('sending is refused with a reason until the tracking worker is ready', () => {
  const status = { state: 'ready', pid: 9, restarts: 0, requests: 3, cameras: 3, unavailable: 0, not_sent: 0, last_error: null };
  assert.equal(trackerProblem(status), undefined);
  assert.match(trackerProblem({ ...status, state: 'starting' }), /跟踪 worker 正在启动/);
  assert.match(trackerProblem({ ...status, state: 'backoff', restarts: 2, last_error: 'exited' }), /已重启 2 次.*exited/);
});

test('sends reuse one control connection and reopen it after it drops', async () => {
  const sockets = [];
  const results = {
    'render.get_worker_status': { state: 'ready', pid: 1, restarts: 0 },
    'tracking.get_worker_status': { state: 'ready', pid: 2, restarts: 0 },
    'sdi.get_status': { video_mode: '1280x720p30' },
    'description.load': { scene_revision: '0', assets_imported: 0, assets_reused: 1 },
  };
  class FakeSocket {
    constructor(url) { this.url = url; sockets.push(this); setTimeout(() => this.onopen?.(), 0); }
    send(text) {
      const { id, method } = JSON.parse(text);
      setTimeout(() => this.onmessage?.({ data: JSON.stringify({ jsonrpc: '2.0', id, result: results[method] }) }), 0);
    }
    close() { setTimeout(() => this.onclose?.(), 0); }
  }
  const original = globalThis.WebSocket;
  globalThis.WebSocket = FakeSocket;
  try {
    const scene = { ground: { pitch_m: [105, 68] }, assets: [], instances: [] };
    await sendDescription('ws://studio/chr-render', scene);
    await sendDescription('ws://studio/chr-render', scene);
    assert.equal(sockets.length, 1, 'the second send reuses the open connection');
    sockets[0].close();
    await new Promise((resolve) => setTimeout(resolve, 5));
    const { loaded } = await sendDescription('ws://studio/chr-render', scene);
    assert.equal(sockets.length, 2, 'a dropped connection is opened again');
    assert.equal(loaded.assets_reused, 1);
  } finally {
    globalThis.WebSocket = original;
  }
});
