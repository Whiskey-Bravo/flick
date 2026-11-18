import { describe, expect, it } from "vitest";
import type { ShortcutDef } from "./api";
import { disabledReasons, emptyForm, iconFromRow, isDirty, toDef, toForm } from "./shortcutForm";

const base: ShortcutDef = { name: "w", aliases: [], about: "", admin: false, choices: [] };

describe("shortcutForm", () => {
  it("round-trips open, run, wsl, pick and choices", () => {
    const defs: ShortcutDef[] = [
      { ...base, open: "https://en.wikipedia.org/wiki/{arg}", aliases: ["wiki", "wp"], about: "Wikipedia" },
      { ...base, run: "notepad", admin: true, terminal: false },
      { ...base, wsl: "htop", terminal: true },
      { ...base, open: "{path}", pick: "C:/Projects" },
      { ...base, run: "code {name}", choices: ["a", "b c"] },
      { ...base, open: "claude://claude.ai/new?q={arg}", icon: "shell:AppsFolder\\A!b" },
    ];
    for (const d of defs) expect(toDef(toForm(d))).toEqual(d);
  });

  it("an icon is kept trimmed, and a blank one is no icon", () => {
    const f = { ...emptyForm(), name: "w", target: "x" };
    expect(toDef({ ...f, icon: "  C:/i/a.png " }).icon).toBe("C:/i/a.png");
    expect("icon" in toDef({ ...f, icon: "   " })).toBe(false);
    expect(toForm({ name: "w", aliases: [], about: "", admin: false, choices: [], open: "x" }).icon).toBe("");
  });

  it("only a picked row with an icon of its own gives an icon", () => {
    expect(iconFromRow("file:C:\\p\\a.lnk")).toBe("C:\\p\\a.lnk");
    expect(iconFromRow("file:shell:AppsFolder\\A!b")).toBe("shell:AppsFolder\\A!b");
    expect(iconFromRow("builtin:web")).toBeNull();
    expect(iconFromRow("file:")).toBeNull();
    expect(iconFromRow("")).toBeNull();
  });

  it("maps the kind to the target key and the list mode to pick or choices", () => {
    expect(toForm({ ...base, wsl: "ls" })).toMatchObject({ kind: "wsl", target: "ls", listMode: "none" });
    expect(toForm({ ...base, run: "x", pick: "C:/p" })).toMatchObject({ listMode: "pick", pick: "C:/p" });
    expect(toForm({ ...base, run: "x", choices: ["a", "b"] })).toMatchObject({ listMode: "choices", choices: "a\nb" });
  });

  it("drops empty aliases and choices and an unset terminal", () => {
    const f = { ...emptyForm(), name: "w", target: "x", aliases: " a, b  c ,", choices: "", terminal: null };
    const d = toDef(f);
    expect(d.aliases).toEqual(["a", "b", "c"]);
    expect(d.choices).toEqual([]);
    expect("terminal" in d).toBe(false);
    expect(d.open).toBe("x");
    expect(toDef({ ...f, listMode: "choices", choices: "a\n\n  \nb\n" }).choices).toEqual(["a", "b"]);
  });

  it("ignores the inactive list mode field", () => {
    const d = toDef({ ...emptyForm(), name: "w", target: "{name}", listMode: "choices", choices: "a", pick: "C:/x" });
    expect(d.pick).toBeUndefined();
    expect(d.choices).toEqual(["a"]);
  });

  it("disables admin for wsl and for placeholders", () => {
    const f = { ...emptyForm(), kind: "run" as const, target: "notepad" };
    expect(disabledReasons(f, true).admin).toBeUndefined();
    expect(disabledReasons({ ...f, kind: "wsl" }, true).admin).toBe("not available for WSL commands");
    for (const p of ["{arg}", "{name}", "{path}"])
      expect(disabledReasons({ ...f, target: `x ${p}` }, true).admin).toBe("can't be combined with placeholders");
  });

  it("never disables a field that is set", () => {
    const f = { ...emptyForm(), kind: "wsl" as const, target: "x {arg}", admin: true };
    expect(disabledReasons(f, true).admin).toBeUndefined();
    expect(disabledReasons({ ...f, admin: false }, true).admin).toBeDefined();
    const p = { ...emptyForm(), target: "{arg}", listMode: "pick" as const };
    expect(disabledReasons(p, true).pick).toBeUndefined();
    expect(disabledReasons(p, true).choices).toBeDefined();
    expect(disabledReasons({ ...p, listMode: "choices" }, true).choices).toBeUndefined();
  });

  it("drops terminal for open shortcuts", () => {
    const f = { ...emptyForm(), name: "w", target: "x", terminal: true };
    expect("terminal" in toDef(f)).toBe(false);
    expect(toDef({ ...f, kind: "run" }).terminal).toBe(true);
  });

  it("disables the wsl kind without WSL", () => {
    const f = emptyForm();
    expect(disabledReasons(f, false).wsl).toBe("WSL is not installed");
    expect(disabledReasons(f, true).wsl).toBeUndefined();
  });

  it("disables lists next to {arg}", () => {
    const f = { ...emptyForm(), target: "x {arg}" };
    expect(disabledReasons(f, true).pick).toBe("can't be combined with {arg}");
    expect(disabledReasons(f, true).choices).toBe("can't be combined with {arg}");
  });

  it("tracks dirtiness", () => {
    const saved = toForm({ ...base, run: "x" });
    expect(isDirty(saved, toForm({ ...base, run: "x" }))).toBe(false);
    expect(isDirty(saved, { ...saved, about: "z" })).toBe(true);
    expect(isDirty(saved, { ...saved, about: "" })).toBe(false);
    expect(isDirty(null, emptyForm())).toBe(false);
    expect(isDirty(null, { ...emptyForm(), name: "n" })).toBe(true);
  });
});
