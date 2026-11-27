// Fusion v2 Tifo placements (#210) as the studio's point-cloud flags. A tifo's
// corners are in the point cloud's coordinates, which are chr world coordinates
// (metres, pitch centre, z down), ordered top-left, top-right, bottom-right,
// bottom-left as the artwork reads; u = c1 - c0 and v = c3 - c0 then give the flag
// plane directly, its normal u × v pointing away from the reading side.
import { DEFAULT_FLAG, FLAG_CLEARANCE } from './flag.ts';
import type { AdPlane, LocalAd, Vector3Tuple } from './local-ad.ts';
import type { FusionTifo, Match } from '../shared/types.ts';

/** How far the corners may be from a rectangle of the placement's size, in metres. */
const RECTANGLE_TOLERANCE_M = 1e-3;
/** The largest cosine between adjacent edges. */
const PERPENDICULAR_TOLERANCE = 1e-4;

const minus = (a: Vector3Tuple, b: Vector3Tuple): Vector3Tuple => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
const dot = (a: Vector3Tuple, b: Vector3Tuple) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const scaled = (a: Vector3Tuple, s: number): Vector3Tuple => [a[0] * s, a[1] * s, a[2] * s];

/** Why the corners are not a `width` × `height` rectangle, or undefined when they are. */
export function rectangleProblem(corners: Vector3Tuple[], width: number, height: number): string | undefined {
  const [c0, c1, c2, c3] = corners, across = minus(c1, c0), down = minus(c3, c0);
  const [w, h] = [Math.hypot(...across), Math.hypot(...down)];
  if (minus(c0, c1).some((x, i) => Math.abs(x + c2[i] - c3[i]) > RECTANGLE_TOLERANCE_M)
    || w < RECTANGLE_TOLERANCE_M || h < RECTANGLE_TOLERANCE_M || Math.abs(dot(across, down)) / (w * h) > PERPENDICULAR_TOLERANCE)
    return 'corners do not form a rectangle (top-left, top-right, bottom-right, bottom-left).';
  if (Math.abs(w - width) > RECTANGLE_TOLERANCE_M || Math.abs(h - height) > RECTANGLE_TOLERANCE_M)
    return `width × height ${width} × ${height} m disagree with the corners (${w} × ${h} m).`;
}

/** The flag plane of a tifo's corners: top-left origin, u along the top edge, v down the left edge. */
export function tifoPlane(corners: Vector3Tuple[]): AdPlane {
  const [c0, c1, , c3] = corners, across = minus(c1, c0), width = Math.hypot(...across), u = scaled(across, 1 / width);
  const left = minus(c3, c0), height = Math.hypot(...left);
  // Exactly perpendicular, as a stored plane must be; the corners already are within tolerance.
  const v = minus(left, scaled(u, dot(left, u)));
  return { origin: c0, u, v: scaled(v, 1 / Math.hypot(...v)), width, height };
}

/** The local flag ID of a tifo; chr-server's instance is `flag-tifo-<placement>`, as the description converter names it. */
export const tifoFlagId = (tifo: Pick<FusionTifo, 'id'>) => `tifo-${tifo.id}`;

/**
 * The tifo as an editable point-cloud flag: its artwork is the package's stored image
 * (`material`: server path and SHA-256) stretched to the rectangle, revealed as Fusion
 * says (direction and duration) from the scene load or static without a tifoReveal, in
 * front of the surface like a flag picked from the reading side.
 */
export function tifoFlag(tifo: FusionTifo, cloudId: string, material: { path: string; sha256: string }): LocalAd {
  return {
    id: tifoFlagId(tifo), name: `Tifo ${tifo.name}`, type: 'flag', space: 'cloud', cloudId,
    plane: tifoPlane(tifo.corners), normalSign: -1, offset: -FLAG_CLEARANCE, scale: 1, yaw: 0, visible: true,
    materialPath: material.path, materialName: tifo.asset.name, materialSha256: material.sha256,
    flag: { ...DEFAULT_FLAG }, artworkRotation: tifo.assetRotation,
    ...(tifo.reveal ? { reveal: { ...tifo.reveal, start: 'load' } } : {}), aspectLocked: false, fusionTifoId: tifo.id,
  };
}

/**
 * The match's local placements with a flag for each of its tifos that has none yet.
 * A tifo already seeded keeps its flag as the operator edited it, so importing the
 * package again adds only new tifos. `materials` maps asset IDs to their stored files.
 */
export function seedTifoFlags(ads: readonly LocalAd[], match: Match, materials: Readonly<Record<string, { path: string; sha256: string }>>): LocalAd[] {
  const seeded = new Set(ads.flatMap((ad) => ad.fusionTifoId ? [ad.fusionTifoId] : []));
  const added = (match.tifos ?? []).filter((tifo) => !seeded.has(tifo.id) && !ads.some((ad) => ad.id === tifoFlagId(tifo)))
    .map((tifo) => tifoFlag(tifo, match.pointCloud.sha256, materials[tifo.asset.id]));
  return [...ads, ...added];
}
