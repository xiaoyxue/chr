// Thins a high-poly model for the page's 3D preview only. chr-server always renders
// the original GLB; the preview shows its placement, so a model above the budget
// keeps its shape and bounds with fewer triangles (meshoptimizer's edge collapse,
// which keeps UV and normal seams), and the page stays responsive.
import * as THREE from 'three';
import { MeshoptSimplifier } from 'meshoptimizer';

/** Triangles a previewed model keeps at most, shared by its meshes in proportion to their size. */
export const PREVIEW_TRIANGLES = 150_000;
/** The largest deviation a collapse may cause, relative to each mesh's extent. */
const MAX_ERROR = 0.01;

const triangles = (geometry: THREE.BufferGeometry) =>
  (geometry.index ? geometry.index.count : geometry.getAttribute('position').count) / 3;

/**
 * Simplifies the indexed meshes of `root` in place until the model has about `budget`
 * triangles, or as many as MAX_ERROR allows. Measure bounds before calling: the
 * simplified surface may move inside them by up to MAX_ERROR. Returns the triangle
 * counts before and after.
 */
export async function simplifyForPreview(root: THREE.Object3D, budget = PREVIEW_TRIANGLES) {
  const meshes: THREE.Mesh[] = [];
  root.traverse((object) => {
    if ((object as THREE.Mesh).isMesh && !(object as THREE.SkinnedMesh).isSkinnedMesh) meshes.push(object as THREE.Mesh);
  });
  const before = meshes.reduce((sum, mesh) => sum + triangles(mesh.geometry), 0);
  if (before <= budget) return { before, after: before };
  await MeshoptSimplifier.ready;
  const ratio = budget / before;
  let after = 0;
  // A geometry shared by several meshes is simplified once.
  const done = new Set<THREE.BufferGeometry>();
  for (const { geometry } of meshes) {
    if (done.has(geometry)) { after += triangles(geometry); continue; }
    done.add(geometry);
    const index = geometry.index, position = geometry.getAttribute('position');
    if (!index || geometry.morphAttributes.position || geometry.groups.length > 1) { after += triangles(geometry); continue; }
    const positions = new Float32Array(position.count * 3);
    for (let i = 0; i < position.count; i++) positions.set([position.getX(i), position.getY(i), position.getZ(i)], i * 3);
    const indices = index.array instanceof Uint32Array ? index.array : Uint32Array.from(index.array as ArrayLike<number>);
    const target = Math.max(3, Math.floor(indices.length / 3 * ratio) * 3);
    const [simplified] = MeshoptSimplifier.simplify(indices, positions, 3, target, MAX_ERROR);
    geometry.setIndex(new THREE.BufferAttribute(simplified, 1));
    after += simplified.length / 3;
  }
  return { before, after };
}
