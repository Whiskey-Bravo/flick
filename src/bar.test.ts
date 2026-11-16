import { describe, it, expect } from "vitest";
import { failureText, initial, reduce, visibleItems, type Effect, type Event, type Item, type State } from "./bar";

const item = (id: string, confirm = false): Item => ({
  id, provider: "apps", title: id, subtitle: "", confirm,
});
const run = (s: State, ...events: Event[]) => {
  let state = s;
  let effects: Effect[] = [];
  for (const e of events) ({ state, effects } = reduce(state, e));
  return { state, effects };
};
const key = (k: "ArrowUp" | "ArrowDown" | "Enter" | "Escape" | "F1", ctrl = false, shift = false): Event =>
  ({ type: "key", key: k, ctrl, shift });
const withItems = (...items: Item[]) => run(initial, { type: "results", items }).state;

describe("bar", () => {
  it("shown resets but keeps config error", () => {
    const dirty = run(initial, { type: "input", text: "x" }, { type: "configError", message: "bad toml" }).state;
    const { state } = run(dirty, { type: "shown" });
    expect(state.text).toBe("");
    expect(state.items).toEqual([]);
    expect(state.error).toBe("bad toml");
  });

  it("input emits scoped query", () => {
    expect(run(initial, { type: "input", text: "bl" }).effects).toEqual([{ type: "query", text: "bl", scope: null }]);
    const scoped = run(initial, { type: "outcome", outcome: { kind: "step", prompt: "Search", scope: "web" } }).state;
    const r = run(scoped, { type: "input", text: "cats" });
    expect(r.state.text).toBe("cats");
    expect(r.effects).toEqual([{ type: "query", text: "cats", scope: "web" }]);
  });

  it("results reset selection", () => {
    const s = run(withItems(item("a"), item("b")), key("ArrowDown")).state;
    expect(s.selected).toBe(1);
    expect(run(s, { type: "results", items: [item("c")] }).state.selected).toBe(0);
    expect(run(s, { type: "results", items: [] }).state.selected).toBe(0);
  });

  it("arrows wrap", () => {
    const s = withItems(item("a"), item("b"), item("c"));
    expect(run(s, key("ArrowUp")).state.selected).toBe(2);
    expect(run(s, key("ArrowDown"), key("ArrowDown"), key("ArrowDown")).state.selected).toBe(0);
  });

  it("enter activates with modifier", () => {
    const s = withItems(item("a"));
    const mod = (ctrl: boolean, shift: boolean) => run(s, key("Enter", ctrl, shift)).effects;
    expect(mod(false, false)).toEqual([{ type: "activate", item: item("a"), modifier: "none" }]);
    expect(mod(true, false)).toEqual([{ type: "activate", item: item("a"), modifier: "ctrl" }]);
    expect(mod(true, true)).toEqual([{ type: "activate", item: item("a"), modifier: "ctrl_shift" }]);
  });

  it("confirm items need a second enter", () => {
    const s = withItems(item("a", true));
    const first = run(s, key("Enter"));
    expect(first.effects).toEqual([]);
    expect(first.state.armed).toBe("a");
    expect(run(first.state, key("Enter")).effects).toEqual([
      { type: "activate", item: item("a", true), modifier: "none" },
    ]);
  });

  it("moving selection disarms", () => {
    const armed = run(withItems(item("a", true), item("b")), key("Enter")).state;
    expect(armed.armed).toBe("a");
    expect(run(armed, key("ArrowDown")).state.armed).toBeNull();
  });

  it("enter with no items does nothing", () => {
    const r = run(initial, key("Enter"));
    expect(r.effects).toEqual([]);
    expect(r.state).toEqual(initial);
  });

  it("step outcome enters scope", () => {
    const s = run(initial, { type: "input", text: "g" }).state;
    const r = run(s, { type: "outcome", outcome: { kind: "step", prompt: "Search Google", scope: "google" } });
    expect(r.state.scope).toEqual({ name: "google", prompt: "Search Google" });
    expect(r.state.text).toBe("");
    expect(r.effects).toEqual([{ type: "query", text: "", scope: "google" }]);
  });

  it("escape unwinds armed then scope then hides", () => {
    let s = run(initial, { type: "outcome", outcome: { kind: "step", prompt: "P", scope: "sc" } }).state;
    s = run(s, { type: "results", items: [item("a", true)] }).state;
    s = run(s, key("Enter")).state;
    expect(s.armed).toBe("a");
    const e1 = run(s, key("Escape"));
    expect(e1.state.armed).toBeNull();
    expect(e1.state.scope).not.toBeNull();
    expect(e1.effects).toEqual([]);
    const e2 = run(e1.state, key("Escape"));
    expect(e2.state.scope).toBeNull();
    expect(e2.effects).toEqual([{ type: "query", text: "", scope: null }]);
    expect(run(e2.state, key("Escape")).effects).toEqual([{ type: "hide" }]);
  });

  it("failed keeps bar open and shows message", () => {
    const s = run(withItems(item("a", true)), key("Enter"), { type: "configError", message: "cfg" }).state;
    const r = run(s, { type: "failed", message: "boom" });
    expect(r.state.error).toBe("boom");
    expect(r.state.armed).toBeNull();
    expect(r.effects).toEqual([]);
    expect(run(r.state, { type: "input", text: "a" }).state.error).toBe("cfg");
  });

  it("armed clears after confirm", () => {
    const armed = run(withItems(item("a", true)), key("Enter")).state;
    expect(run(armed, key("Enter")).state.armed).toBeNull();
  });

  it("enter ignored while activation pending", () => {
    const s = withItems(item("a"));
    const first = run(s, key("Enter"));
    expect(first.effects).toHaveLength(1);
    expect(run(first.state, key("Enter")).effects).toEqual([]);
    for (const clear of [
      { type: "outcome", outcome: { kind: "keep" } },
      { type: "failed", message: "x" },
      { type: "shown" },
    ] as Event[]) {
      const after = run(first.state, clear).state;
      const again = run(after, { type: "results", items: [item("a")] }, key("Enter"));
      expect(again.effects).toHaveLength(1);
    }
  });

  it("outcome keep and hide produce no effects", () => {
    const s = withItems(item("a"));
    for (const kind of ["keep", "hide"] as const) {
      const r = run(s, { type: "outcome", outcome: { kind } });
      expect(r.effects).toEqual([]);
      expect(r.state.items).toEqual(s.items);
    }
  });

  it("config error does not replace a held failure", () => {
    let s = run(initial, { type: "failed", message: "boom" }, { type: "configError", message: "cfg" }).state;
    expect(s.error).toBe("boom");
    expect(run(s, { type: "input", text: "a" }).state.error).toBe("cfg");
    expect(run(s, { type: "shown" }).state.error).toBe("cfg");
  });

  it("arrows with zero items do nothing", () => {
    for (const k of ["ArrowUp", "ArrowDown"] as const) {
      const r = run(initial, key(k));
      expect(r.state).toEqual(initial);
      expect(r.effects).toEqual([]);
    }
  });

  it("visible rows are capped at 8, or 7 while an error shows", () => {
    const many = Array.from({ length: 12 }, (_, i) => item(String(i)));
    const s = withItems(...many);
    expect(visibleItems(s)).toHaveLength(8);
    expect(visibleItems({ ...s, error: "e" })).toHaveLength(7);
  });

  it("selection stays inside the window when the error line appears", () => {
    const many = Array.from({ length: 12 }, (_, i) => item(String(i)));
    const eighth = run(withItems(...many), ...Array(7).fill(key("ArrowDown"))).state;
    expect(eighth.selected).toBe(7);
    expect(eighth.top).toBe(0);
    for (const ev of [{ type: "configError", message: "e" }, { type: "failed", message: "e" }] as Event[]) {
      const s = run(eighth, ev).state;
      expect(s.selected).toBe(7);
      expect(s.top).toBe(1);
    }
  });

  it("window follows selection downward", () => {
    const many = Array.from({ length: 12 }, (_, i) => item(String(i)));
    let s = withItems(...many);
    s = run(s, ...Array(7).fill(key("ArrowDown"))).state;
    expect([s.selected, s.top]).toEqual([7, 0]);
    s = run(s, key("ArrowDown")).state;
    expect([s.selected, s.top]).toEqual([8, 1]);
    expect(visibleItems(s)[0].id).toBe("1");
    expect(visibleItems(s)).toHaveLength(8);
  });

  it("window follows selection upward", () => {
    const many = Array.from({ length: 12 }, (_, i) => item(String(i)));
    let s = run(withItems(...many), key("ArrowUp")).state; // wraps to 11
    expect([s.selected, s.top]).toEqual([11, 4]);
    s = run(s, ...Array(7).fill(key("ArrowUp"))).state;
    expect([s.selected, s.top]).toEqual([4, 4]);
    s = run(s, key("ArrowUp")).state;
    expect([s.selected, s.top]).toEqual([3, 3]);
  });

  it("arrows wrap across the full list and move the window", () => {
    const many = Array.from({ length: 12 }, (_, i) => item(String(i)));
    const s = withItems(...many);
    const up = run(s, key("ArrowUp")).state;
    expect([up.selected, up.top]).toEqual([11, 4]);
    const down = run(up, key("ArrowDown")).state;
    expect([down.selected, down.top]).toEqual([0, 0]);
    const short = withItems(item("a"), item("b"));
    expect(run(short, key("ArrowUp")).state.top).toBe(0);
  });

  it("results reset the window", () => {
    const many = Array.from({ length: 12 }, (_, i) => item(String(i)));
    const scrolled = run(withItems(...many), key("ArrowUp")).state;
    expect(scrolled.top).toBe(4);
    const again = run(scrolled, { type: "results", items: many }).state;
    expect([again.selected, again.top]).toEqual([0, 0]);
    expect(run(scrolled, { type: "input", text: "x" }).state.top).toBe(0);
    expect(run(scrolled, { type: "shown" }).state.top).toBe(0);
  });

  it("error line shrinks the window to 7", () => {
    const many = Array.from({ length: 12 }, (_, i) => item(String(i)));
    let s = run(withItems(...many), { type: "configError", message: "cfg" }).state;
    expect(visibleItems(s)).toHaveLength(7);
    s = run(s, key("ArrowUp")).state;
    expect([s.selected, s.top]).toEqual([11, 5]);
    expect(visibleItems(s)).toHaveLength(7);
    s = run(s, key("ArrowDown")).state;
    expect([s.selected, s.top]).toEqual([0, 0]);
  });

  it("fill outcome sets text and queries", () => {
    let s = run(withItems(item("a", true)), key("Enter")).state; // armed
    s = run(s, key("Enter")).state; // pending
    s = run(s, { type: "outcome", outcome: { kind: "step", prompt: "P", scope: "sc" } }).state;
    const r = run(s, { type: "outcome", outcome: { kind: "fill", text: "wsl ls" } });
    expect(r.state.text).toBe("wsl ls");
    expect(r.state.scope).toEqual({ name: "sc", prompt: "P" });
    expect(r.state.armed).toBeNull();
    expect(r.state.pending).toBe(false);
    expect(r.state.queued).toBeNull();
    expect(r.state.selected).toBe(0);
    expect(r.effects).toEqual([{ type: "query", text: "wsl ls", scope: "sc" }]);
  });

  it("empty fill clears and queries empty text", () => {
    const s = run(withItems(item("a")), { type: "input", text: "abc" }).state;
    const r = run(s, { type: "outcome", outcome: { kind: "fill", text: "" } });
    expect(r.state.text).toBe("");
    expect(r.state.items).toEqual([]);
    expect(r.state.awaiting).toBe(true);
    expect(r.effects).toEqual([{ type: "query", text: "", scope: null }]);
    const scoped = run(initial, { type: "outcome", outcome: { kind: "step", prompt: "P", scope: "sc" } }).state;
    expect(run(scoped, { type: "outcome", outcome: { kind: "fill", text: "" } }).effects).toEqual([
      { type: "query", text: "", scope: "sc" },
    ]);
  });

  it("enter after an empty fill is queued, not run on old items", () => {
    const s = run(withItems(item("old")), { type: "outcome", outcome: { kind: "fill", text: "" } }, key("Enter"));
    expect(s.effects).toEqual([]);
    expect(s.state.queued).toBe("none");
  });

  it("F1 during a pending activation keeps Enter blocked", () => {
    const pending = run(withItems(item("a")), key("Enter")).state;
    expect(pending.pending).toBe(true);
    const f = run(pending, key("F1")).state;
    expect(f.pending).toBe(true);
    expect(run(f, { type: "results", items: [item("a")] }, key("Enter")).effects).toEqual([]);
  });

  it("F1 fills help", () => {
    const s = run(withItems(item("a", true)), key("Enter"), { type: "input", text: "zz" }, key("Enter")).state;
    const r = run(s, key("F1"));
    expect(r.state.text).toBe("help");
    expect(r.state.armed).toBeNull();
    expect(r.state.queued).toBeNull();
    expect(r.effects).toEqual([{ type: "query", text: "help", scope: null }]);
  });

  it("F1 inside a pick list leaves the scope and shows the whole help list", () => {
    const scoped = run(initial, { type: "outcome", outcome: { kind: "step", prompt: "proj", scope: "shortcut:proj" } }).state;
    const r = run(scoped, key("F1"));
    expect(r.state.scope).toBeNull();
    expect(r.state.text).toBe("help");
    expect(r.effects).toEqual([{ type: "query", text: "help", scope: null }]);
  });

  it("enter before a pick list arrives is not queued", () => {
    const step = run(initial, { type: "outcome", outcome: { kind: "step", prompt: "proj", scope: "shortcut:proj" } });
    expect(step.state.awaiting).toBe(true);
    const early = run(step.state, key("Enter"));
    expect(early.state.queued).toBeNull();
    expect(early.effects).toEqual([]);
    // The list arrives: nothing the user never saw is run.
    const r = run(early.state, { type: "results", items: [item("Alpha"), item("beta")] });
    expect(r.effects).toEqual([]);
    expect(r.state.items).toHaveLength(2);
  });

  it("enter after typing inside a pick list is still queued", () => {
    const scoped = run(initial, { type: "outcome", outcome: { kind: "step", prompt: "proj", scope: "shortcut:proj" } }).state;
    const typed = run(scoped, { type: "input", text: "be" }, key("Enter"));
    expect(typed.state.queued).toBe("none");
    const r = run(typed.state, { type: "results", items: [item("beta")] });
    expect(r.effects).toEqual([{ type: "activate", item: item("beta"), modifier: "none" }]);
  });

  it("blur disarms and drops a queued enter", () => {
    const armed = run(withItems(item("a", true)), key("Enter")).state;
    const b = run(armed, { type: "blurred" }).state;
    expect(b.armed).toBeNull();
    expect(b.items).toEqual(armed.items);
    const queued = run(initial, { type: "input", text: "c" }, key("Enter")).state;
    const q = run(queued, { type: "blurred" }).state;
    expect(q.queued).toBeNull();
    expect(q.text).toBe("c");
    expect(run(q, { type: "results", items: [item("calc")] }).effects).toEqual([]);
  });

  it("enter activates the selected row even when scrolled", () => {
    const many = Array.from({ length: 12 }, (_, i) => item(String(i)));
    const s = run(withItems(...many), ...Array(9).fill(key("ArrowDown"))).state;
    expect(s.selected).toBe(9);
    expect(run(s, key("Enter")).effects).toEqual([{ type: "activate", item: item("9"), modifier: "none" }]);
  });

  it("enter before results arrive activates the new top item", () => {
    const typed = run(withItems(item("c-old")), { type: "input", text: "chrome" });
    expect(typed.state.awaiting).toBe(true);
    const early = run(typed.state, key("Enter", true));
    expect(early.effects).toEqual([]);
    expect(early.state.queued).toBe("ctrl");
    const r = run(early.state, { type: "results", items: [item("chrome"), item("other")] });
    expect(r.effects).toEqual([{ type: "activate", item: item("chrome"), modifier: "ctrl" }]);
    expect(r.state.queued).toBeNull();
    expect(r.state.awaiting).toBe(false);
    expect(r.state.pending).toBe(true);
  });

  it("enter right after show waits for the first results", () => {
    const s = run(initial, { type: "shown" }, { type: "input", text: "c" }, key("Enter")).state;
    const r = run(s, { type: "results", items: [item("calc")] });
    expect(r.effects).toEqual([{ type: "activate", item: item("calc"), modifier: "none" }]);
  });

  it("queued enter on a confirm item only arms it", () => {
    const s = run(initial, { type: "input", text: "restart" }, key("Enter")).state;
    const r = run(s, { type: "results", items: [item("restart", true)] });
    expect(r.effects).toEqual([]);
    expect(r.state.armed).toBe("restart");
    expect(r.state.pending).toBe(false);
  });

  it("results without a queued enter activate nothing", () => {
    const s = run(initial, { type: "input", text: "c" }).state;
    const r = run(s, { type: "results", items: [item("calc")] });
    expect(r.effects).toEqual([]);
    expect(r.state.awaiting).toBe(false);
  });

  it("shown, escape and new input drop a queued enter", () => {
    const queued = run(initial, { type: "input", text: "c" }, key("Enter")).state;
    expect(queued.queued).toBe("none");
    for (const clear of [{ type: "shown" }, key("Escape"), { type: "input", text: "ch" }] as Event[]) {
      const s = run(queued, clear).state;
      expect(s.queued).toBeNull();
      expect(run(s, { type: "results", items: [item("calc")] }).effects).toEqual([]);
    }
  });

  it("step and scope escape await their query", () => {
    const step = run(initial, { type: "outcome", outcome: { kind: "step", prompt: "P", scope: "sc" } }).state;
    expect(step.awaiting).toBe(true);
    const back = run({ ...step, awaiting: false }, key("Escape")).state;
    expect(back.awaiting).toBe(true);
  });

  it("a failed query clears awaiting but not a pending activation", () => {
    const pending = run(withItems(item("a")), key("Enter")).state;
    expect(pending.pending).toBe(true);
    const typed = run(pending, { type: "input", text: "b" }, key("Enter")).state;
    expect(typed.awaiting).toBe(true);
    const r = run(typed, { type: "queryFailed", message: "boom" });
    expect(r.state.awaiting).toBe(false);
    expect(r.state.queued).toBeNull();
    expect(r.state.pending).toBe(true);
    expect(r.state.error).toBe("boom");
    expect(r.effects).toEqual([]);
  });

  it("activation errors name the item", () => {
    expect(failureText(item("Blender"), "file not found")).toBe("Blender: file not found");
    expect(failureText(item("Blender"), new Error("boom"))).toBe("Blender: Error: boom");
  });
});
