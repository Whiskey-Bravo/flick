import { describe, expect, it } from "vitest";
import { oneAtATime } from "./oneAtATime";

describe("oneAtATime", () => {
  it("a second save while the first is on its way runs nothing", async () => {
    const busy: boolean[] = [];
    const run = oneAtATime((on) => busy.push(on));
    let finish = (_: boolean) => {};
    let n = 0;
    const save = () => (n++, new Promise<boolean>((r) => (finish = r)));
    const first = run(save);
    expect(await run(save)).toBe(false);
    expect(n).toBe(1);
    finish(true);
    expect(await first).toBe(true);
    expect(busy).toEqual([true, false]);
    // Once it ended, the next save runs.
    const third = run(save);
    finish(false);
    expect(await third).toBe(false);
    expect(n).toBe(2);
  });

  it("a save that throws still ends, so the next one runs", async () => {
    const run = oneAtATime(() => {});
    await expect(run(() => Promise.reject(new Error("boom")))).rejects.toThrow("boom");
    expect(await run(async () => true)).toBe(true);
  });
});
