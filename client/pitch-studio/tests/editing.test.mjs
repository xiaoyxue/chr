import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { createServer } from 'vite';

// These modules have no dependencies to pre-bundle; skip the scan that would outlive the server.
const server = await createServer({ configFile: false, server: { middlewareMode: true }, optimizeDeps: { noDiscovery: true } });
after(() => server.close());
const { anchorPoint, moveAnchor, parseAspect, planeRectangle, rectanglePlane, resizeFromCorner, resizeSide, uniformScale, withAspect } =
  await server.ssrLoadModule('/src/placements/ground-edit.ts');
const { UndoStack } = await server.ssrLoadModule('/src/shared/undo-stack.ts');
const { upgradeLocalAd, validLocalAd, validatePitchAd, withoutPitchFlags } = await server.ssrLoadModule('/src/placements/local-ad.ts');
const { movePlaneAnchor, planeAnchor, planeWithAspect, resizePlane } = await server.ssrLoadModule('/src/placements/plane-edit.ts');

const near = (actual, expected) => {
  for (const [key, value] of Object.entries(expected)) assert.ok(Math.abs(actual[key] - value) < 1e-9, `${key}: ${actual[key]} vs ${value}`);
};
const quad = { x: 10, y: -4, sizeX: 8, sizeY: 2, rz: 0 };

test('the top-left anchor is the local -X/-Y corner, also when rotated', () => {
  near(anchorPoint(quad, 'top-left'), { x: 6, y: -5 });
  near(anchorPoint(quad, 'centre'), { x: 10, y: -4 });
  near(anchorPoint({ ...quad, rz: 90 }, 'top-left'), { x: 11, y: -8 });
});

test('moving an anchor places that point exactly and keeps the size', () => {
  const moved = moveAnchor(quad, 'top-left', { x: 0, y: 0 });
  near(moved, { x: 4, y: 1, sizeX: 8, sizeY: 2 });
  near(anchorPoint(moved, 'top-left'), { x: 0, y: 0 });
  near(moveAnchor({ ...quad, rz: 30 }, 'centre', { x: 1, y: 2 }), { x: 1, y: 2 });
});

test('resizing one side keeps the anchor and, when locked, the aspect ratio', () => {
  const locked = resizeSide(quad, 'top-left', 'sizeX', 12, true);
  near(locked, { sizeX: 12, sizeY: 3 });
  near(anchorPoint(locked, 'top-left'), { x: 6, y: -5 });
  near(resizeSide(quad, 'centre', 'sizeY', 4, true), { x: 10, y: -4, sizeX: 16, sizeY: 4 });
  near(resizeSide(quad, 'centre', 'sizeY', 4, false), { sizeX: 8, sizeY: 4 });
});

test('an aspect ratio is parsed from W:H or a number and sets Y from X at the anchor', () => {
  assert.equal(parseAspect('16:9'), 16 / 9);
  assert.equal(parseAspect(' 4 / 3 '), 4 / 3);
  assert.equal(parseAspect('2：1'), 2);
  assert.equal(parseAspect('1.5'), 1.5);
  for (const text of ['', '0', '-1', '16:0', 'a:b', '1:2:3']) assert.equal(parseAspect(text), undefined, text);
  const wide = withAspect(quad, 'top-left', 2);
  near(wide, { sizeX: 8, sizeY: 4 });
  near(anchorPoint(wide, 'top-left'), { x: 6, y: -5 });
  near(withAspect(quad, 'centre', 2), { x: 10, y: -4, sizeX: 8, sizeY: 4 });
});

test('a corner drag keeps the opposite corner and the locked ratio', () => {
  // Drag the bottom-right corner (14, -3) to (18, 0): the top-left (6, -5) stays.
  const free = resizeFromCorner(quad, [1, 1], { x: 18, y: 0 }, false);
  near(free, { sizeX: 12, sizeY: 5 });
  near(anchorPoint(free, 'top-left'), { x: 6, y: -5 });
  const locked = resizeFromCorner(quad, [1, 1], { x: 18, y: 0 }, true);
  assert.ok(Math.abs(locked.sizeX / locked.sizeY - 4) < 1e-9);
  near(anchorPoint(locked, 'top-left'), { x: 6, y: -5 });
  // Crossing over the fixed corner clamps to the minimum size instead of flipping.
  const collapsed = resizeFromCorner(quad, [1, 1], { x: 0, y: -10 }, true);
  near(collapsed, { sizeX: 0.4, sizeY: 0.1 });
  const rotated = resizeFromCorner({ ...quad, rz: 90 }, [-1, -1], { x: 12, y: -10 }, false);
  near(resizeFromCorner(rotated, [-1, -1], anchorPoint({ ...quad, rz: 90 }, 'top-left'), false), { sizeX: 8, sizeY: 2 });
});

