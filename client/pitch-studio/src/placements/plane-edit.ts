// Editing a local ad's rectangle within its own plane, the same way on the pitch
// (a ground plane) and on a point-cloud surface: the rectangle keeps its axes u, v;
// the top-left anchor is its origin, as in the top view's local (-X, -Y) corner.
import type { Anchor } from './ground-edit.ts';
import type { AdPlane, Vector3Tuple } from './local-ad.ts';

const along = (plane: AdPlane, s: number, t: number): Vector3Tuple =>
  [0, 1, 2].map((i) => plane.origin[i] + plane.u[i] * s + plane.v[i] * t) as Vector3Tuple;

export const planeAnchor = (plane: AdPlane, anchor: Anchor) =>
  anchor === 'centre' ? along(plane, plane.width / 2, plane.height / 2) : plane.origin;

/** Moves the rectangle so that its anchor lands on `point`. */
export function movePlaneAnchor(plane: AdPlane, anchor: Anchor, point: Vector3Tuple): AdPlane {
  const current = planeAnchor(plane, anchor);
  return { ...plane, origin: [0, 1, 2].map((i) => plane.origin[i] + point[i] - current[i]) as Vector3Tuple };
}

/** Sets one side and keeps the anchor fixed; with `keepAspect` the other side follows the ratio. */
export function resizePlane(plane: AdPlane, anchor: Anchor, side: 'width' | 'height', length: number, keepAspect: boolean): AdPlane {
  const ratio = plane.width / plane.height;
  const width = side === 'width' ? length : keepAspect ? length * ratio : plane.width;
  const height = side === 'height' ? length : keepAspect ? length / ratio : plane.height;
  return movePlaneAnchor({ ...plane, width, height }, anchor, planeAnchor(plane, anchor));
}

/** Sets width / height to `ratio` by changing the height; the width and the anchor stay. */
export const planeWithAspect = (plane: AdPlane, anchor: Anchor, ratio: number) => resizePlane(plane, anchor, 'height', plane.width / ratio, false);
