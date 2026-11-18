import { describe, expect, it } from "vitest";
import { TABS, TAB_LABELS, nextTab } from "./tabs";

describe("tabs", () => {
  it("has the nine tabs in order with the spec names", () => {
    expect(TABS).toEqual(["general", "appearance", "search", "answers", "shortcuts", "snippets", "workspaces", "hotkeys", "updates"]);
    expect(TABS.map((t) => TAB_LABELS[t])).toEqual(["General", "Appearance", "Search", "Quick answers", "Shortcuts", "Snippets", "Workspaces", "Hotkeys", "Updates"]);
  });

  it("moves to the next and previous tab", () => {
    expect(nextTab("general", +1)).toBe("appearance");
    expect(nextTab("appearance", +1)).toBe("search");
    expect(nextTab("search", -1)).toBe("appearance");
    expect(nextTab("answers", +1)).toBe("shortcuts");
    expect(nextTab("shortcuts", +1)).toBe("snippets");
    expect(nextTab("workspaces", -1)).toBe("snippets");
  });

  it("wraps at the ends", () => {
    expect(nextTab("updates", +1)).toBe("general");
    expect(nextTab("general", -1)).toBe("updates");
  });
});
