import { describe, expect, it } from "vitest";
import type { DirectHotkeyDef } from "./api";
import { hkSync, isHkDirty, type HkForm } from "./hotkeyForm";

const item = { provider: "apps", id: "a", title: "A" };
const def = (keys: string, i = item): DirectHotkeyDef => ({ keys, item: i });
const form = (keys: string, i = item): HkForm => ({ keys, item: i });

describe("hkSync", () => {
  it("does nothing while closed or adding", () => {
    expect(hkSync(false, "Ctrl+Alt+A", false, null, [def("Ctrl+Alt+A")]).kind).toBe("none");
    expect(hkSync(true, null, true, null, [def("Ctrl+Alt+A")]).kind).toBe("none");
  });

  it("does nothing when the saved copy still matches the file", () => {
    const s = form("Ctrl+Alt+A");
    expect(hkSync(true, "Ctrl+Alt+A", false, s, [def("Ctrl+Alt+A")]).kind).toBe("none");
    expect(hkSync(true, "Ctrl+Alt+A", true, s, [def("Ctrl+Alt+A")]).kind).toBe("none");
  });

  it("keeps an unsaved edit and shows the note", () => {
    const other = { ...item, id: "b", title: "B" };
    expect(hkSync(true, "Ctrl+Alt+A", true, form("Ctrl+Alt+A"), [def("Ctrl+Alt+A", other)])).toEqual({ kind: "note" });
    expect(hkSync(true, "Ctrl+Alt+A", true, form("Ctrl+Alt+A"), [])).toEqual({ kind: "note" });
  });

  it("refreshes a clean form from the file", () => {
    const other = { ...item, id: "b", title: "B" };
    expect(hkSync(true, "Ctrl+Alt+A", false, form("Ctrl+Alt+A"), [def("Ctrl+Alt+A", other)])).toEqual({
      kind: "reload",
      form: form("Ctrl+Alt+A", other),
    });
  });

  it("closes a clean form whose hotkey left the file", () => {
    expect(hkSync(true, "Ctrl+Alt+A", false, form("Ctrl+Alt+A"), [])).toEqual({ kind: "close" });
  });

  it("knows when the form is dirty", () => {
    expect(isHkDirty(null, { keys: "", item: null })).toBe(false);
    expect(isHkDirty(null, form("Ctrl+Alt+A"))).toBe(true);
    expect(isHkDirty(form("Ctrl+Alt+A"), form("Ctrl+Alt+A"))).toBe(false);
  });
});
