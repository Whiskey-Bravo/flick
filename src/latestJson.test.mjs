import { describe, expect, it } from "vitest";
import { buildLatestJson } from "../scripts/latest-json.mjs";

describe("buildLatestJson", () => {
  it("writes the updater manifest", () => {
    const out = buildLatestJson("0.4.0", "Fixes", "abc", new Date("2026-11-17T10:00:00.000Z"));
    expect(out).toEqual({
      version: "0.4.0",
      notes: "Fixes",
      pub_date: "2026-11-17T10:00:00.000Z",
      platforms: {
        "windows-x86_64": {
          signature: "abc",
          url: "https://github.com/Whiskey-Bravo/flick/releases/download/v0.4.0/Flick_0.4.0_x64-setup.exe",
        },
      },
    });
  });
});
