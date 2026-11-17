import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { askItems, REASK_MS } from "./pickerSearch";

describe("askItems", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("asks again once after an empty answer (a device list was still reading)", async () => {
    const answers = [[], ["Headphones"], ["never asked"]];
    const asked: string[] = [];
    const shown: string[][] = [];
    askItems("connect", async (t) => (asked.push(t), answers[asked.length - 1]), (rows) => shown.push(rows));
    await vi.advanceTimersByTimeAsync(0);
    expect(shown).toEqual([[]]);
    await vi.advanceTimersByTimeAsync(REASK_MS);
    expect(asked).toEqual(["connect", "connect"]);
    expect(shown).toEqual([[], ["Headphones"]]);
    await vi.advanceTimersByTimeAsync(REASK_MS * 5);
    expect(asked.length).toBe(2);
  });

  it("asks only once when rows came back, and twice at most when none ever do", async () => {
    let n = 0;
    askItems("notepad", async () => (n++, ["Notepad"]), () => {});
    await vi.advanceTimersByTimeAsync(REASK_MS * 3);
    expect(n).toBe(1);
    let m = 0;
    askItems("zzz", async () => (m++, []), () => {});
    await vi.advanceTimersByTimeAsync(REASK_MS * 3);
    expect(m).toBe(2);
  });

  it("a cancelled ask shows nothing and never asks again", async () => {
    const shown: string[][] = [];
    let n = 0;
    const cancel = askItems("wifi home", async () => (n++, []), (rows) => shown.push(rows));
    await vi.advanceTimersByTimeAsync(0);
    cancel();
    await vi.advanceTimersByTimeAsync(REASK_MS * 3);
    expect(n).toBe(1);
    expect(shown).toEqual([[]]);
  });
});
