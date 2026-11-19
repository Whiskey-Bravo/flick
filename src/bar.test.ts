import { describe, it, expect } from "vitest";
import { actionFailureText, arrowActsOnList, sideActs, completion, failureText, highlighted, initial, menuActions, movedEnough, nameSelection, panelItem, queryArgs, queryReply, reduce, retryArgs, rowsFromWheel, shownRows, visibleItems, wantsTick, type Effect, type Event, type Fold, type Item, type State } from "./bar";

const item = (id: string, confirm = false): Item => ({
  id, provider: "apps", title: id, subtitle: "", confirm,
  icon: "builtin:app", origin: "search", pinned: false, pinnable: false, actions: [], complete: "", panel: "", live: false, folder: "",
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

describe("tab completion", () => {
  const on = run(initial, { type: "options", tabCompletion: true }).state;
  const typed = (text: string, ...items: Item[]) => run(on, { type: "input", text }, { type: "results", items }).state;

  it("is off unless switched on: the first row is highlighted and nothing is offered", () => {
    const s = run(initial, { type: "input", text: "fir" }, { type: "results", items: [item("Firefox")] }).state;
    expect(highlighted(s)).toBe(true);
    expect(completion(s)).toBeNull();
    // Tab keeps its old job: the row's menu (this row has no actions, so nothing).
    expect(run(s, key("Tab")).state.text).toBe("fir");
  });

  it("offers the rest of the top result's name, whatever the case", () => {
    expect(completion(typed("fir", item("Firefox"), item("Firewall")))).toBe("Firefox");
    expect(completion(typed("FIRE", item("Firefox")))).toBe("Firefox");
    // Not a prefix (a fuzzy match), already complete, or nothing there: no offer.
    expect(completion(typed("vsc", item("Visual Studio Code")))).toBeNull();
    expect(completion(typed("Firefox", item("Firefox")))).toBeNull();
    expect(completion(typed("zzz"))).toBeNull();
    expect(completion(run(on, { type: "results", items: [item("Firefox")] }).state)).toBeNull();
  });

  it("never offers an answer's text: only names complete", () => {
    const timer: Item = { ...item("t"), provider: "timers", title: "Timer 30 min — rings at 22:09" };
    expect(completion(typed("timer 30", timer))).toBeNull();
    const sum: Item = { ...item("a"), provider: "answers", title: "12 km = 7.456 mi" };
    expect(completion(typed("12 km", sum))).toBeNull();
    // A row that says what Tab fills is offered whoever made it.
    const shortcut: Item = { ...item("s"), provider: "shortcuts", title: "claude", complete: "claude " };
    expect(completion(typed("cl", shortcut))).toBe("claude ");
    expect(completion(typed("sle", { ...item("p"), provider: "power", title: "Sleep" }))).toBe("Sleep");
  });

  it("prefers the row's own completion text over its title", () => {
    const row: Item = { ...item("claude"), title: "claude", complete: "claude " };
    expect(completion(typed("cl", row))).toBe("claude ");
    // The title would match, but the completion text is what Tab fills.
    const path: Item = { ...item("p"), title: "Documents", complete: "C:\Users\Documents\\" };
    expect(completion(typed("doc", path))).toBeNull();
  });

  it("Tab takes the preview and asks again", () => {
    const s = typed("fir", item("Firefox"), item("Firewall"));
    const r = run(s, key("Tab"));
    expect(r.state.text).toBe("Firefox");
    expect(r.effects).toEqual([{ type: "query", text: "Firefox", scope: null }]);
    expect(completion(r.state)).toBeNull();
  });

  it("highlights no row until an arrow or the mouse, and Enter still opens the top one", () => {
    const s = typed("fir", item("Firefox"), item("Firewall"), item("Fire TV"));
    expect(highlighted(s)).toBe(false);
    expect(run(s, key("Enter")).effects.map((e) => e.type)).toEqual(["activate"]);
    // Down brings the highlight out on the row Enter would open; the next Down moves.
    const down = run(s, key("ArrowDown")).state;
    expect([highlighted(down), down.selected]).toEqual([true, 0]);
    expect(run(down, key("ArrowDown")).state.selected).toBe(1);
    // Up brings it out on the last row.
    const up = run(s, key("ArrowUp")).state;
    expect([highlighted(up), up.selected]).toEqual([true, 2]);
    // The mouse highlights the row under it.
    const hover = run(s, { type: "hover", index: 1 }).state;
    expect([highlighted(hover), hover.selected]).toEqual([true, 1]);
  });

  it("stops offering once a row is highlighted, and starts again when the text changes", () => {
    const s = typed("fir", item("Firefox"), item("Firewall"));
    const down = run(s, key("ArrowDown")).state;
    expect(completion(down)).toBeNull();
    const again = run(down, { type: "input", text: "fire" }, { type: "results", items: [item("Firefox")] }).state;
    expect(highlighted(again)).toBe(false);
    expect(completion(again)).toBe("Firefox");
  });

  it("survives the bar closing and opening, and an armed row is always highlighted", () => {
    const reopened = run(typed("fir", item("Firefox")), { type: "hidden" }, { type: "shown" }).state;
    expect(reopened.tabCompletion).toBe(true);
    expect(highlighted(reopened)).toBe(false);
    const armed = run(on, { type: "armed", item: item("x", true) }).state;
    expect([armed.tabCompletion, highlighted(armed)]).toEqual([true, true]);
  });

  it("on the empty bar Up still recalls a past search first", () => {
    const s = run(on, { type: "past", items: ["old"] }, { type: "results", items: [item("a"), item("b")] }).state;
    expect(run(s, key("ArrowUp")).state.text).toBe("old");
    const down = run(s, key("ArrowDown")).state;
    expect([highlighted(down), down.selected, down.text]).toEqual([true, 0, ""]);
  });
});

describe("bar", () => {
  it("shown resets but keeps config error", () => {
    const dirty = run(initial, { type: "input", text: "x" }, { type: "configError", message: "bad toml" }).state;
    const { state } = run(dirty, { type: "shown" });
    expect(state.text).toBe("");
    expect(state.items).toEqual([]);
    expect(state.error).toBe("bad toml");
  });

  it("an empty bar opens on its last rows until the fresh ones arrive", () => {
    const rows: Item[] = [item("a"), { ...item("b"), origin: "recent" }];
    const s = run(initial, { type: "shown" }, { type: "results", items: rows }, key("ArrowDown"), { type: "hidden" }).state;
    const shown = run(s, { type: "shown" }).state;
    expect(shown.items).toEqual(rows);
    expect([shown.selected, shown.top, shown.hidden]).toEqual([1, 0, false]);
    // An Enter before the fresh rows waits for them instead of acting on an old row.
    expect(shown.awaiting).toBe(true);
    const entered = run(shown, key("Enter")).state;
    expect(entered.queued).toBe("none");
  });

  it("rows still waiting for the empty bar's answer are not kept", () => {
    // Typed, cleared and closed before the empty query answered: the rows are for `x`.
    const s = run(
      initial,
      { type: "shown" },
      { type: "results", items: [item("a")] },
      { type: "input", text: "x" },
      { type: "results", items: [item("x")] },
      { type: "input", text: "" },
      { type: "hidden" },
    ).state;
    expect(s.awaiting).toBe(true);
    expect(run(s, { type: "shown" }).state.items).toEqual([]);
  });

  it("an Enter on a kept row runs only if the fresh rows still have that row", () => {
    const a: Item = { ...item("a"), origin: "recent" };
    const b: Item = { ...item("b"), origin: "recent" };
    const closed = run(initial, { type: "shown" }, { type: "results", items: [a, item("c")] }, { type: "hidden" }).state;
    const entered = run(closed, { type: "shown" }, key("Enter")).state;
    expect([entered.kept, entered.queued, entered.items[entered.selected].id]).toEqual([true, "none", "a"]);
    // Still there, though no longer first: it runs, not the new top row.
    const still = run(entered, { type: "results", items: [b, a] });
    expect(still.effects.map((e) => e.type)).toEqual(["activate"]);
    expect(still.state.items[still.state.selected].id).toBe("a");
    // Gone: nothing runs, and the fresh rows show.
    const gone = run(entered, { type: "results", items: [b, item("c")] });
    expect(gone.effects).toEqual([]);
    expect([gone.state.queued, gone.state.kept, gone.state.items[gone.state.selected].id]).toEqual([null, false, "b"]);
  });

  it("a kept row the user moved to stays selected when the fresh rows arrive", () => {
    const a: Item = { ...item("a"), origin: "recent" };
    const closed = run(initial, { type: "shown" }, { type: "results", items: [a, item("b"), item("c")] }, { type: "hidden" }).state;
    const moved = run(closed, { type: "shown" }, key("ArrowDown"), key("ArrowDown")).state;
    expect(moved.items[moved.selected].id).toBe("c");
    const fresh = run(moved, { type: "results", items: [a, item("c"), item("d")] }).state;
    expect(fresh.items[fresh.selected].id).toBe("c");
    // Not moved: the fresh rows open on the latest recent launch as usual.
    const untouched = run(closed, { type: "shown" }, { type: "results", items: [item("z"), a] }).state;
    expect(untouched.items[untouched.selected].id).toBe("a");
  });

  it("a direct hotkey's row is not kept, even once its arm is gone", () => {
    const armed = run(initial, { type: "armed", item: item("x", true) }, { type: "blurred" }, { type: "hidden" }).state;
    expect(armed.armed).toBeNull();
    expect(run(armed, { type: "shown" }).state.items).toEqual([]);
  });

  it("a bar closed with text or in a pick list opens empty", () => {
    const typed = run(withItems(item("a")), { type: "input", text: "x" }, { type: "results", items: [item("x")] }).state;
    expect(run(typed, { type: "shown" }).state.items).toEqual([]);
    const scoped = run(initial, { type: "outcome", outcome: { kind: "step", prompt: "Search", scope: "web" } }, { type: "results", items: [item("w")] }).state;
    expect(run(scoped, { type: "shown" }).state.items).toEqual([]);
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

  it("enter after an empty fill is not run on old items, nor on rows not seen yet", () => {
    const s = run(withItems(item("old")), { type: "outcome", outcome: { kind: "fill", text: "" } }, key("Enter"));
    expect(s.effects).toEqual([]);
    expect(s.state.queued).toBeNull();
    // Text filled in: the Enter waits for its rows.
    const t = run(withItems(item("old")), { type: "outcome", outcome: { kind: "fill", text: "a" } }, key("Enter"));
    expect(t.state.queued).toBe("none");
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

  it("enter right after Esc out of a pick list is not queued", () => {
    const scoped = run(initial, { type: "outcome", outcome: { kind: "step", prompt: "proj", scope: "shortcut:proj" } }, { type: "results", items: [item("Alpha")] }).state;
    const left = run(scoped, key("Escape"), key("Enter"));
    expect(left.state.queued).toBeNull();
    // The empty bar's rows arrive: none of them is opened unseen.
    const r = run(left.state, { type: "results", items: [{ ...item("p"), origin: "pin" }] });
    expect(r.effects).toEqual([]);
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

  it("file menu errors are shown as they come, without the row's name in front", () => {
    const file = item("a.txt");
    expect(actionFailureText(file, "delete", "Couldn't delete a.txt")).toBe("Couldn't delete a.txt");
    expect(actionFailureText(file, "rename", "A file with that name already exists")).toBe("A file with that name already exists");
    expect(actionFailureText(file, "open_terminal", "Can't open a terminal in a network folder without Windows Terminal")).toBe(
      "Can't open a terminal in a network folder without Windows Terminal",
    );
    // Other menu actions still say which row failed.
    expect(actionFailureText(item("Blender"), "copy_path", "boom")).toBe("Blender: boom");
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

    it("empty bar selects the first pin, or without pins the latest recent launch", () => {
      const pinned = shownWith(row("pin", { origin: "pin" }), row("pin2", { origin: "pin" }), row("r1", { origin: "recent" }));
      expect(pinned.selected).toBe(0);
      // Down from there walks the pins, then the recent launches: none is skipped.
      expect(run(pinned, key("ArrowDown")).state.selected).toBe(1);
      expect(run(pinned, key("ArrowDown"), key("ArrowDown")).state.selected).toBe(2);
      // A row above them (an update waiting) is not what Enter opens unasked.
      const update = shownWith(row("u", { provider: "update" }), row("pin", { origin: "pin" }), row("r1", { origin: "recent" }));
      expect(update.selected).toBe(1);
      const none = shownWith(row("u", { provider: "update" }), row("r1", { origin: "recent" }), row("r2", { origin: "recent" }));
      expect(none.selected).toBe(1);
    });

    it("welcome rows keep the selection on top", () => {
      const s = shownWith(row("w", { provider: "welcome" }), row("r", { origin: "recent" }));
      expect(s.selected).toBe(0);
    });

    it("typed text does not use the recent rule", () => {
      const s = run(initial, { type: "input", text: "x" }, { type: "results", items: [row("a"), row("r", { origin: "recent" })] }).state;
      expect(s.selected).toBe(0);
    });

    it("after moving down the empty bar's rows, Up moves back up and wraps, without recalling", () => {
      const s = run(withPast("b"), { type: "results", items: [item("x"), item("y"), item("z")] }).state;
      const down2 = run(s, key("ArrowDown"), key("ArrowDown")).state;
      expect(down2.selected).toBe(2);
      const up1 = run(down2, key("ArrowUp")).state;
      expect([up1.selected, up1.text]).toEqual([1, ""]);
      const top = run(up1, key("ArrowUp")).state;
      expect([top.selected, top.text]).toEqual([0, ""]);
      // On the top row, Up wraps to the last row: no old search appears.
      const wrapped = run(top, key("ArrowUp")).state;
      expect([wrapped.selected, wrapped.text]).toEqual([2, ""]);
    });

    it("a row picked with the mouse also makes Up move up", () => {
      const s = run(withPast("b"), { type: "results", items: [item("x"), item("y")] }, { type: "hover", index: 1 }).state;
      const up = run(s, key("ArrowUp")).state;
      expect([up.selected, up.text]).toEqual([0, ""]);
    });

    it("a fresh bar still recalls on the first Up, even on the recent row", () => {
      const rows: Item[] = [item("x"), { ...item("y"), origin: "recent" }];
      const s = run(initial, { type: "shown" }, { type: "results", items: rows }, { type: "past", items: ["b"] }).state;
      expect(s.selected).toBe(1);
      expect(run(s, key("ArrowUp")).state.text).toBe("b");
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

    it("after moving through the rows, Up stays in them and wraps instead of recalling", () => {
      const s = run(withPast("b"), { type: "results", items: [row("a"), row("c")] }, key("ArrowDown")).state;
      const up = run(s, key("ArrowUp")).state;
      expect([up.selected, up.text]).toEqual([0, ""]);
      // On the top row Up wraps to the last row; no old search appears.
      const again = run(up, key("ArrowUp")).state;
      expect([again.selected, again.text, again.recall]).toEqual([1, "", -1]);
    });

    it("going back up past a pin never drops into old searches", () => {
      const pin: Item = { ...row("p"), origin: "pin", pinned: true };
      const recent = (id: string): Item => ({ ...row(id), origin: "recent" });
      // The empty bar opens on the pin.
      const open = run(withPast("old"), { type: "results", items: [pin, recent("a"), recent("b")] }).state;
      expect(open.selected).toBe(0);
      const walked = run(open, key("ArrowDown"), key("ArrowDown"), key("ArrowUp"), key("ArrowUp")).state;
      expect([walked.selected, walked.text]).toEqual([0, ""]);
      const past = run(walked, key("ArrowUp")).state;
      expect([past.selected, past.text]).toEqual([2, ""]);
      // As the first move on a fresh bar, Up still recalls.
      expect(run(open, key("ArrowUp")).state.text).toBe("old");
      // And typing starts over: clearing the box gives recall back.
      const typed = run(walked, { type: "input", text: "x" }, { type: "input", text: "" }).state;
      expect(run(typed, key("ArrowUp")).state.text).toBe("old");
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

  it("more_options_effect_carries_the_row", () => {
    const a = rich("a", { provider: "files", actions: ["reveal"] });
    const b = rich("C:/x/b.txt", { provider: "files", actions: ["reveal", "more_options"] });
    const open = run(withItems(a, b), { type: "menu", index: 1 }).state;
    const r = run(open, { type: "menuPick", action: "more_options" });
    // Not `act`: the page measures the row and asks for Windows' own menu.
    expect(r.effects).toEqual([{ type: "moreOptions", item: b, index: 1 }]);
    expect(r.state.pending).toBe(true);
    expect(r.state.menu).toBeNull();
    // A pick on its way already: nothing more is sent.
    expect(run({ ...open, pending: true }, { type: "menuPick", action: "more_options" }).effects).toEqual([]);
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

    it("tab_complete_keeps_pending", () => {
      const p = rich("C:\Games", { provider: "path", complete: "C:\Games\\", actions: ["open_folder"] });
      const r = run({ ...withItems(p), pending: true }, key("Tab"));
      expect(r.state.text).toBe("C:\Games\\");
      expect(r.state.pending).toBe(true);
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

    it("a_non_uninstall_pick_never_fires_an_armed_uninstall", () => {
      const m = rich("m", { actions: ["admin", "copy_path", "uninstall"] });
      const typed = run(initial, { type: "shown" }, { type: "input", text: "x" }, { type: "results", items: [m] }).state;
      const armed = run(typed, key("Tab"), { type: "menuPick", action: "uninstall" }).state;
      expect(armed.armedAction).toBe("uninstall");
      const viaAdmin = run(armed, key("Tab"), { type: "menuPick", action: "admin" });
      expect(viaAdmin.effects).toEqual([{ type: "activate", item: m, modifier: "ctrl_shift", text: "x", scope: null }]);
      const viaCopy = run(armed, key("Tab"), { type: "menuPick", action: "copy_path" });
      expect(viaCopy.effects).toEqual([{ type: "act", item: m, action: "copy_path" }]);
      expect([viaCopy.state.armed, viaCopy.state.armedAction]).toEqual([null, null]);
    });

    it("a_menu_act_pick_is_ignored_while_pending", () => {
      const a = rich("a", { actions: ["copy_path"] });
      const open = run(withItems(a), { type: "menu", index: 0 }).state;
      const r = run({ ...open, pending: true }, { type: "menuPick", action: "copy_path" });
      expect(r.effects).toEqual([]);
      expect(r.state.menu).toBeNull();
    });

    it("an_armed_uninstall_acts_for_enter_with_any_modifier", () => {
      for (const k of [key("Enter", true), key("Enter", true, true)]) {
        expect(run(armedState().state, k).effects).toEqual([{ type: "act", item: u, action: "uninstall" }]);
      }
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

  describe("delete and rename", () => {
    const f = (id: string, title = id): Item => rich(id, { provider: "files", title, actions: ["rename", "delete"] });
    const a = f("C:/x/a.txt", "a.txt");
    const b = f("C:/x/b.txt", "b.txt");
    const c = f("C:/x/report.final.docx", "report.final.docx");
    const typed = (...items: Item[]) =>
      run(initial, { type: "shown" }, { type: "input", text: "x" }, { type: "results", items }).state;
    const pick = (s: State, action: "delete" | "rename") => run(s, key("Tab"), { type: "menuPick", action });

    it("delete_needs_a_second_enter", () => {
      const first = pick(typed(a), "delete");
      expect(first.effects).toEqual([]);
      expect([first.state.armed, first.state.armedAction, first.state.pending]).toEqual([a.id, "delete", false]);
      const second = run(first.state, key("Enter"));
      expect(second.effects).toEqual([{ type: "act", item: a, action: "delete" }]);
      expect([second.state.armed, second.state.armedAction, second.state.pending]).toEqual([null, null, true]);
      // Moving off the row disarms it, so Enter opens whatever is selected.
      const moved = run(typed(a, b), key("Tab"), { type: "menuPick", action: "delete" }, key("ArrowDown"));
      expect([moved.state.armed, moved.state.armedAction]).toEqual([null, null]);
      expect(run(moved.state, key("Enter")).effects.map((e) => e.type)).toEqual(["activate"]);
    });

    it("delete_arm_follows_the_row_not_the_index", () => {
      const s = run(pick(typed(a, b), "delete").state, { type: "answersReady" }).state;
      const moved = run(s, { type: "results", items: [b, a] }).state;
      expect([moved.armed, moved.armedAction, moved.selected]).toEqual([a.id, "delete", 1]);
      expect(run(moved, key("Enter")).effects).toEqual([{ type: "act", item: a, action: "delete" }]);
      const gone = run(s, { type: "results", items: [b] }).state;
      expect([gone.armed, gone.armedAction]).toEqual([null, null]);
    });

    it("a successful delete asks again for the same text and stays at the index", () => {
      const s = run(typed(a, b, c), { type: "hover", index: 1 }).state;
      const r = run(s, { type: "deleted", item: b });
      expect(r.effects).toEqual([{ type: "query", text: "x", scope: null, keepSelection: true }]);
      expect(r.state.pending).toBe(false);
      const after = run(r.state, { type: "results", items: [a, c] }).state;
      expect(after.selected).toBe(1);
      // At the end of the list the selection is clamped.
      const last = run(run(typed(a, b), { type: "hover", index: 1 }).state, { type: "deleted", item: b }).state;
      expect(run(last, { type: "results", items: [a] }).state.selected).toBe(0);
    });

    it("a folder read finishing before the re-query's answer keeps the selection where the deleted row was", () => {
      // The re-query after a delete starts a background read of the folder;
      // its `answers:ready` can reach the page before the re-query's rows do.
      const s = run(typed(a, b, c), { type: "hover", index: 1 }).state;
      const r = run(s, { type: "deleted", item: b }, { type: "answersReady" });
      expect(r.state.keep).toEqual({ provider: "files", id: b.id, index: 1 });
      // Only the newest query's answer reaches the page (main.ts drops older ones).
      expect(r.effects.filter((e) => e.type === "query")).toEqual([{ type: "query", text: "x", scope: null, keepSelection: true }]);
      const after = run(r.state, { type: "results", items: [a, c] }).state;
      expect(after.selected).toBe(1);
      // The same for a rename: the selection follows the renamed row.
      const started = run(typed(a, b, c), { type: "hover", index: 1 }, key("Tab"), { type: "menuPick", action: "rename" });
      const sent = run(started.state, { type: "input", text: "z.txt" }, key("Enter")).state;
      const z = f("C:/x/z.txt", "z.txt");
      const renamed = run(sent, { type: "renamed", id: z.id }, { type: "answersReady" }, { type: "results", items: [a, c, z] }).state;
      expect(renamed.selected).toBe(2);
    });

    describe("inside an unfolded folder", () => {
      const dirRow: Item = rich("C:/x/d/", { provider: "files", title: "d", folder: "C:/x/d/", actions: [] });
      const inA = f("C:/x/d/a.txt", "a.txt");
      const inB = f("C:/x/d/b.txt", "b.txt");
      const inC = f("C:/x/d/c.txt", "c.txt");
      // The folder d unfolded over [a, b, c]; the selection on `at` (a row inside).
      const unfoldedAt = (at: number) => {
        let s = typed(dirRow);
        s = run(s, { type: "key", key: "ArrowRight", ctrl: false, shift: false }).state;
        const fkey = shownRows(s)[0].key;
        s = run(s, { type: "unfolded", key: fkey, path: "C:/x/d/", reply: { state: "ready", rows: [inA, inB, inC], more: 0, typed: "" } }).state;
        return { s: run(s, { type: "hover", index: at }).state, fkey };
      };
      const reply = (fkey: string, rows: Item[]): Event => ({ type: "unfolded", key: fkey, path: "C:/x/d/", reply: { state: "ready", rows, more: 0, typed: "" } });

      it("delete_inside_an_unfolded_folder_refreshes_that_folder", () => {
        const { s, fkey } = unfoldedAt(2);
        expect(shownRows(s)[2].id).toBe(inB.id);
        const r = run(s, { type: "deleted", item: inB });
        expect(r.effects).toEqual([
          { type: "query", text: "x", scope: null, keepSelection: true },
          { type: "unfold", key: fkey, path: "C:/x/d/" },
        ]);
        // The folder answers without the deleted file: its place is taken by the next row.
        const after = run(r.state, { type: "results", items: [dirRow] }, reply(fkey, [inA, inC])).state;
        expect(shownRows(after).map((x) => x.id)).toEqual([dirRow.id, inA.id, inC.id]);
        expect(shownRows(after)[after.selected].id).toBe(inC.id);
      });

      it("a folder read again keeps its rows while it answers reading, and the ready answer replaces them", () => {
        const { s, fkey } = unfoldedAt(2);
        const r = run(s, { type: "deleted", item: inB });
        const reading: Event = { type: "unfolded", key: fkey, path: "C:/x/d/", reply: { state: "reading", rows: [], more: 0, typed: "" } };
        const waiting = run(r.state, { type: "results", items: [dirRow] }, reading).state;
        expect(shownRows(waiting).map((x) => x.id)).toEqual([dirRow.id, inA.id, inB.id, inC.id]);
        // The read finished: the fold is asked again, and its ready answer drops the file.
        const again = run(waiting, { type: "answersReady" });
        expect(again.effects).toContainEqual({ type: "unfold", key: fkey, path: "C:/x/d/" });
        const after = run(again.state, reply(fkey, [inA, inC])).state;
        expect(shownRows(after).map((x) => x.id)).toEqual([dirRow.id, inA.id, inC.id]);
        expect(shownRows(after)[after.selected].id).toBe(inC.id);
        expect(after.unfolded[fkey].refresh).toBeUndefined();
      });

      it("rename_inside_an_unfolded_folder_selects_the_renamed_row", () => {
        const { s, fkey } = unfoldedAt(1);
        const started = pick(s, "rename");
        const sent = run(started.state, { type: "input", text: "z.txt" }, key("Enter")).state;
        const ok = run(sent, { type: "renamed", id: "C:/x/d/z.txt" });
        expect(ok.effects).toEqual([
          { type: "query", text: "x", scope: null, keepSelection: true },
          { type: "unfold", key: fkey, path: "C:/x/d/" },
        ]);
        const z = f("C:/x/d/z.txt", "z.txt");
        const after = run(ok.state, { type: "results", items: [dirRow] }, reply(fkey, [inB, inC, z])).state;
        expect(shownRows(after).map((x) => x.id)).not.toContain(inA.id);
        expect(shownRows(after)[after.selected].id).toBe(z.id);
      });
    });

    describe("rename", () => {
      const start = (items: Item[] = [a, c], index = 1) => {
        const s = run(typed(...items), { type: "hover", index }).state;
        return pick(s, "rename");
      };
      const kind = (r: ReturnType<typeof run>) => r.effects.map((e) => e.type);

      it("rename_mode_selects_the_base_name", () => {
        expect(nameSelection("report.final.docx", false)).toEqual([0, 12]);
        expect(nameSelection(".gitignore", false)).toEqual([0, 10]);
        expect(nameSelection("a.b", true)).toEqual([0, 3]);
        expect(nameSelection("README", false)).toEqual([0, 6]);
        const r = start();
        expect(r.state.text).toBe("report.final.docx");
        expect(r.effects).toEqual([{ type: "selectName", start: 0, end: 12 }]);
        // A folder row (its id ends in a backslash) selects the whole name.
        const dir = f("C:/x/a.b" + String.fromCharCode(92), "a.b");
        expect(start([dir], 0).effects).toEqual([{ type: "selectName", start: 0, end: 3 }]);
      });

      it("rename_enter_sends_the_new_name_for_the_original_row", () => {
        const r = start();
        // A re-query arrives that moves the rows: the name still goes to the row renamed.
        const moved = run(r.state, { type: "results", items: [c, a] }).state;
        const typedName = run(moved, { type: "input", text: "new.docx" }).state;
        const sent = run(typedName, key("Enter"));
        expect(sent.effects).toEqual([{ type: "rename", item: c, name: "new.docx" }]);
        expect(sent.state.pending).toBe(true);
        expect(sent.state.rename).not.toBeNull();
      });

      it("rename_esc_restores_text_and_selection", () => {
        const r = start();
        const edited = run(r.state, { type: "input", text: "zzz" }).state;
        const esc = run(edited, key("Escape"));
        expect(esc.effects).toEqual([]);
        expect([esc.state.rename, esc.state.text, esc.state.selected]).toEqual([null, "x", 1]);
      });

      it("rename_esc_while_the_rename_is_on_its_way_does_nothing", () => {
        // Esc would say "cancelled" while the rename still happens.
        const sent = run(start().state, { type: "input", text: "new.docx" }, key("Enter")).state;
        const esc = run(sent, key("Escape"));
        expect(esc.state).toBe(sent);
        expect(esc.effects).toEqual([]);
        expect([esc.state.rename !== null, esc.state.text, esc.state.pending]).toEqual([true, "new.docx", true]);
      });

      it("an_enter_queued_before_rename_never_fires_in_rename_mode", () => {
        // Enter pressed while results were on their way, then Rename picked.
        const s = run(typed(a, c), { type: "hover", index: 1 }, { type: "input", text: "xy" }, key("Enter")).state;
        expect(s.queued).toBe("none");
        const r = pick(run(s, { type: "results", items: [a, c] }).state, "rename");
        expect(r.state.queued).toBeNull();
        // Rename picked from the pointer's menu while still awaiting: the late results open nothing.
        const late = run(typed(a, c), { type: "input", text: "xy" }, key("Enter"), { type: "menu", index: 1 }, { type: "menuPick", action: "rename" });
        expect([late.state.rename !== null, late.state.queued]).toEqual([true, null]);
        const after = run(late.state, { type: "results", items: [a, c] });
        expect(after.effects.map((e) => e.type)).not.toContain("activate");
        expect(after.state.rename).not.toBeNull();
      });

      it("rename_failure_keeps_the_mode_with_the_error", () => {
        const sent = run(start().state, key("Enter")).state;
        const failed = run(sent, { type: "failed", message: "A file with that name already exists" }).state;
        expect(failed.rename).not.toBeNull();
        expect(failed.pending).toBe(false);
        expect(failed.error).toContain("already exists");
        // Typing again clears the error and keeps the mode.
        const again = run(failed, { type: "input", text: "other.docx" }).state;
        expect([again.rename === null, again.error]).toEqual([false, null]);
      });

      it("rename_success_selects_the_renamed_row", () => {
        const sent = run(start().state, { type: "input", text: "new.docx" }, key("Enter")).state;
        const ok = run(sent, { type: "renamed", id: "C:/x/new.docx" });
        expect(ok.effects).toEqual([{ type: "query", text: "x", scope: null, keepSelection: true }]);
        expect([ok.state.rename, ok.state.text, ok.state.pending]).toEqual([null, "x", false]);
        const renamed = rich("C:/x/new.docx", { provider: "files", title: "new.docx" });
        const after = run(ok.state, { type: "results", items: [renamed, a] }).state;
        expect(after.selected).toBe(0);
      });

      it("rename_declined_in_windows_prompt_stays_in_rename_mode_with_no_error", () => {
        const sent = run(start().state, { type: "input", text: "new.docx" }, key("Enter")).state;
        expect(sent.pending).toBe(true);
        const r = run(sent, { type: "renameDeclined" });
        expect(r.effects).toEqual([]);
        expect([r.state.rename === null, r.state.pending, r.state.error, r.state.failure, r.state.text]).toEqual([false, false, null, null, "new.docx"]);
        // Enter tries again.
        expect(kind(run(r.state, key("Enter")))).toEqual(["rename"]);
      });

      it("typing_in_rename_mode_sends_no_query", () => {
        const r = start();
        const t = run(r.state, { type: "input", text: "q" });
        expect(t.effects).toEqual([]);
        expect(t.state.text).toBe("q");
        expect(t.state.rename).not.toBeNull();
        // The list, the menu and the mouse do nothing meanwhile.
        for (const e of [key("Tab"), key("ArrowDown"), key("ArrowUp"), key("p", true), key("F1")]) {
          const k = run(t.state, e);
          expect([kind(k), k.state.selected, k.state.menu, k.state.text]).toEqual([[], 1, null, "q"]);
        }
        for (const e of [{ type: "menu", index: 0 }, { type: "click", index: 0, ctrl: false, shift: false }, { type: "hover", index: 0 }] as Event[]) {
          expect(run(t.state, e).state).toBe(t.state);
        }
        expect(sideActs(t.state, true)).toBe(false);
      });

      it("hiding leaves rename mode", () => {
        for (const e of [{ type: "blurred" }, { type: "hidden" }] as Event[]) {
          const s = run(start().state, e).state;
          expect([s.rename, s.text]).toEqual([null, "x"]);
        }
      });

      it("a blur while the rename is on its way keeps rename mode and the typed name", () => {
        // Windows' own prompt (overwrite, change the extension) takes the focus.
        const sent = run(start().state, { type: "input", text: "new.docx" }, key("Enter")).state;
        const blurred = run(sent, { type: "blurred" }).state;
        expect([blurred.rename !== null, blurred.text, blurred.pending]).toEqual([true, "new.docx", true]);
        // No in the prompt: the typed name is still there to send again.
        const declined = run(blurred, { type: "renameDeclined" }).state;
        expect([declined.rename !== null, declined.text]).toEqual([true, "new.docx"]);
        expect(kind(run(declined, key("Enter")))).toEqual(["rename"]);
        // Yes: the rename ends as usual, back on the text from before.
        const ok = run(blurred, { type: "renamed", id: "C:/x/new.docx" });
        expect([ok.state.rename, ok.state.text]).toEqual([null, "x"]);
        expect(ok.effects).toEqual([{ type: "query", text: "x", scope: null, keepSelection: true }]);
      });
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

    it("tab_on_a_row_scrolled_out_of_view_brings_it_back_before_the_menu_opens", () => {
      const s = withItems(rich("r0", { actions: ["copy_path"] }), ...Array.from({ length: 19 }, (_, i) => item(`r${i + 1}`)));
      const r = run(s, { type: "wheel", rows: 5 }, key("Tab")).state;
      expect(r.menu?.index).toBe(0);
      expect(r.top).toBe(0);
    });

    it("wheel_closes_an_open_menu", () => {
      const s = withItems(rich("r0", { actions: ["copy_path"] }), ...Array.from({ length: 19 }, (_, i) => item(`r${i + 1}`)));
      const open = run(s, { type: "menu", index: 0 }).state;
      expect(open.menu).not.toBeNull();
      expect(run(open, { type: "wheel", rows: 3 }).state.menu).toBeNull();
      expect(run(open, { type: "wheel", rows: 0 }).state.menu).toBeNull();
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

    it("rows still shown for older text are not kept: the empty bar opens on the first pin", () => {
      // "fi" typed and deleted: the "fi" rows show until the empty bar's answer comes.
      const s = run(initial, { type: "shown" }, { type: "input", text: "fi" }, { type: "results", items: [item("Firefox")] }, { type: "input", text: "" }).state;
      const r = run(s, { type: "updateChanged" });
      expect(r.state.keep).toBeNull();
      const update = rich("update", { provider: "update" });
      const pin = rich("p", { origin: "pin" });
      const after = run(r.state, { type: "results", items: [update, pin] }).state;
      expect(after.selected).toBe(1);
    });
  });

  describe("live rows", () => {
    const clock: Item = { ...item("now:local"), provider: "answers", actions: ["copy"], live: true };
    const shown = (...items: Item[]) =>
      run(initial, { type: "shown" }, { type: "input", text: "time" }, { type: "results", items }).state;

    it("wants_a_tick_only_while_a_live_row_shows", () => {
      expect(wantsTick(shown(clock))).toBe(true);
      expect(wantsTick(run(shown(clock), { type: "hidden" }).state)).toBe(false);
      expect(wantsTick(shown(item("a")))).toBe(false);
      expect(wantsTick(initial)).toBe(false);
    });

    it("no_tick_while_the_menu_is_open", () => {
      const open = run(shown(clock), key("Tab")).state;
      expect(open.menu).not.toBeNull();
      expect(wantsTick(open)).toBe(false);
      expect(wantsTick(run(open, { type: "menuClose" }).state)).toBe(true);
    });

    it("a_full_tick_keeps_an_armed_live_row_armed_and_selected", () => {
      const u = { ...item("now:u"), provider: "answers", actions: ["uninstall" as const], live: true };
      const armed = run(shown(item("a"), u), key("ArrowDown"), key("Tab"), { type: "menuPick", action: "uninstall" }).state;
      expect([armed.armed, armed.selected]).toEqual(["now:u", 1]);
      const after = run(armed, { type: "answersReady" }, { type: "results", items: [item("a"), u] }).state;
      expect([after.armed, after.selected]).toEqual(["now:u", 1]);
    });

    it("a_tick_requeries_and_keeps_the_selected_live_row", () => {
      const r = run(shown(clock, item("a")), { type: "answersReady" });
      expect(r.effects).toEqual([{ type: "query", text: "time", scope: null, keepSelection: true }]);
      expect(r.state.keep).toEqual({ provider: "answers", id: "now:local" });
    });

    it("a_tick_leaves_the_menu_and_the_armed_row_alone", () => {
      const open = run(shown(clock), key("Tab")).state;
      expect(open.menu).not.toBeNull();
      expect(run(open, { type: "answersReady" }).state.menu).toEqual(open.menu);
      const u = { ...item("u"), actions: ["uninstall" as const] };
      const armed = run(shown(clock, u), key("ArrowDown"), key("Tab"), { type: "menuPick", action: "uninstall" }).state;
      expect(armed.armed).toBe("u");
      expect(run(armed, { type: "answersReady" }).state.armed).toBe("u");
    });

    it("a_menu_opened_after_the_tick_survives_its_reply_when_the_row_is_the_same", () => {
      const ticked = run(shown(clock, item("a")), { type: "answersReady" }).state;
      const open = run(ticked, key("Tab")).state;
      expect(open.menu).not.toBeNull();
      const after = run(open, { type: "results", items: [clock, item("a")] }).state;
      expect(after.menu).toEqual(open.menu);
    });

    it("a_menu_opened_after_the_tick_closes_when_its_row_is_gone", () => {
      const ticked = run(shown(clock, item("a")), { type: "answersReady" }).state;
      const open = run(ticked, key("Tab")).state;
      expect(open.menu).not.toBeNull();
      expect(run(open, { type: "results", items: [item("a"), clock] }).state.menu).toBeNull();
    });

    it("a_tick_asks_only_for_the_visible_live_rows_and_never_queries", () => {
      const tokyo: Item = { ...item("now:Tokyo"), provider: "answers", live: true };
      const s = shown(clock, item("a"), tokyo);
      const r = run(s, { type: "tick" });
      expect(r.effects).toEqual([
        { type: "liveRow", provider: "answers", id: "now:local" },
        { type: "liveRow", provider: "answers", id: "now:Tokyo" },
      ]);
      expect(r.effects.some((f) => f.type === "query")).toBe(false);
      expect(r.state).toBe(s);
    });

    it("a_tick_does_nothing_while_hidden_or_with_the_menu_open_or_without_live_rows", () => {
      expect(run(shown(clock), { type: "hidden" }, { type: "tick" }).effects).toEqual([]);
      expect(run(shown(clock), key("Tab"), { type: "tick" }).effects).toEqual([]);
      expect(run(shown(item("a")), { type: "tick" }).effects).toEqual([]);
    });

    it("a_live_row_result_changes_only_that_item", () => {
      const u = { ...item("now:u"), provider: "answers", actions: ["uninstall" as const], live: true };
      const armed = run(shown(item("a"), u), key("ArrowDown"), key("Tab"), { type: "menuPick", action: "uninstall" }).state;
      const fresh: Item = { ...u, title: "12:00:01", subtitle: "later" };
      const after = run(armed, { type: "liveRowResult", item: fresh }).state;
      expect(after.items[1].title).toBe("12:00:01");
      expect(after.items[1].subtitle).toBe("later");
      expect(after.items[0]).toBe(armed.items[0]);
      expect([after.selected, after.menu, after.armed, after.top]).toEqual([armed.selected, armed.menu, armed.armed, armed.top]);
      expect(after.items[1].actions).toEqual(["uninstall"]);
    });

    it("a_live_row_result_for_a_missing_row_or_while_hidden_is_the_same_state", () => {
      const s = shown(clock, item("a"));
      const other: Item = { ...clock, id: "now:Nowhere", title: "x" };
      expect(reduce(s, { type: "liveRowResult", item: other }).state).toBe(s);
      expect(reduce(s, { type: "liveRowResult", item: { ...clock, provider: "apps", title: "x" } }).state).toBe(s);
      const hidden = run(s, { type: "hidden" }).state;
      expect(reduce(hidden, { type: "liveRowResult", item: { ...clock, title: "x" } }).state).toBe(hidden);
      // A row that is no longer live is not patched either, nor is an unchanged one.
      expect(reduce(s, { type: "liveRowResult", item: { ...clock, title: "x", live: false } }).state).toBe(s);
      expect(reduce(s, { type: "liveRowResult", item: { ...clock } }).state).toBe(s);
    });

    it("answers_ready_without_live_rows_behaves_as_before", () => {
      const s = run(initial, { type: "shown" }, { type: "input", text: "1+1" }, { type: "results", items: [item("a")] }).state;
      expect(run(s, { type: "answersReady" }).effects).toEqual([{ type: "query", text: "1+1", scope: null, keepSelection: true }]);
    });

    it("no_tick_effect_once_hidden", () => {
      expect(run(shown(clock), { type: "hidden" }, { type: "answersReady" }).effects).toEqual([]);
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

  it("panel_item_is_the_single_qr_row", () => {
    const qr = { ...item("q"), panel: "<svg/>" };
    expect(panelItem(withItems(qr))).toEqual(qr);
    expect(panelItem(withItems(qr, item("b")))).toBeNull();
    expect(panelItem(withItems(item("a")))).toBeNull();
  });

  it("save_panel_acts_on_the_panel_row_only_when_it_offers_saving", () => {
    const qr = { ...item("q"), panel: "<svg/>", actions: ["save_picture" as const] };
    const r = run(withItems(qr), { type: "savePanel" });
    expect(r.effects).toEqual([{ type: "act", item: qr, action: "save_picture" }]);
    expect(run(withItems(item("a")), { type: "savePanel" }).effects).toEqual([]);
    expect(run(withItems({ ...qr, actions: [] }), { type: "savePanel" }).effects).toEqual([]);
    expect(run(withItems(qr, item("b")), { type: "savePanel" }).effects).toEqual([]);
  });

  it("tab_on_the_panel_row_opens_a_menu_with_save_picture", () => {
    const qr = { ...item("q"), panel: "<svg/>", actions: ["save_picture" as const] };
    const r = run(withItems(qr), key("Tab"));
    expect(r.state.menu).toEqual({ index: 0, actions: ["save_picture"], selected: 0, at: "row" });
  });

  it("enter_on_a_panel_row_activates_it", () => {
    const qr = { ...item("q"), panel: "<svg/>" };
    const r = run(withItems(qr), key("Enter"));
    expect(r.effects).toEqual([{ type: "activate", item: qr, modifier: "none", text: "", scope: null }]);
  });

  it("copy_is_a_menu_action_that_acts", () => {
    const c = { ...item("c"), actions: ["copy" as const] };
    expect(menuActions(c)).toEqual(["copy"]);
    const r = run(withItems(c), key("Tab"), { type: "menuPick", action: "copy" });
    expect(r.effects).toEqual([{ type: "act", item: c, action: "copy" }]);
    expect(r.state.pending).toBe(true);
  });

  it("ctrl_enter_activates_with_the_ctrl_modifier", () => {
    const c = { ...item("c"), actions: ["copy" as const] };
    const r = run(withItems(c), key("Enter", true));
    expect(r.effects).toEqual([{ type: "activate", item: c, modifier: "ctrl", text: "", scope: null }]);
  });

  it("armed_event_shows_the_item_armed", () => {
    const dirty = run(initial, { type: "input", text: "x" }).state;
    const it = item("w", true);
    const r = reduce(dirty, { type: "armed", item: it });
    expect(r.state.items).toEqual([it]);
    expect(r.state.armed).toBe("w");
    expect(r.state.text).toBe("");
    expect(r.state.hidden).toBe(false);
    expect(r.effects).toEqual([{ type: "loadPast" }]);
    const e = reduce(r.state, key("Enter"));
    expect(e.effects).toEqual([{ type: "activate", item: it, modifier: "none", text: "", scope: null }]);
  });

  it("reopen_fills_the_text_and_keeps_the_error", () => {
    const hiddenState = run(initial, { type: "hidden" }).state;
    const r = reduce(hiddenState, { type: "reopen", text: "work", error: "2 of 5 could not open" });
    expect(r.state.text).toBe("work");
    expect(r.state.hidden).toBe(false);
    expect(r.effects).toEqual([{ type: "query", text: "work", scope: null }, { type: "loadPast" }]);
    const after = run(r.state, { type: "results", items: [item("a")] }).state;
    expect(after.error).toBe("2 of 5 could not open");
    expect(run(after, { type: "input", text: "wor" }).state.error).toBeNull();
  });

  it("reopen_without_an_error_only_fills_the_text", () => {
    const r = reduce(run(initial, { type: "hidden" }).state, { type: "reopen", text: "search ", error: "" });
    expect(r.state.text).toBe("search ");
    expect(r.state.error).toBeNull();
    expect(r.state.failure).toBeNull();
    expect(r.state.hidden).toBe(false);
    expect(r.effects).toEqual([{ type: "query", text: "search ", scope: null }, { type: "loadPast" }]);
  });

  it("scoped_event_opens_the_pick_list", () => {
    const dirty = run(initial, { type: "input", text: "x" }, { type: "hidden" }).state;
    const r = reduce(dirty, { type: "scoped", prompt: "proj", scope: "shortcut:proj" });
    expect(r.state.scope).toEqual({ name: "shortcut:proj", prompt: "proj" });
    expect(r.state.text).toBe("");
    expect(r.state.hidden).toBe(false);
    expect(r.effects).toEqual([{ type: "query", text: "", scope: "shortcut:proj" }, { type: "loadPast" }]);
    const listed = run(r.state, { type: "results", items: [item("a")] }).state;
    const e = reduce(listed, key("Enter"));
    expect(e.effects).toEqual([{ type: "activate", item: item("a"), modifier: "none", text: "", scope: "shortcut:proj" }]);
  });

  it("armed_row_survives_answers_ready", () => {
    const it = item("w", true);
    const armed = run(initial, { type: "armed", item: it }).state;
    expect(armed.queried).toBeNull();
    const r = reduce(armed, { type: "answersReady" });
    expect(r.effects).toEqual([]);
    expect(r.state.items).toEqual([it]);
    expect(r.state.armed).toBe("w");
  });
});

describe("query numbering", () => {
  it("sends_the_page_sequence_with_the_query", () => {
    expect(queryArgs({ text: "fi", scope: null }, 42)).toEqual({ text: "fi", scope: null, seq: 42 });
  });
  it("the_retry_carries_sequence_zero_which_is_never_cancelled", () => {
    expect(retryArgs({ text: "fi", scope: "shortcut:x" })).toEqual({ text: "fi", scope: "shortcut:x", seq: 0 });
  });
  it("a_null_answer_for_the_current_search_asks_again_instead_of_waiting_for_ever", () => {
    expect(queryReply(null)).toBe("ask again");
    const items = [item("a")];
    expect(queryReply(items)).toEqual({ type: "results", items });
  });
});

describe("folder unfolding", () => {
  const SEP = "\u0001";
  const keyOf = (provider: string, id: string, chain = "") => `${chain}${SEP}${provider}${SEP}${id}`;
  const dir = (id: string, path = `C:\\${id}\\`): Item => ({ ...item(id), provider: "files", folder: path, actions: ["reveal"], pinnable: true });
  const entry = (name: string, o: Partial<Item> = {}): Item =>
    ({ ...item(`C:\\x\\${name}`), provider: "path", title: name, subtitle: "Folder", actions: ["reveal", "copy_path"], pinnable: true, ...o });
  const ready = (rows: Item[], more = 0, typed = "~\\x\\"): Omit<Fold, "path"> => ({ state: "ready", rows, more, typed });
  const fold = (path: string, reply: Omit<Fold, "path">): Fold => ({ path, ...reply });
  const ids = (s: State) => shownRows(s).map((r) => [r.provider, r.id, r.depth]);

  describe("shown rows", () => {
    it("nothing_unfolded_shows_the_items_one_to_one", () => {
      const s = withItems(item("a"), dir("D"), item("b"));
      const rows = shownRows(s);
      expect(rows.map(({ depth, chain, key, fold, parent, ...rest }) => rest)).toEqual(s.items);
      expect(rows.map((r) => [r.depth, r.fold, r.parent, r.chain])).toEqual([
        [0, "none", null, ""], [0, "folded", null, ""], [0, "none", null, ""],
      ]);
      expect(rows[1].key).toBe(keyOf("files", "D"));
      expect(initial.unfolded).toEqual({});
    });

    it("an_unfolded_folder_is_followed_by_its_rows_one_level_down", () => {
      const s0 = withItems(dir("D"), item("b"));
      const k = keyOf("files", "D");
      const s = { ...s0, unfolded: { [k]: fold("C:\\D\\", ready([entry("x"), entry("y.txt", { subtitle: "1 KB" })])) } };
      expect(ids(s)).toEqual([["files", "D", 0], ["path", "C:\\x\\x", 1], ["path", "C:\\x\\y.txt", 1], ["apps", "b", 0]]);
      const rows = shownRows(s);
      expect(rows[0].fold).toBe("unfolded");
      expect([rows[1].parent, rows[1].chain, rows[1].key]).toEqual([k, k, keyOf("path", "C:\\x\\x", k)]);
    });

    it("a_folder_inside_unfolded_too_is_two_levels_down", () => {
      const k = keyOf("files", "D");
      const sub = entry("sub", { folder: "C:\\x\\sub\\" });
      const k2 = keyOf("path", sub.id, k);
      const s = {
        ...withItems(dir("D")),
        unfolded: { [k]: fold("C:\\D\\", ready([sub, entry("z")])), [k2]: fold("C:\\x\\sub\\", ready([entry("deep")])) },
      };
      expect(ids(s)).toEqual([["files", "D", 0], ["path", sub.id, 1], ["path", "C:\\x\\deep", 2], ["path", "C:\\x\\z", 1]]);
      expect(shownRows(s)[2].parent).toBe(k2);
    });

    it("the_same_folder_shown_twice_has_two_keys_and_only_the_unfolded_one_shows_rows", () => {
      const sub = entry("sub", { folder: "C:\\x\\sub\\" });
      const k = keyOf("files", "D");
      const s = { ...withItems(dir("D"), sub), unfolded: { [k]: fold("C:\\D\\", ready([sub])) } };
      const rows = shownRows(s);
      expect(rows.map((r) => [r.id, r.depth, r.fold])).toEqual([["D", 0, "unfolded"], [sub.id, 1, "folded"], [sub.id, 0, "folded"]]);
      expect(rows[1].key).not.toBe(rows[2].key);
      const deeper = { ...s, unfolded: { ...s.unfolded, [rows[2].key]: fold("C:\\x\\sub\\", ready([entry("q")])) } };
      expect(ids(deeper)).toEqual([["files", "D", 0], ["path", sub.id, 1], ["path", sub.id, 0], ["path", "C:\\x\\q", 1]]);
    });

    it("each_folder_state_shows_one_row_made_by_the_page", () => {
      const k = keyOf("files", "D");
      const made = (reply: Omit<Fold, "path">) => shownRows({ ...withItems(dir("D")), unfolded: { [k]: fold("C:\\D\\", reply) } }).slice(1);
      const plain = { subtitle: "", confirm: false, icon: "", origin: "search", pinned: false, pinnable: false, actions: [], complete: "", panel: "", live: false, folder: "" };
      const expectOne = (reply: Omit<Fold, "path">, id: string, title: string) => {
        const rows = made(reply);
        expect(rows).toHaveLength(1);
        expect(rows[0]).toEqual({ ...plain, provider: "fold", id, title, depth: 1, chain: k, key: keyOf("fold", id, k), fold: "none", parent: k });
      };
      expectOne({ state: "reading", rows: [], more: 0, typed: "" }, "reading", "Reading\u2026");
      expectOne({ state: "missing", rows: [], more: 0, typed: "" }, "missing", "Folder not found");
      expectOne({ state: "denied", rows: [], more: 0, typed: "" }, "denied", "Can't open this folder");
      expectOne(ready([]), "empty", "Empty folder");
    });

    it("more_than_15_shows_15_rows_then_n_more", () => {
      const k = keyOf("files", "D");
      const fifteen = Array.from({ length: 15 }, (_, i) => entry(`f${i}`));
      const rows = shownRows({ ...withItems(dir("D")), unfolded: { [k]: fold("C:\\D\\", ready(fifteen, 9985)) } });
      expect(rows).toHaveLength(17);
      expect(rows.slice(1, 16).every((r) => r.provider === "path" && r.depth === 1)).toBe(true);
      expect([rows[16].provider, rows[16].id, rows[16].title, rows[16].depth]).toEqual(["fold", "more", "9985 more\u2026", 1]);
    });

    it("selection_window_and_menu_index_the_shown_rows", () => {
      const k = keyOf("files", "D");
      const s = { ...withItems(dir("D"), item("b")), unfolded: { [k]: fold("C:\\D\\", ready([entry("x"), entry("y")])) } };
      // Down walks the rows inside, in the order shown.
      const down = run(s, key("ArrowDown"), key("ArrowDown")).state;
      expect(down.selected).toBe(2);
      expect(run(down, key("Enter")).effects).toEqual([{ type: "activate", item: entry("y"), modifier: "none", text: "", scope: null }]);
      expect(run(s, key("ArrowUp")).state.selected).toBe(3);
      expect(visibleItems(s).map((r) => r.id)).toEqual(["D", "C:\\x\\x", "C:\\x\\y", "b"]);
      expect(run(s, { type: "hover", index: 3 }).state.selected).toBe(3);
      expect(run(s, { type: "click", index: 1, ctrl: true, shift: false }).effects).toEqual([
        { type: "activate", item: entry("x"), modifier: "ctrl", text: "", scope: null },
      ]);
      const menu = run(s, { type: "menu", index: 2 }).state;
      expect(menu.menu).toEqual({ index: 2, actions: ["reveal", "copy_path", "pin"], selected: 0, at: "pointer" });
      expect(run(menu, { type: "menuPick", action: "pin" }).effects).toEqual([{ type: "togglePin", item: entry("y") }]);
      expect(run(s, { type: "hover", index: 1 }, key("p", true)).effects).toEqual([{ type: "togglePin", item: entry("x") }]);
      // The window counts the shown rows.
      const many = { ...withItems(dir("D")), unfolded: { [k]: fold("C:\\D\\", ready(Array.from({ length: 15 }, (_, i) => entry(`f${i}`)))) } };
      expect(run(many, { type: "wheel", rows: 100 }).state.top).toBe(8);
      const last = run(many, key("ArrowUp")).state;
      expect([last.selected, last.top]).toEqual([15, 8]);
    });
  });

  const side = (k: "ArrowLeft" | "ArrowRight", ctrl = false, shift = false) => ({ type: "key", key: k, ctrl, shift }) as Event;
  const R = side("ArrowRight");
  const L = side("ArrowLeft");
  const D = dir("D");
  const kD = keyOf("files", "D");
  const sub = entry("sub", { folder: "C:\\x\\sub\\" });
  const kSub = keyOf("path", sub.id, kD);
  const typedIn = (text: string, ...items: Item[]) =>
    run(initial, { type: "shown" }, { type: "input", text }, { type: "results", items }).state;
  const open = (s: State, k: string, f: Fold): State => ({ ...s, unfolded: { ...s.unfolded, [k]: f } });
  const at = (s: State, selected: number): State => ({ ...s, selected, browsed: true });
  const reading = (path: string): Fold => ({ path, state: "reading", rows: [], more: 0, typed: "" });
  // D unfolded (sub folder, a file), then b.
  const tree = () => open(typedIn("proj", D, item("b")), kD, fold("C:\\D\\", ready([sub, entry("f.txt")], 0, "~\\Documents\\Projects\\")));
  const unchanged = (s: State, ...events: Event[]) => {
    const r = run(s, ...events);
    expect(r.state).toBe(s);
    expect(r.effects).toEqual([]);
  };

  describe("right and left", () => {
    it("right_on_a_folded_folder_starts_reading_it_and_the_selection_stays", () => {
      const s = typedIn("proj", item("a"), D);
      const r = run(at(s, 1), R);
      expect(r.state.unfolded).toEqual({ [kD]: reading("C:\\D\\") });
      expect(r.effects).toEqual([{ type: "unfold", key: kD, path: "C:\\D\\" }]);
      expect([r.state.selected, r.state.browsed]).toEqual([1, true]);
      expect(ids(r.state)).toEqual([["apps", "a", 0], ["files", "D", 0], ["fold", "reading", 1]]);
      // Right counts as moving through the rows: the highlight comes out.
      const tc = run(typedIn("proj", D), { type: "options", tabCompletion: true }).state;
      expect(run(tc, R).state.browsed).toBe(true);
    });

    it("right_on_an_unfolded_folder_moves_to_its_first_row_inside", () => {
      const r = run({ ...tree(), browsed: false }, R);
      expect([r.state.selected, r.state.browsed]).toEqual([1, true]);
      expect(r.effects).toEqual([]);
      // While it is still being read, the first row inside is "Reading…".
      expect(run(open(typedIn("proj", D), kD, reading("C:\\D\\")), R).state.selected).toBe(1);
    });

    it("left_on_an_unfolded_folder_folds_it_and_every_folder_under_it", () => {
      const D2 = dir("D2");
      const kD2 = keyOf("files", "D2");
      const s = open(open(open(typedIn("proj", D, D2), kD, fold("C:\\D\\", ready([sub]))), kSub, fold("C:\\x\\sub\\", ready([entry("deep")]))), kD2, fold("C:\\D2\\", ready([entry("q")])));
      const r = run(s, L);
      // D2's key starts with D's: only the folders really under D go.
      expect(r.state.unfolded).toEqual({ [kD2]: s.unfolded[kD2] });
      expect([r.state.selected, r.state.browsed]).toEqual([0, true]);
      expect(r.effects).toEqual([]);
      // An unfolded folder inside folds the same way, leaving its parent open.
      const inner = run(at(s, 1), L).state;
      expect(Object.keys(inner.unfolded).sort()).toEqual([kD, kD2].sort());
      expect(inner.selected).toBe(1);
    });

    it("left_on_a_row_inside_goes_to_the_row_of_its_folder", () => {
      const s = open(tree(), kSub, fold("C:\\x\\sub\\", ready([entry("deep")])));
      // D, sub, deep, f.txt, b
      expect(run(at(s, 2), L).state.selected).toBe(1);
      expect(run(at(s, 3), L).state.selected).toBe(0);
      // A folded folder inside: to its folder's row as well.
      expect(run(at(tree(), 1), L).state.selected).toBe(0);
      // The page's own rows too.
      const more = open(typedIn("proj", item("a"), D), kD, fold("C:\\D\\", ready([entry("x")], 4)));
      expect(shownRows(more)[3].id).toBe("more");
      const back = run(at(more, 3), L);
      expect([back.state.selected, back.state.browsed]).toEqual([1, true]);
      expect(back.effects).toEqual([]);
    });

    it("right_on_the_more_row_is_enter_on_it", () => {
      const s = open(typedIn("proj", D), kD, fold("C:\\D\\", ready([entry("x")], 4, "~\\Documents\\Big\\")));
      const r = run(at(s, 2), R);
      expect(r.state.text).toBe("~\\Documents\\Big\\");
      expect(r.effects).toEqual([{ type: "query", text: "~\\Documents\\Big\\", scope: null }]);
      expect(r.state.unfolded).toEqual({});
    });

    it("other_rows_do_nothing", () => {
      const s = tree();
      unchanged(at(s, 2), R); // a file inside
      unchanged(at(s, 3), R); // a result that is no folder
      unchanged(at(s, 3), L);
      unchanged(at(typedIn("proj", D), 0), L); // a folded result
      unchanged(at(open(typedIn("proj", D), kD, reading("C:\\D\\")), 1), R); // "Reading…"
    });

    it("ignored_with_the_menu_open_a_row_armed_answers_awaited_an_activation_pending_ctrl_shift_or_a_panel", () => {
      const s = typedIn("proj", D, item("b"));
      const menu = run(s, { type: "menu", index: 0 }).state;
      expect(menu.menu).not.toBeNull();
      unchanged(menu, R);
      unchanged({ ...s, armed: "b" }, R);
      unchanged({ ...s, pending: true }, R);
      unchanged(s, side("ArrowRight", true));
      unchanged(s, side("ArrowRight", false, true));
      unchanged(tree(), side("ArrowLeft", true));
      const qr = typedIn("qr x", { ...D, panel: "<svg/>" });
      unchanged(qr, R);
    });

    it("review_focus_2_right_while_older_results_are_awaited_unfolds_nothing", () => {
      const waiting = run(typedIn("proj", D), { type: "input", text: "proje" }).state;
      expect(waiting.awaiting).toBe(true);
      unchanged(waiting, R);
      unchanged({ ...tree(), awaiting: true }, L);
    });
  });

  describe("replies", () => {
    const asked = () => run(typedIn("proj", D, item("b")), R).state;

    it("a_reply_is_stored_for_its_folder", () => {
      const r = run(asked(), { type: "unfolded", key: kD, path: "C:\\D\\", reply: ready([entry("x")], 0, "~\\D\\") });
      expect(r.state.unfolded).toEqual({ [kD]: fold("C:\\D\\", ready([entry("x")], 0, "~\\D\\")) });
      expect(ids(r.state)).toEqual([["files", "D", 0], ["path", "C:\\x\\x", 1], ["apps", "b", 0]]);
      expect(r.effects).toEqual([]);
    });

    it("a_reply_keeps_the_selection_on_its_row", () => {
      // Selection on b below the folder: "Reading…" becomes three rows, b stays selected.
      const s = at(asked(), 2);
      expect(shownRows(s)[2].id).toBe("b");
      const r = run(s, { type: "unfolded", key: kD, path: "C:\\D\\", reply: ready([entry("x"), entry("y"), entry("z")]) });
      expect(shownRows(r.state)[r.state.selected].id).toBe("b");
    });

    it("review_focus_1_a_reply_for_a_folder_folded_or_a_search_changed_is_dropped", () => {
      const reply = { type: "unfolded", key: kD, path: "C:\\D\\", reply: ready([entry("x")]) } as const;
      // Folded again before the answer came.
      const folded = run(asked(), L).state;
      expect(folded.unfolded).toEqual({});
      unchanged(folded, reply);
      // The text changed.
      const typed = run(asked(), { type: "input", text: "proje" }).state;
      unchanged(typed, reply);
      // The bar was hidden.
      unchanged(run(asked(), { type: "hidden" }).state, reply);
      // A reply for another path under the same key (an older unfolding) is never spliced in.
      unchanged(asked(), { ...reply, path: "C:\\Other\\" });
    });

    it("a_late_reading_reply_never_undoes_an_answer", () => {
      const answered = run(asked(), { type: "unfolded", key: kD, path: "C:\\D\\", reply: ready([entry("x")]) }).state;
      unchanged(answered, { type: "unfolded", key: kD, path: "C:\\D\\", reply: { state: "reading", rows: [], more: 0, typed: "~\\D\\" } });
    });

    it("answers_ready_asks_again_for_every_folder_still_reading", () => {
      const s = open(open(asked(), kSub, reading("C:\\x\\sub\\")), keyOf("files", "E"), fold("C:\\E\\", ready([])));
      expect(run(s, { type: "answersReady" }).effects).toEqual([
        { type: "query", text: "proj", scope: null, keepSelection: true },
        { type: "unfold", key: kD, path: "C:\\D\\" },
        { type: "unfold", key: kSub, path: "C:\\x\\sub\\" },
      ]);
      // On the empty bar, where today nothing is asked again.
      const empty = run(run(initial, { type: "shown" }, { type: "results", items: [D] }).state, R).state;
      expect(run(empty, { type: "answersReady" }).effects).toEqual([{ type: "unfold", key: kD, path: "C:\\D\\" }]);
      // Never while hidden.
      expect(run({ ...s, hidden: true }, { type: "answersReady" }).effects).toEqual([]);
    });
  });

  describe("rows made by the page", () => {
    const states: Omit<Fold, "path">[] = [
      { state: "reading", rows: [], more: 0, typed: "" },
      { state: "missing", rows: [], more: 0, typed: "~\\D\\" },
      { state: "denied", rows: [], more: 0, typed: "~\\D\\" },
      ready([]),
    ];

    it("enter_on_more_puts_the_folder_path_in_the_bar", () => {
      const s = at(open(typedIn("proj", D), kD, fold("C:\\D\\", ready([entry("x")], 4, "~\\Documents\\Big\\"))), 2);
      const r = run(s, key("Enter"));
      expect([r.state.text, r.state.items, r.state.awaiting]).toEqual(["~\\Documents\\Big\\", [], true]);
      expect(r.effects).toEqual([{ type: "query", text: "~\\Documents\\Big\\", scope: null }]);
      // A click on it too.
      expect(run(s, { type: "click", index: 2, ctrl: false, shift: false }).state.text).toBe("~\\Documents\\Big\\");
    });

    it("the_other_rows_do_nothing_on_enter_tab_ctrl_p_or_a_click", () => {
      for (const reply of states) {
        const s = at(open(typedIn("proj", D), kD, fold("C:\\D\\", reply)), 1);
        expect(shownRows(s)[1].provider).toBe("fold");
        unchanged(s, key("Enter"));
        unchanged(s, key("Tab"));
        unchanged(s, key("p", true));
        expect(run(s, { type: "click", index: 1, ctrl: false, shift: false }).effects).toEqual([]);
        expect(run(s, { type: "menu", index: 1 }).state.menu).toBeNull();
      }
      const more = at(open(typedIn("proj", D), kD, fold("C:\\D\\", ready([entry("x")], 4))), 2);
      unchanged(more, key("Tab"));
      unchanged(more, key("p", true));
    });
  });

  describe("rows inside", () => {
    it("enter_on_a_row_inside_activates_it_as_a_path_row", () => {
      const r = run(at(tree(), 2), key("Enter", true));
      expect(r.effects).toEqual([{ type: "activate", item: entry("f.txt"), modifier: "ctrl", text: "proj", scope: null }]);
    });

    it("tab_on_a_row_inside_opens_its_menu_even_while_browsing_a_path", () => {
      const here = entry("here", { folder: "C:\\x\\here\\" });
      const s = open(typedIn("C:\\x\\", here, entry("a.txt")), keyOf("path", here.id), fold("C:\\x\\here\\", ready([entry("in.txt")])));
      const r = run(at(s, 1), key("Tab"));
      expect(r.state.menu).toEqual({ index: 1, actions: ["reveal", "copy_path", "pin"], selected: 0, at: "row" });
      // Path browsing's own rows: no menu on Tab, as today.
      expect(run(at(s, 2), key("Tab")).state.menu).toBeNull();
    });

    it("rows_inside_offer_no_grey_completion", () => {
      const s = { ...tree(), text: "f", tabCompletion: true, browsed: false, selected: 2 };
      expect(shownRows(s)[2].title).toBe("f.txt");
      expect(completion(s)).toBeNull();
    });
  });

  describe("typing while inside", () => {
    it("typing_at_the_end_while_inside_switches_to_the_folder_path", () => {
      const r = run(at(tree(), 2), { type: "input", text: "projf" });
      expect(r.state.text).toBe("~\\Documents\\Projects\\f");
      expect(r.effects).toEqual([{ type: "query", text: "~\\Documents\\Projects\\f", scope: null }]);
      expect([r.state.unfolded, r.state.awaiting, r.state.selected, r.state.browsed]).toEqual([{}, true, 0, false]);
      // A paste at the end the same way.
      expect(run(at(tree(), 1), { type: "input", text: "proj a b" }).state.text).toBe("~\\Documents\\Projects\\ a b");
    });

    it("before_the_folder_answered_its_full_path_is_used", () => {
      const s = at(open(typedIn("proj", D), kD, reading("C:\\D\\")), 1);
      expect(run(s, { type: "input", text: "projx" }).state.text).toBe("C:\\D\\x");
    });

    it("any_other_edit_or_a_row_not_inside_is_an_ordinary_search", () => {
      for (const text of ["pro", "xproj", "pr0j", ""]) {
        const r = run(at(tree(), 2), { type: "input", text });
        expect([r.state.text, r.state.unfolded]).toEqual([text, {}]);
      }
      // On the unfolded folder itself (depth 0): ordinary.
      expect(run(at(tree(), 0), { type: "input", text: "projf" }).state.text).toBe("projf");
    });

    it("inside_a_pick_list_typing_is_ordinary", () => {
      const s = { ...at(tree(), 2), scope: { name: "web", prompt: "Search" } };
      const r = run(s, { type: "input", text: "projf" });
      expect(r.state.text).toBe("projf");
      expect(r.effects).toEqual([{ type: "query", text: "projf", scope: "web" }]);
    });

    it("review_fix_composing_input_inside_is_ordinary_text_until_it_ends", () => {
      const composing = run(at(tree(), 2), { type: "input", text: "projf", composing: true });
      expect([composing.state.text, composing.state.unfolded]).toEqual(["projf", {}]);
      expect(composing.effects).toEqual([{ type: "query", text: "projf", scope: null }]);
      const done = run(at(tree(), 2), { type: "input", text: "projf", composing: false });
      expect(done.state.text).toBe("~\\Documents\\Projects\\f");
    });
  });

  describe("sideActs", () => {
    it("is_true_exactly_where_right_or_left_would_act", () => {
      const s = tree(); // D (unfolded), sub (folded), f.txt, b
      expect(sideActs(at(s, 0), true)).toBe(true); // unfolded folder
      expect(sideActs(at(s, 0), false)).toBe(true);
      expect(sideActs(at(s, 1), true)).toBe(true); // folded folder inside
      expect(sideActs(at(s, 1), false)).toBe(true); // row with a parent
      expect(sideActs(at(s, 2), true)).toBe(false); // a file inside
      expect(sideActs(at(s, 2), false)).toBe(true);
      expect(sideActs(at(s, 3), true)).toBe(false); // any other row
      expect(sideActs(at(s, 3), false)).toBe(false);
      expect(sideActs(at(typedIn("proj", D), 0), true)).toBe(true); // folded result
      expect(sideActs(at(typedIn("proj", D), 0), false)).toBe(false);
      const more = open(typedIn("proj", D), kD, fold("C:\\D\\", ready([entry("x")], 4)));
      expect(sideActs(at(more, 2), true)).toBe(true); // the more row
      expect(sideActs(at(more, 2), false)).toBe(true);
      expect(sideActs(at(open(typedIn("proj", D), kD, reading("C:\\D\\")), 1), true)).toBe(false); // Reading
      expect(sideActs({ ...typedIn("proj"), selected: 0 }, true)).toBe(false); // no rows
    });

    it("is_false_in_every_blocked_state", () => {
      const s = at(tree(), 0);
      expect(sideActs(s, true)).toBe(true);
      const menu = run(s, { type: "menu", index: 0 }).state;
      expect(sideActs(menu, true)).toBe(false);
      expect(sideActs({ ...s, armed: "b" }, true)).toBe(false);
      expect(sideActs({ ...s, awaiting: true }, true)).toBe(false);
      expect(sideActs({ ...s, pending: true }, true)).toBe(false);
      expect(sideActs(typedIn("qr x", { ...D, panel: "<svg/>" }), true)).toBe(false);
    });
  });

  describe("what folds", () => {
    it("everything_folds_when_the_text_changes_a_scope_comes_or_goes_or_the_bar_hides_or_opens", () => {
      const s = tree();
      const folds = (t: State, ...events: Event[]) => expect(run(t, ...events).state.unfolded).toEqual({});
      folds(s, { type: "input", text: "projx" });
      folds(s, key("F1"));
      folds(s, { type: "outcome", outcome: { kind: "fill", text: "C:\\" } });
      folds(s, { type: "outcome", outcome: { kind: "step", prompt: "P", scope: "web" } });
      folds({ ...s, text: "", queried: { text: "", scope: null }, browsed: false, past: ["old"] }, key("ArrowUp"));
      folds({ ...s, scope: { name: "web", prompt: "P" } }, key("Escape"));
      folds(s, key("Escape"));
      folds(s, { type: "scoped", prompt: "P", scope: "web" });
      folds(s, { type: "hidden" });
      folds(s, { type: "shown" });
      folds(s, { type: "reopen", text: "a", error: "" });
    });

    it("a_requery_for_the_same_text_keeps_the_folds_and_the_selected_row", () => {
      const s = at(open(tree(), kSub, fold("C:\\x\\sub\\", ready([entry("deep")]))), 2);
      const asked = run(s, { type: "answersReady" }).state;
      expect(asked.keep).toEqual({ provider: "path", id: "C:\\x\\deep", chain: kSub });
      const r = run(asked, { type: "results", items: [item("new"), D, item("b")] }).state;
      expect(r.unfolded).toBe(s.unfolded);
      expect(shownRows(r)[r.selected].key).toBe(keyOf("path", "C:\\x\\deep", kSub));
      expect(r.selected).toBe(3);
    });

    it("a_folder_gone_from_the_results_folds_with_the_folders_under_it", () => {
      const D2 = dir("D2");
      const kD2 = keyOf("files", "D2");
      const s = at(open(open(tree(), kSub, fold("C:\\x\\sub\\", ready([entry("deep")]))), kD2, fold("C:\\D2\\", ready([entry("q")]))), 3);
      const withD2 = { ...s, items: [D, item("b"), D2] };
      // D, sub, deep, f.txt, b, D2, q: the selection is on f.txt inside D.
      expect(shownRows(withD2)[3].id).toBe("C:\\x\\f.txt");
      const asked = run(withD2, { type: "answersReady" }).state;
      const r = run(asked, { type: "results", items: [item("b"), D2, item("c"), item("d"), item("e")] }).state;
      expect(r.unfolded).toEqual({ [kD2]: s.unfolded[kD2] });
      // b, D2, q, c, d, e: the row that took its place.
      expect(r.selected).toBe(3);
      // Nothing left that far down: the last row.
      expect(run(asked, { type: "results", items: [item("b")] }).state.selected).toBe(0);
      // A result row that is gone: back to the first, as today.
      const onB = run(at(withD2, 4), { type: "answersReady" }).state;
      expect(run(onB, { type: "results", items: [D2, D] }).state.selected).toBe(0);
    });

    it("a_menu_stays_open_while_its_row_keeps_its_place", () => {
      const s = run(tree(), { type: "menu", index: 2 }).state;
      const asked = run(s, { type: "answersReady" }).state;
      expect(run(asked, { type: "results", items: [D, item("b")] }).state.menu).toEqual(s.menu);
      expect(run(asked, { type: "results", items: [item("new"), D, item("b")] }).state.menu).toBeNull();
    });

    it("a_live_row_changing_keeps_the_folds", () => {
      const clock: Item = { ...item("now:local"), provider: "answers", live: true };
      const s = open(typedIn("time", clock, D), kD, fold("C:\\D\\", ready([entry("x")])));
      const r = run(s, { type: "liveRowResult", item: { ...clock, title: "12:01" } }).state;
      expect(r.unfolded).toBe(s.unfolded);
      expect(ids(r)).toEqual(ids(s));
    });

    it("fresh_empty_bar_rows_keep_the_folds_and_select_by_shown_row", () => {
      // The empty bar laid out again after a pin: the first pin is selected, counted in shown rows.
      const p = { ...item("p"), origin: "pin" as const, pinned: true };
      const s0 = run(initial, { type: "shown" }, { type: "results", items: [D, p] }).state;
      expect(s0.selected).toBe(1);
      const s = run(at(s0, 0), R, { type: "unfolded", key: kD, path: "C:\\D\\", reply: ready([entry("x"), entry("y")]) }).state;
      const r = run(s, { type: "pinned", provider: "apps", id: "p", pinned: true }, { type: "results", items: [D, p] }).state;
      expect(r.unfolded).toEqual(s.unfolded);
      expect(shownRows(r)[r.selected].id).toBe("p");
    });

    it("pinning_a_row_inside_marks_it_pinned", () => {
      const s = at(tree(), 2);
      const r = run(s, { type: "pinned", provider: "path", id: "C:\\x\\f.txt", pinned: true }).state;
      expect(shownRows(r)[2].pinned).toBe(true);
      expect(run(r, key("Tab")).state.menu?.actions).toEqual(["reveal", "copy_path", "unpin"]);
    });
  });

  describe("chevron", () => {
    it("a_chevron_click_unfolds_or_folds_that_row_and_selects_it", () => {
      const s = at(typedIn("proj", item("a"), D), 0);
      const r = run(s, { type: "fold", index: 1 });
      expect(r.effects).toEqual([{ type: "unfold", key: kD, path: "C:\\D\\" }]);
      expect([r.state.selected, r.state.unfolded]).toEqual([1, { [kD]: reading("C:\\D\\") }]);
      // Folding from a row inside: the selection goes onto the folder's row.
      const f = run(at(tree(), 2), { type: "fold", index: 0 });
      expect([f.state.selected, f.state.unfolded, f.effects]).toEqual([0, {}, []]);
      // No chevron there: nothing.
      unchanged(at(tree(), 2), { type: "fold", index: 3 });
      unchanged(tree(), { type: "fold", index: 9 });
    });

    it("a_chevron_click_is_ignored_when_the_keys_are", () => {
      const s = typedIn("proj", item("a"), D);
      const menu = run(s, { type: "menu", index: 1 }).state;
      expect(menu.menu).not.toBeNull();
      unchanged(menu, { type: "fold", index: 1 });
      unchanged({ ...s, armed: "a" }, { type: "fold", index: 1 });
      unchanged({ ...s, awaiting: true }, { type: "fold", index: 1 });
      unchanged({ ...s, pending: true }, { type: "fold", index: 1 });
    });
  });
});

describe("arrowActsOnList", () => {
  it("acts_only_with_the_caret_at_the_end_and_no_selection_or_modifier", () => {
    expect(arrowActsOnList("ArrowRight", 5, 5, 5, false)).toBe(true);
    expect(arrowActsOnList("ArrowLeft", 0, 0, 0, false)).toBe(true);
    expect(arrowActsOnList("ArrowRight", 2, 2, 5, false)).toBe(false);
    expect(arrowActsOnList("ArrowLeft", 0, 5, 5, false)).toBe(false);
    expect(arrowActsOnList("ArrowRight", 5, 5, 5, true)).toBe(false);
    expect(arrowActsOnList("ArrowRight", null, null, 5, false)).toBe(false);
  });
});
