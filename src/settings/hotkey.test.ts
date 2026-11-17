import { describe, expect, it } from "vitest";
import { directComboOk, hotkeyFromEvent, hotkeyKeys } from "./hotkey";

const ev = (key: string, code: string, mods: Partial<Record<"ctrlKey" | "altKey" | "shiftKey" | "metaKey", boolean>> = {}) => ({
  key,
  code,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  metaKey: false,
  ...mods,
});

describe("hotkeyFromEvent", () => {
  it("names a modifier plus key", () => {
    expect(hotkeyFromEvent(ev(" ", "Space", { altKey: true }))).toBe("Alt+Space");
    expect(hotkeyFromEvent(ev("k", "KeyK", { ctrlKey: true, shiftKey: true }))).toBe("Ctrl+Shift+K");
    expect(hotkeyFromEvent(ev("F1", "F1", { altKey: true }))).toBe("Alt+F1");
    expect(hotkeyFromEvent(ev("5", "Digit5", { ctrlKey: true }))).toBe("Ctrl+5");
  });
  it("orders modifiers Ctrl Alt Shift Super", () => {
    expect(hotkeyFromEvent(ev("F12", "F12", { metaKey: true, shiftKey: true, altKey: true, ctrlKey: true }))).toBe(
      "Ctrl+Alt+Shift+Super+F12",
    );
  });
  it("ignores a lone modifier, Escape and keys without Ctrl/Alt/Win", () => {
    expect(hotkeyFromEvent(ev("Alt", "AltLeft", { altKey: true }))).toBeNull();
    expect(hotkeyFromEvent(ev("Escape", "Escape"))).toBeNull();
    expect(hotkeyFromEvent(ev("Escape", "Escape", { altKey: true }))).toBeNull();
    expect(hotkeyFromEvent(ev("a", "KeyA"))).toBeNull();
    expect(hotkeyFromEvent(ev("A", "KeyA", { shiftKey: true }))).toBeNull();
  });
  it("accepts a bare function key", () => {
    expect(hotkeyFromEvent(ev("F5", "F5"))).toBe("F5");
  });
  it("ignores keys it cannot name", () => {
    expect(hotkeyFromEvent(ev("Dead", "IntlBackslash", { altKey: true }))).toBeNull();
  });
});

describe("hotkeyKeys", () => {
  it("splits for display", () => {
    expect(hotkeyKeys("Alt+Space")).toEqual(["Alt", "Space"]);
    expect(hotkeyKeys("Ctrl+Shift+K")).toEqual(["Ctrl", "Shift", "K"]);
    expect(hotkeyKeys("")).toEqual([]);
  });
});

describe("directComboOk", () => {
  it("accepts two modifiers or Win with one key", () => {
    for (const c of ["Ctrl+Alt+T", "Ctrl+Shift+K", "Alt+Shift+1", "Super+N", "Win+Shift+KeyT"]) {
      expect(directComboOk(c), c).toBe(true);
    }
  });
  it("rejects one modifier, none, or no key", () => {
    for (const c of ["Ctrl+T", "Alt+F4", "F5", "Shift+F5", "Ctrl+Alt", "", "Ctrl+Alt+T+K"]) {
      expect(directComboOk(c), c).toBe(false);
    }
  });
});
