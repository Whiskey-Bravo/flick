import { describe, expect, it } from "vitest";
import { SEARCH_ENGINES, TEMPERATURE, engineChoice } from "./choices";

describe("engineChoice", () => {
  it("treats empty as DuckDuckGo", () => {
    expect(engineChoice("")).toBe("DuckDuckGo");
  });
  it("maps each preset to its label", () => {
    for (const e of SEARCH_ENGINES) expect(engineChoice(e.url)).toBe(e.label);
  });
  it("calls anything else Custom", () => {
    expect(engineChoice("https://example.com/?q={arg}")).toBe("Custom");
  });
  it("has the fixed presets and temperature options", () => {
    expect(SEARCH_ENGINES.map((e) => e.label)).toEqual(["DuckDuckGo", "Google", "Bing"]);
    expect(TEMPERATURE.map((t) => t[0])).toEqual(["", "c", "f"]);
  });
});
