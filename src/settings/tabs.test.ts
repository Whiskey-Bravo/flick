import { describe, expect, it } from "vitest";
import { TABS, TAB_LABELS, nextTab } from "./tabs";

describe("tabs", () => {
  it("has the five tabs in order with the spec names", () => {
    expect(TABS).toEqual(["general", "search", "answers", "shortcuts", "updates"]);
    expect(TABS.map((t) => TAB_LABELS[t])).toEqual(["General", "Search", "Quick answers", "Shortcuts", "Updates"]);
  });

  it("moves to the next and previous tab", () => {
    expect(nextTab("general", +1)).toBe("search");
    expect(nextTab("search", -1)).toBe("general");
    expect(nextTab("answers", +1)).toBe("shortcuts");
  });

  it("wraps at the ends", () => {
    expect(nextTab("updates", +1)).toBe("general");
    expect(nextTab("general", -1)).toBe("updates");
  });
});
