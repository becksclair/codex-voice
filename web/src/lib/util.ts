/**
 * Clamp `value` into the inclusive `[min, max]` range.
 *
 * Ports the `clamp` helper from app.html (line ~1852). The argument order
 * matches the original — `Math.min(max, Math.max(min, value))` — so a `min`
 * greater than `max` yields `max`, exactly as the legacy app behaves.
 */
export function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}