test('uniform scale keeps the model proportions', () => {
  near(uniformScale({ scaleX: 1, scaleY: 2, scaleZ: 0.5 }, 1.5), { scaleX: 1.5, scaleY: 3, scaleZ: 0.75 });
  // Equal scales stay exactly equal, which bounded Fusion models require.
  const scaled = uniformScale({ scaleX: 1.3, scaleY: 1.3, scaleZ: 1.3 }, 1.7);
  assert.ok(scaled.scaleX === scaled.scaleY && scaled.scaleY === scaled.scaleZ);
});

test('ground ad planes round-trip through centred rectangles', () => {
  const plane = { origin: [-3, 2, -1], u: [1, 0, 0], v: [0, 1, 0], width: 6, height: 4 };
  const rect = planeRectangle(plane);
  near(rect, { x: 0, y: 4, sizeX: 6, sizeY: 4, rz: 0 });
  near(anchorPoint(rect, 'top-left'), { x: -3, y: 2 });
  const moved = rectanglePlane(plane, moveAnchor(rect, 'centre', { x: 10, y: 10 }));
  assert.deepEqual(moved.origin, [7, 8, -1]);
  assert.deepEqual([moved.u, moved.v], [plane.u, plane.v]);
});

test('undo restores the newest step first and keeps a step whose restore fails', async () => {
  const state = [];
  let changes = 0;
  const stack = new UndoStack(() => { changes += 1; }, 2);
  for (const name of ['a', 'b', 'c']) stack.push({ label: name, restore: () => { state.push(name); }, reapply: () => {} });
  assert.equal(stack.size, 2);
  assert.equal(stack.label, 'c');
  await stack.undo();
  assert.deepEqual(state, ['c']);
  stack.push({ label: 'busy', restore: () => { throw new Error('busy'); }, reapply: () => {} });
  await assert.rejects(stack.undo(), /busy/);
  assert.equal(stack.label, 'busy');
  stack.clear();
  assert.equal(stack.canUndo, false);
  await stack.undo();
  assert.deepEqual(state, ['c']);
  assert.ok(changes > 0);
});

test('stored flags must carry valid wave settings; older ads stay loadable', () => {
  const ad = { id: 'a', name: 'a', type: '2D', space: 'pitch', plane: { origin: [0, 0, 0], u: [1, 0, 0], v: [0, 1, 0], width: 2, height: 1 },
    normalSign: -1, offset: 0, scale: 1, yaw: 0, visible: true };
  const legacyCloud = { ...ad, type: '3D', space: undefined, cloudId: 'cloud' };
  const flag = { ...ad, type: 'flag', space: 'cloud', cloudId: 'cloud', flag: { amplitude: 0.32, frequency: 1.35, speed: 1, material: 'simple' } };
  for (const valid of [ad, legacyCloud, flag, { ...flag, flag: { ...flag.flag, material: 'pbr', artworkPath: '/srv/flag.png' } }, { ...flag, flag: { ...flag.flag, material: 'cloth' } }]) assert.ok(validLocalAd(valid));
  for (const [key, value] of [['amplitude', 5.5], ['frequency', Number.NaN], ['speed', -1], ['material', 'silk'], ['artworkPath', 'flag.png']]) {
    assert.equal(validLocalAd({ ...flag, flag: { ...flag.flag, [key]: value } }), false, key);
  }
  // A native flag's type and wind and fabric; its saved wave stays valid for switching back.
  const native = { ...flag, flag: { ...flag.flag, type: 'native', native: { wind: 7, direction: 25, autoDirection: true, tension: 65 } } };
  for (const valid of [native, { ...native, flag: { ...native.flag, native: undefined } }, { ...native, flag: { ...native.flag, type: 'default' } }]) assert.ok(validLocalAd(valid));
  for (const [key, value] of [['wind', 18.5], ['direction', 12.5], ['direction', -181], ['tension', 101], ['autoDirection', 'yes']]) {
    assert.equal(validLocalAd({ ...native, flag: { ...native.flag, native: { ...native.flag.native, [key]: value } } }), false, `${key} ${value}`);
  }
  assert.equal(validLocalAd({ ...native, flag: { ...native.flag, type: 'physical' } }), false);
  assert.equal(validLocalAd({ ...flag, flag: undefined }), false);
  assert.equal(validLocalAd({ ...ad, flag: flag.flag }), false);
  assert.ok(validLocalAd({ ...ad, materialPath: '/srv/logo.png' }));
  assert.equal(validLocalAd({ ...ad, materialPath: 'logo.png' }), false);
  // An early flag's artwork path becomes its material path.
  const early = { ...flag, flag: { ...flag.flag, artworkPath: '/srv/flag.png' } };
  assert.deepEqual(upgradeLocalAd(early), { ...flag, materialPath: '/srv/flag.png' });
  assert.equal(upgradeLocalAd(ad), ad);
  // A tifo flag's artwork rotation, reveal, aspect lock and source (#210); other ads have none.
  const tifo = { ...flag, artworkRotation: 270, reveal: { direction: 'center_horizontal', duration: 60, start: 'cue' }, aspectLocked: true, fusionTifoId: 'PLC-1' };
  assert.ok(validLocalAd(tifo));
  // Every direction, and durations past a minute as Fusion allows (#215).
  for (const direction of ['left_to_right', 'right_to_left', 'top_to_bottom', 'bottom_to_top', 'center_horizontal', 'center_vertical'])
    assert.ok(validLocalAd({ ...tifo, reveal: { direction, duration: 600, start: 'load' } }), direction);
  for (const [key, value] of [['artworkRotation', 45], ['reveal', { direction: 'center-horizontal', duration: 2, start: 'load' }],
    ['reveal', { direction: 'diagonal', duration: 2, start: 'load' }],
    ['reveal', { direction: 'center_horizontal', duration: 0, start: 'load' }], ['reveal', { direction: 'center_horizontal', duration: -1, start: 'load' }],
    ['reveal', { direction: 'center_horizontal', duration: Infinity, start: 'load' }],
    ['reveal', { direction: 'center_horizontal', duration: 2, start: 'later' }], ['aspectLocked', 'yes'], ['fusionTifoId', '../x']]) {
    assert.equal(validLocalAd({ ...tifo, [key]: value }), false, `${key} ${JSON.stringify(value)}`);
    assert.equal(validLocalAd({ ...ad, [key]: tifo[key] }), false, `${key} on a 2D ad`);
  }
});

