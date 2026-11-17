import { describe, it, expect } from "vitest";
import { failureText, initial, menuActions, movedEnough, reduce, rowsFromWheel, visibleItems, type Effect, type Event, type Item, type State } from "./bar";

const item = (id: string, confirm = false): Item => ({
  id, provider: "apps", title: id, subtitle: "", confirm,
  icon: "builtin:app", origin: "search", pinned: false, pinnable: false, actions: [], complete: "",
});
const run = (s: State, ...events: Event[]) => {
  let state = s;
  let effects: Effect[] = [];
  for (const e of events) ({ state, effects } = reduce(state, e));
  return { state, effects };
};
const key = (k: "ArrowUp" | "ArrowDown" | "Enter" | "Escape" | "F1" | "p" | "Tab", ctrl = false, shift = false): Event =>
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

  it("queries the empty text when shown", () => {
    const { effects } = reduce(initial, { type: "shown" });
    expect(effects).toEqual([{ type: "query", text: "", scope: null }, { type: "loadPast" }]);
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
    expect(mod(false, false)).toEqual([{ type: "activate", item: item("a"), modifier: "none", text: "", scope: null }]);
    expect(mod(true, false)).toEqual([{ type: "activate", item: item("a"), modifier: "ctrl", text: "", scope: null }]);
    expect(mod(true, true)).toEqual([{ type: "activate", item: item("a"), modifier: "ctrl_shift", text: "", scope: null }]);
  });

  it("confirm items need a second enter", () => {
    const s = withItems(item("a", true));
    const first = run(s, key("Enter"));
    expect(first.effects).toEqual([]);
    expect(first.state.armed).toBe("a");
    expect(run(first.state, key("Enter")).effects).toEqual([
      { type: "activate", item: item("a", true), modifier: "none", text: "", scope: null },
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
    expect(r.effects).toEqual([{ type: "activate", item: item("beta"), modifier: "none", text: "be", scope: "shortcut:proj" }]);
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
    expect(run(s, key("Enter")).effects).toEqual([{ type: "activate", item: item("9"), modifier: "none", text: "", scope: null }]);
  });

  it("enter before results arrive activates the new top item", () => {
    const typed = run(withItems(item("c-old")), { type: "input", text: "chrome" });
    expect(typed.state.awaiting).toBe(true);
    const early = run(typed.state, key("Enter", true));
    expect(early.effects).toEqual([]);
    expect(early.state.queued).toBe("ctrl");
    const r = run(early.state, { type: "results", items: [item("chrome"), item("other")] });
    expect(r.effects).toEqual([{ type: "activate", item: item("chrome"), modifier: "ctrl", text: "chrome", scope: null }]);
    expect(r.state.queued).toBeNull();
    expect(r.state.awaiting).toBe(false);
    expect(r.state.pending).toBe(true);
  });

  it("enter right after show waits for the first results", () => {
    const s = run(initial, { type: "shown" }, { type: "input", text: "c" }, key("Enter")).state;
    const r = run(s, { type: "results", items: [item("calc")] });
    expect(r.effects).toEqual([{ type: "activate", item: item("calc"), modifier: "none", text: "c", scope: null }]);
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
  describe("icons, pins and recents", () => {
    const row = (id: string, o: Partial<Item> = {}): Item => ({ ...item(id), ...o });
    const shownWith = (...items: Item[]) =>
      run(initial, { type: "shown" }, { type: "results", items }).state;
    const withPast = (...past: string[]) => run(initial, { type: "past", items: past }).state;
    const queryOf = (text: string): Effect => ({ type: "query", text, scope: null });

    it("shown also loads past searches", () => {
      expect(reduce(initial, { type: "shown" }).effects).toContainEqual({ type: "loadPast" });
    });

    it("shown forgets past searches until they are loaded again", () => {
      const reopened = run(withPast("old"), { type: "shown" }).state;
      expect(reopened.past).toEqual([]);
      // Up before the list arrives recalls nothing.
      const up = run(reopened, key("ArrowUp"));
      expect(up.state.text).toBe("");
      expect(up.effects).toEqual([]);
      expect(run(reopened, { type: "past", items: ["new"] }, key("ArrowUp")).state.text).toBe("new");
    });

    it("empty bar selects the latest recent launch", () => {
      const s = shownWith(row("pin", { origin: "pin" }), row("r1", { origin: "recent" }), row("r2", { origin: "recent" }));
      expect(s.selected).toBe(1);
    });

    it("welcome rows keep the selection on top", () => {
      const s = shownWith(row("w", { provider: "welcome" }), row("r", { origin: "recent" }));
      expect(s.selected).toBe(0);
    });

    it("typed text does not use the recent rule", () => {
      const s = run(initial, { type: "input", text: "x" }, { type: "results", items: [row("a"), row("r", { origin: "recent" })] }).state;
      expect(s.selected).toBe(0);
    });

    it("Up on an empty bar recalls past searches", () => {
      const s = withPast("b", "a");
      const up1 = run(s, key("ArrowUp"));
      expect(up1.state.text).toBe("b");
      expect(up1.effects).toEqual([queryOf("b")]);
      const up2 = run(up1.state, key("ArrowUp"));
      expect(up2.state.text).toBe("a");
      expect(up2.effects).toEqual([queryOf("a")]);
      const up3 = run(up2.state, key("ArrowUp"));
      expect(up3.state.text).toBe("a");
      const down1 = run(up3.state, key("ArrowDown"));
      expect(down1.state.text).toBe("b");
      expect(down1.effects).toEqual([queryOf("b")]);
      const down2 = run(down1.state, key("ArrowDown"));
      expect(down2.state.text).toBe("");
      expect(down2.state.recall).toBe(-1);
      expect(down2.effects).toEqual([queryOf("")]);
    });

    it("Up recalls even when the empty bar has rows", () => {
      const s = run(withPast("b"), { type: "results", items: [row("a"), row("c")] }, key("ArrowDown")).state;
      expect(run(s, key("ArrowUp")).state.text).toBe("b");
    });

    it("Up does nothing without past searches", () => {
      const s = run(initial, { type: "results", items: [row("a"), row("b")] }).state;
      const r = run(s, key("ArrowUp"));
      expect(r.state.text).toBe("");
      expect(r.state.selected).toBe(1);
      expect(r.effects).toEqual([]);
    });

    it("Down on an empty bar not recalling moves rows", () => {
      const s = run(withPast("b"), { type: "results", items: [row("a"), row("c")] }).state;
      const r = run(s, key("ArrowDown"));
      expect(r.state.selected).toBe(1);
      expect(r.state.text).toBe("");
      expect(r.effects).toEqual([]);
    });

    it("Up with typed text moves the selection as before", () => {
      const s = run(withPast("b"), { type: "input", text: "x" }, { type: "results", items: [row("a"), row("c")] }).state;
      const r = run(s, key("ArrowUp"));
      expect(r.state.text).toBe("x");
      expect(r.state.selected).toBe(1);
    });

    it("typing ends recall", () => {
      const recalled = run(withPast("b", "a"), key("ArrowUp")).state;
      expect(recalled.recall).toBe(0);
      const typed = run(recalled, { type: "input", text: "bx" }, { type: "results", items: [row("a"), row("c")] }).state;
      expect(typed.recall).toBe(-1);
      expect(run(typed, key("ArrowUp")).state.selected).toBe(1);
    });

    it("Ctrl+P pins only pinnable rows", () => {
      const p = row("p", { pinnable: true });
      const s = withItems(p, row("n"));
      expect(run(s, key("p", true)).effects).toEqual([{ type: "togglePin", item: p }]);
      expect(run(s, key("p", false)).effects).toEqual([]);
      const second = run(s, key("ArrowDown"), key("p", true));
      expect(second.effects).toEqual([]);
      const pinned = run(s, { type: "pinned", provider: "apps", id: "p", pinned: true }).state;
      expect(pinned.items[0].pinned).toBe(true);
      expect(pinned.items[1].pinned).toBe(false);
    });

    it("pinning on the empty bar lays it out again", () => {
      const p = row("p", { pinnable: true, origin: "recent" });
      const s = shownWith(p);
      expect(run(s, { type: "pinned", provider: "apps", id: "p", pinned: true }).effects).toEqual([queryOf("")]);
      const typed = run(s, { type: "input", text: "p" }, { type: "results", items: [p] }).state;
      expect(run(typed, { type: "pinned", provider: "apps", id: "p", pinned: true }).effects).toEqual([]);
      const scoped = run(initial, { type: "outcome", outcome: { kind: "step", prompt: "Search", scope: "web" } }).state;
      expect(run(scoped, { type: "pinned", provider: "apps", id: "p", pinned: true }).effects).toEqual([]);
    });

    it("leaving a scope after recalling resets recall", () => {
      const recalled = run(withPast("b", "a"), key("ArrowUp"), key("ArrowUp")).state;
      const out = run(
        recalled,
        { type: "outcome", outcome: { kind: "step", prompt: "Search", scope: "web" } },
        key("Escape"),
      ).state;
      expect(out.recall).toBe(-1);
      expect(run(out, key("ArrowUp")).state.text).toBe("b");
    });

    it("activate effect carries text and scope", () => {
      const scoped = run(initial, { type: "outcome", outcome: { kind: "step", prompt: "Search", scope: "web" } }).state;
      const s = run(scoped, { type: "input", text: "cats" }, { type: "results", items: [item("a")] }).state;
      expect(run(s, key("Enter")).effects).toEqual([
        { type: "activate", item: item("a"), modifier: "none", text: "cats", scope: "web" },
      ]);
    });
  });
});

describe("mouse and menu", () => {
  const rich = (id: string, o: Partial<Item> = {}): Item => ({ ...item(id), ...o });
  const click = (index: number, ctrl = false, shift = false): Event => ({ type: "click", index, ctrl, shift });
  const fileItem = (id: string, o: Partial<Item> = {}): Item => rich(id, { actions: ["reveal", "copy_path"], pinnable: true, ...o });

  it("hover selects a row", () => {
    const s = withItems(item("a"), item("b"), item("c"));
    expect(run(s, { type: "hover", index: 2 }).state.selected).toBe(2);
  });

  it("hover is ignored while the menu is open", () => {
    const s = withItems(rich("a", { pinnable: true }), item("b"));
    const open = run(s, { type: "menu", index: 0 }).state;
    expect(run(open, { type: "hover", index: 1 }).state.selected).toBe(0);
  });

  it("results arriving under a still pointer keep the keyboard selection", () => {
    const s = run(withItems(item("a"), item("b"), item("c")), key("ArrowDown"), key("ArrowDown")).state;
    expect(s.selected).toBe(2);
    expect(run(s, { type: "results", items: [item("x"), item("y"), item("z")] }).state.selected).toBe(0);
  });

  it("movedEnough only reports real movement", () => {
    expect(movedEnough(null, { x: 1, y: 2 })).toBe(true);
    expect(movedEnough({ x: 1, y: 2 }, { x: 1, y: 2 })).toBe(false);
    expect(movedEnough({ x: 1, y: 2 }, { x: 1, y: 3 })).toBe(true);
  });

  it("click activates and a power row needs a second click", () => {
    const s = withItems(item("a"), item("p", true));
    expect(run(s, click(0)).effects).toEqual([{ type: "activate", item: item("a"), modifier: "none", text: "", scope: null }]);
    const first = run(s, click(1));
    expect(first.effects).toEqual([]);
    expect(first.state.armed).toBe("p");
    const second = run(first.state, click(1));
    expect(second.effects).toEqual([{ type: "activate", item: item("p", true), modifier: "none", text: "", scope: null }]);
  });

  it("click opens the clicked row even while a query is in flight", () => {
    const s = run(withItems(item("a"), item("b")), { type: "input", text: "chr" }).state;
    expect(s.awaiting).toBe(true);
    const r = run(s, click(1));
    expect(r.effects).toEqual([{ type: "activate", item: item("b"), modifier: "none", text: "chr", scope: null }]);
    expect(r.state.queued).toBeNull();
    // The new results do not open anything else.
    expect(run(r.state, { type: "results", items: [item("c")] }).effects).toEqual([]);
  });

  it("ctrl+shift click runs as admin", () => {
    const s = withItems(item("a"));
    expect(run(s, click(0, true, true)).effects).toEqual([
      { type: "activate", item: item("a"), modifier: "ctrl_shift", text: "", scope: null },
    ]);
  });

  it("menu lists only the actions a row supports", () => {
    const menu = (i: Item) => run(withItems(i), { type: "menu", index: 0 }).state.menu;
    expect(menu(fileItem("f"))).toEqual({ index: 0, actions: ["reveal", "copy_path", "pin"], selected: 0, at: "pointer" });
    expect(menu(rich("a", { pinnable: true, pinned: true, actions: ["admin", "reveal"] }))?.actions).toEqual(["admin", "reveal", "unpin"]);
    expect(menu(rich("p", { pinnable: true, confirm: true }))?.actions).toEqual(["pin"]);
    expect(menu(rich("w"))).toBeNull();
  });

  it("menu keys stay inside the menu", () => {
    const s = withItems(rich("a", { pinnable: true, actions: ["reveal", "admin"] }));
    const open = run(s, { type: "menu", index: 0 }).state;
    expect(run(open, key("ArrowDown")).state.menu?.selected).toBe(1);
    expect(run(open, key("ArrowUp")).state.menu?.selected).toBe(2);
    const esc = run(open, key("Escape"));
    expect(esc.state.menu).toBeNull();
    expect(esc.effects).toEqual([]);
  });

  it("menu closes on input, blur and shown", () => {
    const open = run(withItems(rich("a", { pinnable: true })), { type: "menu", index: 0 }).state;
    expect(run(open, { type: "input", text: "x" }).state.menu).toBeNull();
    expect(run(open, { type: "blurred" }).state.menu).toBeNull();
    expect(run(open, { type: "shown" }).state.menu).toBeNull();
    expect(run(open, { type: "menuClose" }).state.menu).toBeNull();
  });

  it("menu picks emit the right effects", () => {
    const a = rich("a", { pinnable: true, actions: ["reveal", "admin"] });
    const open = run(withItems(a), { type: "menu", index: 0 }).state;
    expect(run(open, { type: "menuPick", action: "pin" }).effects).toEqual([{ type: "togglePin", item: a }]);
    expect(run(open, { type: "menuPick", action: "admin" }).effects).toEqual([
      { type: "activate", item: a, modifier: "ctrl_shift", text: "", scope: null },
    ]);
    const viaEnter = run(open, key("Enter"));
    expect(viaEnter.effects).toEqual([{ type: "act", item: a, action: "reveal" }]);
    expect(viaEnter.state.menu).toBeNull();
  });

  it("menu_picks_emit_act_effects", () => {
    for (const action of ["copy_path", "reveal", "open_folder", "close", "close_window"] as const) {
      const a = rich("a", { actions: [action] });
      const open = run(withItems(a), { type: "menu", index: 0 }).state;
      const r = run(open, { type: "menuPick", action });
      expect(r.effects).toEqual([{ type: "act", item: a, action }]);
      expect(r.state.pending).toBe(true);
      expect(r.state.menu).toBeNull();
    }
  });

  it("menu_lists_row_actions_then_pin", () => {
    const a = rich("a", { actions: ["admin", "reveal", "uninstall", "close"], pinnable: true, pinned: true });
    expect(menuActions(a)).toEqual(["admin", "reveal", "uninstall", "close", "unpin"]);
  });

  describe("tab", () => {
    it("tab_opens_the_menu_of_the_selected_row_under_it", () => {
      const r = run(withItems(fileItem("f")), key("Tab"));
      expect(r.state.menu).toEqual({ index: 0, actions: ["reveal", "copy_path", "pin"], selected: 0, at: "row" });
      expect(r.effects).toEqual([]);
    });

    it("tab_does_nothing_without_a_row_or_actions", () => {
      expect(run(initial, key("Tab"))).toEqual({ state: initial, effects: [] });
      expect(run(withItems(item("a")), key("Tab")).state.menu).toBeNull();
    });

    it("tab_and_escape_close_an_open_menu", () => {
      const open = run(withItems(fileItem("f")), key("Tab")).state;
      expect(run(open, key("Tab")).state.menu).toBeNull();
      const esc = run(open, key("Escape"));
      expect(esc.state.menu).toBeNull();
      expect(esc.effects).toEqual([]);
    });

    it("tab_completes_in_path_mode", () => {
      const p = rich("C:\\Games", { provider: "path", complete: "C:\\Games\\", actions: ["open_folder"] });
      const r = run(withItems(p), key("Tab"));
      expect(r.state.text).toBe("C:\\Games\\");
      expect(r.state.menu).toBeNull();
      expect(r.effects).toEqual([{ type: "query", text: "C:\\Games\\", scope: null }]);
    });

    it("tab_on_an_inert_path_row_does_nothing", () => {
      const p = rich("C:\\x", { provider: "path", complete: "" });
      const s = withItems(p, fileItem("f"));
      const r = run(s, key("Tab"));
      expect(r.state.menu).toBeNull();
      expect(r.effects).toEqual([]);
      // Any path row makes the list path mode, even when another row is selected.
      expect(run(s, key("ArrowDown"), key("Tab")).state.menu).toBeNull();
    });

    it("tab_while_awaiting_does_nothing", () => {
      const s = run(withItems(fileItem("f")), { type: "input", text: "x" }).state;
      const r = run(s, key("Tab"));
      expect(r.effects).toEqual([]);
      expect(r.state.menu).toBeNull();
    });
  });

  describe("uninstall", () => {
    const u = rich("u", { actions: ["uninstall"] });
    const armedState = () => {
      const typed = run(initial, { type: "shown" }, { type: "input", text: "x" }, { type: "results", items: [u] }).state;
      return run(typed, key("Tab"), { type: "menuPick", action: "uninstall" });
    };

    it("uninstall_arms_then_a_second_enter_acts", () => {
      const first = armedState();
      expect(first.effects).toEqual([]);
      expect(first.state.armed).toBe("u");
      expect(first.state.armedAction).toBe("uninstall");
      expect(first.state.menu).toBeNull();
      const second = run(first.state, key("Enter"));
      expect(second.effects).toEqual([{ type: "act", item: u, action: "uninstall" }]);
      expect(second.state.pending).toBe(true);
      expect(second.state.armed).toBeNull();
      expect(second.state.armedAction).toBeNull();
    });

    it("moving_or_typing_disarms_an_uninstall", () => {
      const s = run(withItems(u, item("b")), key("Tab"), { type: "menuPick", action: "uninstall" }).state;
      expect(s.armedAction).toBe("uninstall");
      for (const e of [key("ArrowDown"), { type: "input", text: "x" } as Event]) {
        const r = run(s, e).state;
        expect([r.armed, r.armedAction]).toEqual([null, null]);
      }
      const moved = run(s, key("ArrowDown"), key("ArrowUp"));
      expect(run(moved.state, key("Enter")).effects).toEqual([
        { type: "activate", item: u, modifier: "none", text: "", scope: null },
      ]);
    });

    it("armed_uninstall_survives_only_on_the_same_row", () => {
      const s = run(armedState().state, { type: "answersReady" }).state;
      const kept = run(s, { type: "results", items: [item("x"), u] }).state;
      expect([kept.armed, kept.armedAction]).toEqual(["u", "uninstall"]);
      const lost = run(s, { type: "results", items: [item("x")] }).state;
      expect([lost.armed, lost.armedAction]).toEqual([null, null]);
    });
  });

  describe("wheel", () => {
    const many = () => withItems(...Array.from({ length: 20 }, (_, i) => item(`r${i}`)));

    it("wheel_scrolls_without_changing_the_selection", () => {
      const s = many();
      const a = run(s, { type: "wheel", rows: 3 }).state;
      expect([a.top, a.selected]).toEqual([3, 0]);
      expect(run(s, { type: "wheel", rows: 100 }).state.top).toBe(12);
      expect(run(a, { type: "wheel", rows: -100 }).state.top).toBe(0);
      const b = run(s, { type: "wheel", rows: 5 }, key("ArrowDown")).state;
      expect([b.selected, b.top]).toEqual([1, 1]);
    });

    it("ready_after_scrolling_keeps_selection_and_window", () => {
      const items = Array.from({ length: 20 }, (_, i) => item(`r${i}`));
      const typed = run(initial, { type: "shown" }, { type: "input", text: "x" }, { type: "results", items }).state;
      const down = run(typed, ...Array.from({ length: 12 }, () => key("ArrowDown"))).state;
      expect([down.selected, down.top]).toEqual([12, 5]);
      const ready = run(down, { type: "answersReady" }, { type: "results", items }).state;
      expect([ready.selected, ready.top]).toEqual([12, 5]);
    });

    it("rows_from_wheel_counts_40_px_per_row", () => {
      expect(rowsFromWheel(0, 100)).toEqual({ rows: 2, acc: 20 });
      expect(rowsFromWheel(20, 30)).toEqual({ rows: 1, acc: 10 });
      expect(rowsFromWheel(0, -50)).toEqual({ rows: -1, acc: -10 });
      expect(rowsFromWheel(0, 10)).toEqual({ rows: 0, acc: 10 });
    });
  });

  describe("update changed", () => {
    it("requeries the empty bar only", () => {
      const empty = run(initial, { type: "shown" }, { type: "results", items: [item("u")] });
      expect(run(empty.state, { type: "updateChanged" }).effects).toEqual([
        { type: "query", text: "", scope: null, keepSelection: true },
      ]);
      const typed = run(initial, { type: "shown" }, { type: "input", text: "a" }, { type: "results", items: [item("a")] });
      expect(run(typed.state, { type: "updateChanged" }).effects).toEqual([]);
      expect(run(empty.state, { type: "hidden" }, { type: "updateChanged" }).effects).toEqual([]);
    });
  });

  describe("answers ready", () => {
    const typed = (text: string, ...items: Item[]) =>
      run(initial, { type: "shown" }, { type: "input", text }, { type: "results", items }).state;

    it("ready_ignored_when_text_changed", () => {
      const s = { ...typed("20 usd", item("a")), text: "20 usd i" };
      const text = run(s, { type: "answersReady" });
      expect(text.effects).toEqual([]);
      // Text and scope unchanged since the last query: nothing to do while hidden, a re-query otherwise.
      const scoped = run(typed("20 usd", item("a")), { type: "outcome", outcome: { kind: "step", prompt: "P", scope: "web" } });
      expect(run(scoped.state, { type: "answersReady" }).effects).toEqual([
        { type: "query", text: "", scope: "web", keepSelection: true },
      ]);
    });

    it("ready_requeries_and_keeps_selection", () => {
      const s = run(typed("20 usd", item("w"), item("x"), item("y")), key("ArrowDown")).state;
      expect(s.selected).toBe(1);
      const r = run(s, { type: "answersReady" });
      expect(r.effects).toEqual([{ type: "query", text: "20 usd", scope: null, keepSelection: true }]);
      expect(r.state.awaiting).toBe(false);
      const got = run(r.state, { type: "results", items: [item("w"), item("v"), item("x")] }).state;
      expect(got.selected).toBe(2);
      // The row is gone: back to the first.
      const gone = run(r.state, { type: "results", items: [item("p"), item("q")] }).state;
      expect(gone.selected).toBe(0);
      // Only the answersReady query keeps the selection; later results behave as before.
      const later = run(got, { type: "results", items: [item("x"), item("w")] }).state;
      expect(later.selected).toBe(0);
    });

    it("ready_keeps_provider_and_id_together", () => {
      const other = { ...item("x"), provider: "files" };
      const s = run(typed("t", item("a"), item("x")), key("ArrowDown")).state;
      const r = run(s, { type: "answersReady" });
      expect(run(r.state, { type: "results", items: [other, item("x")] }).state.selected).toBe(1);
    });

    it("ready_ignored_when_hidden", () => {
      const s = typed("20 usd", item("a"));
      expect(run(s, { type: "hidden" }, { type: "answersReady" }).effects).toEqual([]);
      // A focus loss alone does not stop live updates: the back end says when the bar is hidden.
      expect(run(s, { type: "blurred" }, { type: "answersReady" }).effects).toHaveLength(1);
      const esc = run(s, key("Escape"));
      expect(esc.effects).toEqual([{ type: "hide" }]);
      expect(run(esc.state, { type: "answersReady" }).effects).toEqual([]);
      // Shown again: live once more.
      expect(run(esc.state, { type: "shown" }, { type: "input", text: "a" }, { type: "answersReady" }).effects).toEqual([
        { type: "query", text: "a", scope: null, keepSelection: true },
      ]);
    });

    it("ready_keeps_a_queued_enter", () => {
      const waiting = run(typed("20 usd", item("old")), { type: "input", text: "20 usd in eur" }, key("Enter")).state;
      expect(waiting.queued).toBe("none");
      const r = run(waiting, { type: "answersReady" }, { type: "results", items: [item("a"), item("b")] });
      expect(r.effects).toEqual([{ type: "activate", item: item("a"), modifier: "none", text: "20 usd in eur", scope: null }]);
      expect(r.state.queued).toBeNull();
    });

    it("ready_keeps_top_and_armed_by_provider_and_id", () => {
      const many = Array.from({ length: 12 }, (_, i) => item(`r${i}`));
      let s = typed("t", ...many);
      for (let i = 0; i < 9; i++) s = run(s, key("ArrowDown")).state;
      expect([s.selected, s.top]).toEqual([9, 2]);
      s = { ...s, armed: "r9" };
      const r = run(s, { type: "answersReady" });
      // One row was added on top: the selected row moves down, the window does not jump to the top.
      const got = run(r.state, { type: "results", items: [item("new"), ...many] }).state;
      expect([got.selected, got.top, got.armed]).toEqual([10, 3, "r9"]);
      // The same id under another provider is another row: back to the first, nothing armed.
      const other = many.map((i) => (i.id === "r9" ? { ...i, provider: "files" } : i));
      const lost = run(r.state, { type: "results", items: other }).state;
      expect([lost.selected, lost.top, lost.armed]).toEqual([0, 0, null]);
    });

    it("hidden_event_hides_and_shown_clears", () => {
      const s = typed("t", item("a"));
      const h = run(s, { type: "hidden" }).state;
      expect(h.hidden).toBe(true);
      expect(run(h, { type: "shown" }).state.hidden).toBe(false);
      expect(run(h, { type: "input", text: "u" }).state.hidden).toBe(false);
    });

    it("ready_ignored_before_any_query", () => {
      expect(run(initial, { type: "answersReady" }).effects).toEqual([]);
    });
  });
});
