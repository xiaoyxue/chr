import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { createServer } from 'vite';

const server = await createServer({ configFile: false, server: { middlewareMode: true } });
after(() => server.close());
const { motionRows, sampleMotion, validateModelPose, wireMotion } = await server.ssrLoadModule('/src/placements/model-motion.ts');
const { describeScene, glbModel } = await server.ssrLoadModule('/src/render/render-description.ts');

const close = (actual, expected, tolerance = 1e-9) =>
  actual.forEach((value, i) => assert.ok(Math.abs(value - expected[i]) <= tolerance, `${actual} vs ${expected}`));
const apply = (rows, [x, y, z]) => rows.slice(0, 3).map((row) => row[0] * x + row[1] * y + row[2] * z + row[3]);
const multiply = (a, b) => a.map((row) => [0, 1, 2, 3].map((c) => row.reduce((sum, v, k) => sum + v * b[k][c], 0)));
// The issue's example pose.
const example = { x: 0, y: 0, scale: 0.7, yaw: 30, swingAxis: 'z', swingAmplitude: 45, swingPeriod: 2, rotationMode: 'continuous',
  motion: { enabled: true, start: { x: -8, y: 2, z: 0, scale: 0.5 }, end: { x: 16, y: -6, z: -4, scale: 1.5 }, duration: 4, playback: 'ping-pong' } };

test('poses keep their animation fields; legacy poses stay static and legacy swing still swings', () => {
  assert.deepEqual(validateModelPose(example), example);
  const legacy = { x: 0.2, y: 0, scale: 0.5, yaw: 10 };
  assert.deepEqual(validateModelPose(legacy), legacy);
  assert.equal(wireMotion(legacy, [0, 0, 0]), undefined);
  const swing = wireMotion({ ...legacy, swingAxis: 'x', swingAmplitude: 30, swingPeriod: 4 }, [1, 2, 3]);
  assert.deepEqual(swing.rotation, { axis: [1, 0, 0], mode: 'swing', amplitude: Math.PI / 6, period: 4 });
  assert.equal(swing.path, undefined);
  // Disabled motion is kept for round trips but does not move the model.
  const disabled = { ...example, swingAxis: null, motion: { ...example.motion, enabled: false } };
  assert.deepEqual(validateModelPose(disabled), disabled);
  assert.equal(wireMotion(disabled, [0, 0, 0]), undefined);
  assert.deepEqual(wireMotion(example, [5, 6, -0.015]).path,
    { start: { offset: [-8, 2, 0], scale: 0.5 }, end: { offset: [16, -6, -4], scale: 1.5 }, duration: 4, playback: 'ping_pong' });
});

test('invalid animation settings name their field', () => {
  for (const [change, field] of [
    [{ swingAxis: 'w' }, /swingAxis/], [{ swingPeriod: 0.05 }, /swingPeriod/], [{ swingAmplitude: 200 }, /swingAmplitude/],
    [{ rotationMode: 'spin' }, /rotationMode/], [{ swingPeriod: undefined }, /swingPeriod/],
    [{ motion: { ...example.motion, duration: 0 } }, /duration/], [{ motion: { ...example.motion, playback: 'bounce' } }, /playback/],
    [{ motion: { ...example.motion, end: { x: 0, y: 0, z: 0, scale: 0 } } }, /end/],
  ]) assert.throws(() => validateModelPose({ ...example, ...change }), field, JSON.stringify(change));
});

test('ping-pong samples start, midpoint, end, midpoint and start every 2 s; once holds and loop restarts', () => {
  const motion = wireMotion(example, [0, 0, 0]);
  const expected = [[[-8, 2, 0], 0.5], [[4, -2, -2], 1], [[16, -6, -4], 1.5], [[4, -2, -2], 1], [[-8, 2, 0], 0.5]];
  expected.forEach(([offset, scale], i) => {
    const sample = sampleMotion(motion, 2 * i);
    close(sample.offset, offset);
    assert.ok(Math.abs(sample.scale - scale) < 1e-12);
  });
  const once = { ...motion, path: { ...motion.path, playback: 'once' } };
  assert.deepEqual(sampleMotion(once, 100).offset, [16, -6, -4]);
  // Once ends at 4 s, but the continuous rotation goes on: at 6.5 s it is a quarter into its fourth 2 s turn.
  close([sampleMotion(once, 6.5).angle], [Math.PI / 2]);
  const loop = { ...motion, path: { ...motion.path, playback: 'loop' } };
  close(sampleMotion(loop, 5).offset, sampleMotion(loop, 1).offset);
});

test('the path moves in field axes whatever the heading and rotation; scale and rotation keep the bottom centre', () => {
  const slot = { x: 5, y: 6, z: 0, rx: 0, ry: 0, rz: 0, id: 'car', name: 'car', type: '3D', region: { width: 20, height: 19 },
    modelPose: example, scaleX: 1, scaleY: 1, scaleZ: 1 };
  const bounds = { min: [-1, 0, -2], max: [1, 2, 2] };
  const neutral = glbModel(slot, bounds);
  const motion = wireMotion(example, apply(neutral, [0, 0, 0]));
  // chr's GLB basis puts the glTF bottom centre (0, 0, 0) at the asset origin.
  const bottom = apply(neutral, [0, 0, 0]);
  close(bottom, [5, 6, -0.015]);
  for (const t of [0, 1, 2, 3.3]) {
    const posed = multiply(motionRows(motion, t), neutral);
    const { offset } = sampleMotion(motion, t);
    close(apply(posed, [0, 0, 0]), bottom.map((v, i) => v + offset[i]));
  }
  // Negative z raises the model: at the end (4 s) its bottom centre is 4 m up.
  close(apply(multiply(motionRows(motion, 4), neutral), [0, 0, 0]), [21, 0, -4.015]);
});

test('a moving Fusion model is described with its motion about its bottom centre; a static one without', () => {
  const car = { id: 'ASSET-car', name: 'car', contentType: 'model/gltf-binary', url: '/x' };
  const slot = { x: 5, y: 6, z: 0, rx: 0, ry: 0, rz: 0, id: 'car', name: 'car', type: '3D', region: { width: 20, height: 19 },
    modelPose: example, scaleX: 1, scaleY: 1, scaleZ: 1, asset: car };
  const options = (slots) => ({ field: { length: 105, width: 68 }, slots, hidden: new Set(),
    geometry: new Map(slots.map((s) => [s.id, { kind: 'glb', bounds: { min: [-1, 0, -2], max: [1, 2, 2] } }])),
    assets: new Map([['ASSET-car', { kind: 'glb', entry: 'assets/car.glb', sha256: 'ab' }]]), packagePath: '/srv/m.zip' });
  const [moving] = describeScene(options([slot])).instances;
  close(moving.motion.pivot, [5, 6, -0.015]);
  assert.deepEqual(moving.motion.rotation, { axis: [0, 0, 1], mode: 'continuous', amplitude: Math.PI / 4, period: 2 });
  const [still] = describeScene(options([{ ...slot, modelPose: { x: 0, y: 0, scale: 0.7, yaw: 30 } }])).instances;
  assert.equal('motion' in still, false);
});
