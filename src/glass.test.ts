import { describe, it, expect } from "vitest";
import { LENS_EDGE, inkIsDark, lensMap } from "./glass";

describe("glass", () => {
  const W = 200;
  const H = 120;
  const map = lensMap(W, H, 22);
  const at = (x: number, y: number) => [...map.slice((y * W + x) * 4, (y * W + x) * 4 + 4)];

  it("leaves the middle of the panel alone", () => {
    expect(at(100, 60)).toEqual([128, 128, 128, 255]);
    // Just past the edge zone, from every side.
    expect(at(LENS_EDGE + 1, 60)).toEqual([128, 128, 128, 255]);
    expect(at(100, H - LENS_EDGE - 2)).toEqual([128, 128, 128, 255]);
  });

  it("pulls the rim's picture in from further inside", () => {
    // Left rim: shifted right (red above 128), no vertical shift.
    const left = at(0, 60);
    expect(left[0]).toBeGreaterThan(240);
    expect(left[1]).toBe(128);
    // Right rim: shifted left.
    expect(at(W - 1, 60)[0]).toBeLessThan(16);
    // Top rim: shifted down; bottom rim: shifted up.
    expect(at(100, 0)[1]).toBeGreaterThan(240);
    expect(at(100, H - 1)[1]).toBeLessThan(16);
    expect(at(100, 0)[0]).toBe(128);
  });

  it("eases off smoothly towards the inside", () => {
    const shifts = [0, 4, 8, 12, 16, 20].map((x) => at(x, 60)[0] - 128);
    for (let i = 1; i < shifts.length; i++) expect(shifts[i]).toBeLessThan(shifts[i - 1]);
    expect(shifts[shifts.length - 1]).toBeGreaterThanOrEqual(0);
  });

  it("bends a corner along its diagonal", () => {
    // Near the top-left corner's curve: pushed right and down alike.
    const [r, g] = at(8, 8);
    expect(r).toBeGreaterThan(128);
    expect(Math.abs(r - g)).toBeLessThanOrEqual(1);
  });

  it("is every pixel of the panel, opaque", () => {
    expect(map.length).toBe(W * H * 4);
    for (let i = 3; i < map.length; i += 4 * 97) expect(map[i]).toBe(255);
    // A size of nothing still gives one pixel, not an error.
    expect(lensMap(0, 0, 22).length).toBe(4);
  });

  it("writes in dark ink only on a light picture", () => {
    expect(inkIsDark(240)).toBe(true);
    expect(inkIsDark(151)).toBe(true);
    expect(inkIsDark(150)).toBe(false);
    expect(inkIsDark(20)).toBe(false);
  });
});
