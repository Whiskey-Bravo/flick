import { describe, expect, it } from "vitest";
import { sameConfig } from "./sameConfig";

describe("sameConfig", () => {
  it("is true for equal content and false once a value differs", () => {
    expect(sameConfig({ hotkey: "Alt+Space", n: [1] }, { hotkey: "Alt+Space", n: [1] })).toBe(true);
    expect(sameConfig({ hotkey: "Alt+Space" }, { hotkey: "Ctrl+Space" })).toBe(false);
  });
});
