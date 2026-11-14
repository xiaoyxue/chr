import type { FieldDimensions, GroundRectangle, Point } from '../../shared/types.ts';

export function containsGroundPlacement(slot: GroundRectangle, point: Point) {
  const angle = -(slot.rz ?? 0) * Math.PI / 180;
  const x = point.x - slot.x, y = point.y - slot.y;
  return Math.abs(x * Math.cos(angle) - y * Math.sin(angle)) <= slot.sizeX / 2
    && Math.abs(x * Math.sin(angle) + y * Math.cos(angle)) <= slot.sizeY / 2;
}

export function isPlacementInside(ad: GroundRectangle, { length, width }: FieldDimensions) {
  const angle = (ad.rz ?? 0) * Math.PI / 180;
  const halfX = (Math.abs(Math.cos(angle)) * ad.sizeX + Math.abs(Math.sin(angle)) * ad.sizeY) / 2;
  const halfY = (Math.abs(Math.sin(angle)) * ad.sizeX + Math.abs(Math.cos(angle)) * ad.sizeY) / 2;
  return [ad.x, ad.y, ad.sizeX, ad.sizeY].every(Number.isFinite)
    && Number.isFinite(angle)
    && ad.sizeX > 0 && ad.sizeY > 0
    && Math.abs(ad.x) + halfX <= length / 2 + 1e-7
    && Math.abs(ad.y) + halfY <= width / 2 + 1e-7;
}
