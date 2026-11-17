import { describe, expect, it } from "vitest";
import type { ItemRef, PickRow, WorkspaceDef } from "./api";
import { addItem, emptyWsForm, isWsDirty, moveItem, removeItem, toWsDef, toWsForm } from "./workspaceForm";

const it1: ItemRef = { provider: "apps", id: "a", title: "A" };
const it2: ItemRef = { provider: "files", id: "b", title: "B" };
const it3: ItemRef = { provider: "games", id: "c", title: "C" };
const row = (r: ItemRef): PickRow => ({ ...r, subtitle: "s", icon: "i", confirm: false });

describe("workspaceForm", () => {
  it("round-trips a def", () => {
    const def: WorkspaceDef = { name: "work", aliases: ["w", "wk"], items: [it1, it2] };
    expect(toWsDef(toWsForm(def))).toEqual(def);
  });

  it("splits aliases on commas and spaces", () => {
    const f = { ...emptyWsForm(), name: "x", aliases: " w, wk  x " };
    expect(toWsDef(f).aliases).toEqual(["w", "wk", "x"]);
  });

  it("adds an item once", () => {
    const a = addItem([it1], row(it2));
    expect(a).toEqual([it1, it2]);
    expect(addItem(a, { ...row(it2), title: "other" })).toEqual([it1, it2]);
  });

  it("moves items and ignores the ends", () => {
    const l = [it1, it2, it3];
    expect(moveItem(l, 1, true)).toEqual([it2, it1, it3]);
    expect(moveItem(l, 1, false)).toEqual([it1, it3, it2]);
    expect(moveItem(l, 0, true)).toEqual(l);
    expect(moveItem(l, 2, false)).toEqual(l);
  });

  it("removes an item", () => {
    expect(removeItem([it1, it2, it3], 1)).toEqual([it1, it3]);
  });

  it("knows when the form is dirty", () => {
    expect(isWsDirty(null, emptyWsForm())).toBe(false);
    expect(isWsDirty(null, { ...emptyWsForm(), name: "a" })).toBe(true);
    const f = toWsForm({ name: "a", aliases: [], items: [it1] });
    expect(isWsDirty(f, toWsForm({ name: "a", aliases: [], items: [it1] }))).toBe(false);
    expect(isWsDirty(f, { ...f, aliases: "z" })).toBe(true);
  });
});
