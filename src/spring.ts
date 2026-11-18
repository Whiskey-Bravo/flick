// The Wobble level's motion: a weight on a damped spring, stepped frame by
// frame. Because a move starts from the weight's current speed, quick moves
// one after another carry their momentum instead of each starting from rest.
// Pure maths; main.ts drives it from requestAnimationFrame.

export type Spring = {
  // Where the weight is, and how fast it moves (units per second).
  x: number;
  v: number;
};

export type Feel = {
  // How hard the spring pulls towards the target (per second squared).
  stiffness: number;
  // How strongly speed is braked (per second).
  damping: number;
};

// Overshoots by about a fifth of the way and settles in under half a second
// (damping ratio about 0.45): the highlight.
export const HIGHLIGHT: Feel = { stiffness: 260, damping: 14.5 };
// A little heavier and calmer (about 0.53): the list's height.
export const LIST: Feel = { stiffness: 200, damping: 15 };

// The longest stretch of time one call simulates: after a pause (a hidden
// window, a stalled frame) the weight carries on from where it was instead
// of leaping.
const LONGEST = 0.064;
// The simulation's own step: small enough to stay stable for the feels above.
const STEP = 0.004;

// The spring `seconds` later, pulled towards `target`.
export function stepSpring(s: Spring, target: number, seconds: number, feel: Feel): Spring {
  let { x, v } = s;
  let left = Math.min(Math.max(seconds, 0), LONGEST);
  while (left > 1e-9) {
    const h = Math.min(left, STEP);
    v += (-feel.stiffness * (x - target) - feel.damping * v) * h;
    x += v * h;
    left -= h;
  }
  return { x, v };
}

// Close enough to the target and slow enough to stop simulating.
export function settled(s: Spring, target: number): boolean {
  return Math.abs(s.x - target) < 0.05 && Math.abs(s.v) < 1;
}

// The highlight against an end of the list. Its spring would carry it past
// the first or last row; instead it stops at the edge and is squashed by the
// distance it would have overshot, like something soft pressed against a
// wall. `at` is where the spring is, `first` and `last` the positions of the
// first and last rows, `size` a row's height. Returns where to draw it, how
// tall (1 is full height, never less than `least`), and which edge it is
// pressed against (0 none, -1 the top, 1 the bottom).
export function pressed(
  at: number,
  first: number,
  last: number,
  size: number,
  least = 0.5,
): { at: number; tall: number; edge: -1 | 0 | 1 } {
  const over = at < first ? first - at : at > last ? at - last : 0;
  if (over === 0 || size <= 0) return { at, tall: 1, edge: 0 };
  return { at: at < first ? first : last, tall: Math.max(least, 1 - over / size), edge: at < first ? -1 : 1 };
}

// How much the highlight stretches along its direction of travel, as a
// share of its height: nothing at rest, growing with speed up to `most`.
export function stretch(v: number, most = 0.16, fullAt = 1400): number {
  return Math.min(most, (Math.abs(v) / fullAt) * most);
}
