import { describe, it, expect } from "vitest";
import { HIGHLIGHT, LIST, settled, stepSpring, stretch, type Spring } from "./spring";

// Runs the spring at 60 frames a second and returns every position on the way.
function run(from: Spring, target: number, seconds: number, feel = HIGHLIGHT): { path: number[]; end: Spring } {
  let s = from;
  const path: number[] = [];
  for (let t = 0; t < seconds; t += 1 / 60) {
    s = stepSpring(s, target, 1 / 60, feel);
    path.push(s.x);
  }
  return { path, end: s };
}

describe("spring", () => {
  it("reaches its target and comes to rest", () => {
    const { end } = run({ x: 0, v: 0 }, 44, 1);
    expect(end.x).toBeCloseTo(44, 1);
    expect(settled(end, 44)).toBe(true);
    expect(settled({ x: 0, v: 0 }, 44)).toBe(false);
    expect(settled({ x: 44, v: 30 }, 44)).toBe(false);
  });

  it("overshoots only a little", () => {
    for (const feel of [HIGHLIGHT, LIST]) {
      const { path } = run({ x: 0, v: 0 }, 100, 1.2, feel);
      const peak = Math.max(...path);
      expect(peak).toBeGreaterThan(100);
      expect(peak).toBeLessThan(106);
      // And it never swings back far below the target once it has passed it.
      const after = path.slice(path.findIndex((x) => x >= 100));
      expect(Math.min(...after)).toBeGreaterThan(98);
    }
  });

  it("carries its speed into the next move", () => {
    // Half way to the first target, the target moves on: the weight is
    // already moving, so it gets there sooner than one starting from rest.
    const moving = run({ x: 0, v: 0 }, 44, 0.08).end;
    expect(moving.v).toBeGreaterThan(100);
    const carried = run(moving, 88, 0.1).end;
    const fromRest = run({ x: moving.x, v: 0 }, 88, 0.1).end;
    expect(carried.x).toBeGreaterThan(fromRest.x);
  });

  it("keeps going past a target that turns round", () => {
    // Moving down fast, then told to go back up: it still travels down a
    // little before it turns. That is the inertia.
    const moving = run({ x: 0, v: 0 }, 200, 0.1).end;
    const { path } = run(moving, 0, 0.3);
    expect(Math.max(...path)).toBeGreaterThan(moving.x);
  });

  it("is steady whatever the frame rate, and after a long pause", () => {
    const fast = run({ x: 0, v: 0 }, 100, 0.5).end;
    let slow: Spring = { x: 0, v: 0 };
    for (let t = 0; t < 0.5; t += 1 / 20) slow = stepSpring(slow, 100, 1 / 20, HIGHLIGHT);
    expect(slow.x).toBeCloseTo(fast.x, 0);
    // A frame that took ten seconds simulates only a moment: no leap, no NaN.
    const paused = stepSpring({ x: 0, v: 0 }, 100, 10, HIGHLIGHT);
    expect(paused.x).toBeGreaterThan(0);
    expect(paused.x).toBeLessThan(100);
    expect(stepSpring({ x: 5, v: 3 }, 100, 0, HIGHLIGHT)).toEqual({ x: 5, v: 3 });
    expect(stepSpring({ x: 5, v: 3 }, 100, -1, HIGHLIGHT)).toEqual({ x: 5, v: 3 });
  });

  it("stretches with speed, up to a limit", () => {
    expect(stretch(0)).toBe(0);
    expect(stretch(900)).toBeCloseTo(0.035, 5);
    expect(stretch(-900)).toBeCloseTo(0.035, 5);
    expect(stretch(1800)).toBeCloseTo(0.07, 5);
    expect(stretch(99999)).toBeCloseTo(0.07, 5);
  });
});
