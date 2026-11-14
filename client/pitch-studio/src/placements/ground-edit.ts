// Editing math for targets on the pitch plane, in chr world coordinates
// (pitch-centred metres, X along the length, Y across, ground Z = 0).
// The top view looks down with +X to the right and +Y down the screen, so a
// rectangle's "top-left" corner is its local (-X, -Y) corner.
import type { GroundRectangle, Point } from '../shared/types.ts';
import type { AdPlane } from './local-ad.ts';

export type Anchor = 'top-left' | 'centre';
export const MIN_SIZE = 0.1;

function rotate(x: number, y: number, degrees = 0): Point {
  const angle = degrees * Math.PI / 180, c = Math.cos(angle), s = Math.sin(angle);
  return { x: c * x - s * y, y: s * x + c * y };
}

/** The anchor's world position; `x`/`y` of the rectangle are its centre. */
export function anchorPoint(rect: GroundRectangle, anchor: Anchor): Point {
  if (anchor === 'centre') return { x: rect.x, y: rect.y };
  const offset = rotate(-rect.sizeX / 2, -rect.sizeY / 2, rect.rz);
  return { x: rect.x + offset.x, y: rect.y + offset.y };
}

/** Moves the rectangle so that its anchor lands on `point`. */
export function moveAnchor<T extends GroundRectangle>(rect: T, anchor: Anchor, point: Point): T {
  const current = anchorPoint(rect, anchor);
  return { ...rect, x: rect.x + point.x - current.x, y: rect.y + point.y - current.y };
}

/**
 * Sets one side length and keeps the anchor fixed. With `keepAspect` the other
 * side follows the current ratio.
 */
export function resizeSide<T extends GroundRectangle>(rect: T, anchor: Anchor, side: 'sizeX' | 'sizeY', length: number, keepAspect: boolean): T {
  const ratio = rect.sizeX / rect.sizeY;
  const sizeX = side === 'sizeX' ? length : keepAspect ? length * ratio : rect.sizeX;
  const sizeY = side === 'sizeY' ? length : keepAspect ? length / ratio : rect.sizeY;
  return moveAnchor({ ...rect, sizeX, sizeY }, anchor, anchorPoint(rect, anchor));
}

/** Width / height from "16:9", "16/9" or "1.78"; undefined unless finite and positive. */
export function parseAspect(text: string): number | undefined {
  const parts = text.trim().split(/\s*[:/：]\s*/).map(Number);
  const ratio = parts.length === 2 ? parts[0] / parts[1] : parts.length === 1 ? parts[0] : NaN;
  return Number.isFinite(ratio) && ratio > 0 ? ratio : undefined;
}

/** Sets width / height to `ratio` by changing the Y side; the X side and the anchor stay. */
export function withAspect<T extends GroundRectangle>(rect: T, anchor: Anchor, ratio: number): T {
  return resizeSide(rect, anchor, 'sizeY', rect.sizeX / ratio, false);
}

/**
 * Drags the corner at local signs `corner` (each -1 or 1) to `point` while the
 * opposite corner stays fixed. With `keepAspect` the size follows the projection
 * of the drag onto the diagonal, so the ratio never changes.
 */
export function resizeFromCorner<T extends GroundRectangle>(rect: T, corner: readonly [number, number], point: Point, keepAspect: boolean): T {
  const [sx, sy] = corner;
  const fixedOffset = rotate(-sx * rect.sizeX / 2, -sy * rect.sizeY / 2, rect.rz);
  const fixed = { x: rect.x + fixedOffset.x, y: rect.y + fixedOffset.y };
  const local = rotate(point.x - fixed.x, point.y - fixed.y, -(rect.rz ?? 0));
  let sizeX = sx * local.x, sizeY = sy * local.y;
  if (keepAspect) {
    const factor = (sizeX * rect.sizeX + sizeY * rect.sizeY) / (rect.sizeX ** 2 + rect.sizeY ** 2);
    const scale = Math.max(factor, MIN_SIZE / Math.min(rect.sizeX, rect.sizeY));
    sizeX = rect.sizeX * scale;
    sizeY = rect.sizeY * scale;
  } else {
    sizeX = Math.max(sizeX, MIN_SIZE);
    sizeY = Math.max(sizeY, MIN_SIZE);
  }
  const centre = rotate(sx * sizeX / 2, sy * sizeY / 2, rect.rz);
  return { ...rect, x: fixed.x + centre.x, y: fixed.y + centre.y, sizeX, sizeY };
}

/** Scales all three model axes by one factor so that X becomes `scale`, keeping their proportions. */
export function uniformScale<T extends { scaleX: number; scaleY: number; scaleZ: number }>(slot: T, scale: number): T {
  const follow = (value: number) => value === slot.scaleX ? scale : value * scale / slot.scaleX;
  return { ...slot, scaleX: scale, scaleY: follow(slot.scaleY), scaleZ: follow(slot.scaleZ) };
}

/** A ground ad plane as a centred rectangle; its `u` axis gives the rotation. */
export function planeRectangle(plane: AdPlane): GroundRectangle {
  const { origin, u, v, width, height } = plane;
  return { x: origin[0] + (u[0] * width + v[0] * height) / 2, y: origin[1] + (u[1] * width + v[1] * height) / 2,
    sizeX: width, sizeY: height, rz: Math.atan2(u[1], u[0]) * 180 / Math.PI };
}

/** The plane with the rectangle's centre and size, keeping its axes and height. */
export function rectanglePlane(plane: AdPlane, rect: GroundRectangle): AdPlane {
  const { u, v } = plane;
  return { ...plane, width: rect.sizeX, height: rect.sizeY, origin: [
    rect.x - (u[0] * rect.sizeX + v[0] * rect.sizeY) / 2,
    rect.y - (u[1] * rect.sizeX + v[1] * rect.sizeY) / 2,
    plane.origin[2],
  ] };
}