test('flags are point-cloud only: stored pitch flags are dropped on load and refused on save', () => {
  const plane = { origin: [0, 0, 0], u: [1, 0, 0], v: [0, 1, 0], width: 2, height: 1 };
  const ad = { id: 'a', name: 'a', type: '2D', space: 'pitch', plane, normalSign: -1, offset: 0, scale: 1, yaw: 0, visible: true };
  const pitchFlag = { ...ad, id: 'f', type: 'flag', flag: { amplitude: 0.32, frequency: 1.35, speed: 1, material: 'simple' } };
  // An early record stays readable, so the store does not look corrupt.
  assert.ok(validLocalAd(pitchFlag));
  assert.deepEqual(withoutPitchFlags([ad, pitchFlag, { ...pitchFlag, id: 'g' }]), { kept: [ad], removed: 2 });
  assert.throws(() => validatePitchAd(pitchFlag, { length: 105, width: 68 }), /旗帜只能放在点云平面上/);
  validatePitchAd(ad, { length: 105, width: 68 });
});

test('a rectangle on any plane moves and resizes about its anchor, keeping its axes', () => {
  // A wall rectangle: u along +Y, v down (+Z).
  const plane = { origin: [50, -2, -3], u: [0, 1, 0], v: [0, 0, 1], width: 4, height: 2 };
  assert.deepEqual(planeAnchor(plane, 'centre'), [50, 0, -2]);
  assert.deepEqual(movePlaneAnchor(plane, 'centre', [50, 1, -2]).origin, [50, -1, -3]);
  const wider = resizePlane(plane, 'centre', 'width', 8, true);
  assert.deepEqual([wider.width, wider.height, planeAnchor(wider, 'centre')], [8, 4, [50, 0, -2]]);
  const fixed = resizePlane(plane, 'top-left', 'height', 3, false);
  assert.deepEqual([fixed.origin, fixed.width, fixed.height], [plane.origin, 4, 3]);
  assert.deepEqual(planeWithAspect(plane, 'top-left', 1).height, 4);
});

test('redo makes an undone step again, and a new step clears what can be redone', async () => {
  const state = [];
  const stack = new UndoStack();
  const step = (name) => ({ label: name, restore: () => { state.push(`undo ${name}`); }, reapply: () => { state.push(`redo ${name}`); } });
  stack.push(step('a'));
  stack.push(step('b'));
  await stack.undo();
  assert.equal(stack.redoLabel, 'b');
  await stack.redo();
  assert.deepEqual(state, ['undo b', 'redo b']);
  assert.equal(stack.label, 'b');
  await stack.undo();
  stack.push(step('c'));
  assert.equal(stack.canRedo, false);
  await stack.redo();
  assert.deepEqual(state, ['undo b', 'redo b', 'undo b']);
});
