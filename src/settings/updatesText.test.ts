import { describe, expect, it } from "vitest";
import { firstLines, lastCheckedText } from "./updatesText";

describe("firstLines", () => {
  it("keeps the first three non-empty lines", () => {
    expect(firstLines("- a\r\n\r\n- b\n- c\n- d")).toEqual(["- a", "- b", "- c"]);
  });
  it("is empty for empty notes", () => {
    expect(firstLines("")).toEqual([]);
  });
});

describe("lastCheckedText", () => {
  it("says so when never checked", () => {
    expect(lastCheckedText(null)).toBe("Not checked yet");
    expect(lastCheckedText("garbage")).toBe("Not checked yet");
  });
  it("shows a local time", () => {
    expect(lastCheckedText("2026-11-17T18:00:00Z")).toMatch(/^Last checked: .+/);
  });
});
