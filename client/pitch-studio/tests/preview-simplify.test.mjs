import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { createServer } from 'vite';

const server = await createServer({ configFile: false, server: { middlewareMode: true } });
after(() => server.close());
const THREE = await server.ssrLoadModule('three');
const { simplifyForPreview } = await server.ssrLoadModule('/src/placements/preview-simplify.ts');

/** A finely tessellated sphere: smooth, so it simplifies well within the error bound. */
const sphere = (segments) => new THREE.Mesh(new THREE.SphereGeometry(1, segments, segments / 2), new THREE.MeshStandardMaterial());

test('a model over the budget keeps its bounds with about the budgeted triangles; one under it is untouched', async () => {
  const model = new THREE.Group();
  model.add(sphere(512), sphere(256));
  const bounds = new THREE.Box3().setFromObject(model);
  const { before, after } = await simplifyForPreview(model, 20_000);
  assert.ok(before > 300_000, `${before}`);
  assert.ok(after <= 20_000 && after > 5_000, `${after}`);
  // Edge collapses keep the surface within 1 % of its extent, so the fit does not change.
  const thinned = new THREE.Box3().setFromObject(model);
  for (const [a, b] of [[thinned.min, bounds.min], [thinned.max, bounds.max]])
    for (const axis of ['x', 'y', 'z']) assert.ok(Math.abs(a[axis] - b[axis]) < 0.02, `${axis}: ${a[axis]} vs ${b[axis]}`);
  const small = sphere(32), index = small.geometry.index;
  assert.deepEqual(await simplifyForPreview(small, 20_000), { before: index.count / 3, after: index.count / 3 });
  assert.equal(small.geometry.index, index);
});
