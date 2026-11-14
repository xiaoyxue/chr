import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { createServer } from 'vite';

// These modules have no dependencies to pre-bundle; skip the scan that would outlive the server.
const server = await createServer({ configFile: false, server: { middlewareMode: true }, optimizeDeps: { noDiscovery: true } });
after(() => server.close());
const { anchorPoint, moveAnchor, parseAspect, planeRectangle, rectanglePlane, resizeFromCorner, resizeSide, uniformScale, withAspect } =
  await server.ssrLoadModule('/src/placements/ground-edit.ts');
const { UndoStack } = await server.ssrLoadModule('/src/shared/undo-stack.ts');

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
  for (const name of ['a', 'b', 'c']) stack.push({ label: name, restore: () => { state.push(name); } });
  assert.equal(stack.size, 2);
  assert.equal(stack.label, 'c');
  await stack.undo();
  assert.deepEqual(state, ['c']);
  stack.push({ label: 'busy', restore: () => { throw new Error('busy'); } });
  await assert.rejects(stack.undo(), /busy/);
  assert.equal(stack.label, 'busy');
  stack.clear();
  assert.equal(stack.canUndo, false);
  await stack.undo();
  assert.deepEqual(state, ['c']);
  assert.ok(changes > 0);
});
