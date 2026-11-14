import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { createServer } from 'vite';

const server = await createServer({ configFile: false, server: { middlewareMode: true } });
after(() => server.close());
const { RpcError, blendedMaterials, describeScene, glbModel, quadModel, rendererFailure, workerProblem } = await server.ssrLoadModule('/src/render/render-description.ts');
const { fitFusionModel } = await server.ssrLoadModule('/src/placements/fusion-creatives.ts');

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

test('placements share their asset and keep their visibility', () => {
  const ad = { id: 'A1', name: 'ad', contentType: 'image/png', url: '/x' };
  const slots = ['p1', 'p2', 'p3'].map((id, i) => ({ ...pose, id, name: id, type: '2D', x: i, sizeX: 2, sizeY: 1, assetRotation: 0, asset: i < 2 ? ad : undefined }));
  const scene = describeScene({
    field: { length: 105, width: 68 }, slots, hidden: new Set(['p2']),
    geometry: new Map(slots.map((slot) => [slot.id, { kind: 'image', width: 2, height: 1 }])),
    assets: new Map([['A1', { kind: 'image_quad', entry: 'assets/A1.png', sha256: 'ab' }]]),
    packagePath: '/srv/match.zip',
  });
  assert.deepEqual(scene.ground, { pitch_m: [105, 68] });
  assert.deepEqual(scene.assets, [{ id: 'A1', kind: 'image_quad', source: { package: '/srv/match.zip', entry: 'assets/A1.png' }, sha256: 'ab' }]);
  assert.deepEqual(scene.instances.map(({ id, asset, visible }) => [id, asset, visible]), [['p1', 'A1', true], ['p2', 'A1', false]]);
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
