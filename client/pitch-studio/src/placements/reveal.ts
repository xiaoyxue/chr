// Flag reveals (#210, #215): where a reveal opens a flag's rectangle, as chr's render
// description names it, and Fusion's tifoReveal. Directions are in the rectangle as
// the flag's corners lay it out (u right along the width, v down the height from the
// top-left corner), whatever the artwork rotation.
import type { FlagReveal, RevealDirection } from '../shared/types.ts';

export const REVEAL_DIRECTIONS: readonly RevealDirection[] = [
  'left_to_right', 'right_to_left', 'top_to_bottom', 'bottom_to_top', 'center_horizontal', 'center_vertical',
];
/** Fusion's tifoReveal.direction for each chr direction. */
const FUSION_DIRECTIONS: Record<string, RevealDirection> = Object.fromEntries(
  REVEAL_DIRECTIONS.map((direction) => [direction.replaceAll('_', '-'), direction]));
export const REVEAL_LABELS: Record<RevealDirection, string> = {
  left_to_right: '从左向右展开', right_to_left: '从右向左展开', top_to_bottom: '从上向下展开',
  bottom_to_top: '从下向上展开', center_horizontal: '中线水平展开', center_vertical: '中线垂直展开',
};
/** Fusion's duration when a tifoReveal gives none, in seconds. */
export const REVEAL_DURATION = 2;

export const isRevealDirection = (value: unknown): value is RevealDirection => REVEAL_DIRECTIONS.includes(value as RevealDirection);
/** A reveal's duration: finite seconds above 0, as Fusion and chr-server accept, with no upper cap. */
export const validRevealDuration = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value) && value > 0;
export const validReveal = (reveal: FlagReveal) => Boolean(reveal) && typeof reveal === 'object' && isRevealDirection(reveal.direction)
  && validRevealDuration(reveal.duration) && ['load', 'cue'].includes(reveal.start);

/** A package's tifoReveal as a flag reveal without its start; absent or null, the tifo is static. */
export function fusionReveal(value: unknown): Omit<FlagReveal, 'start'> | undefined {
  if (value === undefined || value === null) return undefined;
  if (typeof value !== 'object' || Array.isArray(value)) throw new Error('tifoReveal must be an object or null.');
  const { direction, duration: given } = value as Record<string, unknown>, duration = given ?? REVEAL_DURATION;
  const chr = typeof direction === 'string' && Object.hasOwn(FUSION_DIRECTIONS, direction) ? FUSION_DIRECTIONS[direction] : undefined;
  if (!chr) throw new Error(`unsupported tifoReveal direction ${JSON.stringify(direction)}; expected one of ${Object.keys(FUSION_DIRECTIONS).join(', ')}.`);
  if (!validRevealDuration(duration)) throw new Error(`tifoReveal duration ${JSON.stringify(duration)} must be a finite number of seconds above 0.`);
  return { direction: chr, duration };
}

/**
 * Flag point (`s` right, `t` down, both in [0, 1]) moved onto the part a reveal has
 * opened at `progress` in [0, 1], as the renderer's Flag::revealed_uv: what is not
 * shown yet collapses onto the edge of what is, so the artwork is cut, never squeezed.
 */
export function revealedPoint(s: number, t: number, direction: RevealDirection, progress: number): [number, number] {
  const centred = (x: number) => 0.5 + Math.min(Math.max(x - 0.5, -0.5 * progress), 0.5 * progress);
  switch (direction) {
    case 'center_horizontal': return [centred(s), t];
    case 'center_vertical': return [s, centred(t)];
    case 'left_to_right': return [Math.min(s, progress), t];
    case 'right_to_left': return [Math.max(s, 1 - progress), t];
    case 'top_to_bottom': return [s, Math.min(t, progress)];
    case 'bottom_to_top': return [s, Math.max(t, 1 - progress)];
  }
}
