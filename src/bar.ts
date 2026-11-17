// Pure bar state machine: no DOM, no Tauri. All decisions live in `reduce`.
export type Item = {
  id: string;
  provider: string;
  title: string;
  subtitle: string;
  confirm: boolean;
  icon: string; // file:<path>, ext:<.ext>, folder or builtin:<name>
  origin: "search" | "pin" | "recent";
  pinned: boolean;
  pinnable: boolean;
  revealable: boolean;
  elevatable: boolean;
};
export type Modifier = "none" | "ctrl" | "ctrl_shift";
export type MenuAction = "pin" | "unpin" | "reveal" | "admin";
export type Menu = { index: number; actions: MenuAction[]; selected: number };
export type State = {
  text: string;
  items: Item[];
  selected: number; // index into the full `items` list
  armed: string | null; // id awaiting a second Enter
  scope: { name: string; prompt: string } | null;
  error: string | null; // what the UI should display
  configError: string | null; // last config error from the back end
  failure: string | null; // last activation failure, until next input/shown
  pending: boolean; // an activate is in flight
  awaiting: boolean; // a query is in flight; `items` belong to older text
  queued: Modifier | null; // Enter pressed while awaiting, run on the next results
  top: number; // index of the first visible row
  past: string[]; // past searches, newest first
  recall: number; // index into `past` being shown, -1 = not recalling
  menu: Menu | null; // the right-click menu, when open
  hidden: boolean; // the bar was hidden (Esc, focus loss) and not shown or typed into since
  queried: { text: string; scope: string | null } | null; // what the last query asked for
  keep: { provider: string; id: string } | null; // row to stay on when the re-query's results arrive
};
export type Effect =
  | { type: "query"; text: string; scope: string | null; keepSelection?: true }
  | { type: "activate"; item: Item; modifier: Modifier; text: string; scope: string | null }
  | { type: "loadPast" }
  | { type: "togglePin"; item: Item }
  | { type: "reveal"; item: Item }
  | { type: "hide" };
export type Event =
  | { type: "shown" }
  | { type: "blurred" }
  | { type: "hidden" }
  | { type: "answersReady" }
  | { type: "input"; text: string }
  | { type: "results"; items: Item[] }
  | { type: "past"; items: string[] }
  | { type: "hover"; index: number }
  | { type: "click"; index: number; ctrl: boolean; shift: boolean }
  | { type: "menu"; index: number }
  | { type: "menuPick"; action: MenuAction }
  | { type: "menuClose" }
  | { type: "pinned"; provider: string; id: string; pinned: boolean }
  | { type: "key"; key: "ArrowUp" | "ArrowDown" | "Enter" | "Escape" | "F1" | "p"; ctrl: boolean; shift: boolean }
  | {
      type: "outcome";
      outcome:
        | { kind: "hide" }
        | { kind: "keep" }
        | { kind: "step"; prompt: string; scope: string }
        | { kind: "fill"; text: string };
    }
  | { type: "failed"; message: string }
  | { type: "queryFailed"; message: string }
  | { type: "configError"; message: string | null };

export const initial: State = {
  text: "",
  items: [],
  selected: 0,
  armed: null,
  scope: null,
  error: null,
  configError: null,
  failure: null,
  pending: false,
  awaiting: false,
  queued: null,
  top: 0,
  past: [],
  recall: -1,
  menu: null,
  hidden: false,
  queried: null,
  keep: null,
};

// A mousemove counts only when the pointer really moved: rows changing under a
// still pointer fire mouse events with the same coordinates.
export type Point = { x: number; y: number };
export function movedEnough(prev: Point | null, next: Point): boolean {
  return !prev || prev.x !== next.x || prev.y !== next.y;
}

// Text for a failed activation: the item's title, then the back end's message.
export function failureText(item: Item, err: unknown): string {
  return `${item.title}: ${String(err)}`;
}

// The window shows 8 rows, or 7 when the error line takes a row's worth of space.
export const MAX_ROWS = 8;
export const capOf = (s: State): number => (s.error ? MAX_ROWS - 1 : MAX_ROWS);
export function visibleItems(s: State): Item[] {
  return s.items.slice(s.top, s.top + capOf(s));
}
// Keep `selected` in range and the window on it, moving `top` the minimum needed.
const fit = (s: State): State => {
  const cap = capOf(s);
  const selected = Math.min(s.selected, Math.max(0, s.items.length - 1));
  let top = Math.min(s.top, Math.max(0, s.items.length - cap));
  if (selected < top) top = selected;
  else if (selected >= top + cap) top = selected - cap + 1;
  return top === s.top && selected === s.selected ? s : { ...s, selected, top };
};

type Result = { state: State; effects: Effect[] };
const done = (state: State, effects: Effect[] = []): Result => ({ state, effects });

// Remembers what the last query asked for, and drops a pending "keep this row"
// unless the query is the re-query that asked for it.
export function reduce(s: State, e: Event): Result {
  const r = step(s, e);
  const q = [...r.effects].reverse().find((f): f is Extract<Effect, { type: "query" }> => f.type === "query");
  const hide = r.effects.some((f) => f.type === "hide");
  const hidden = e.type === "shown" || e.type === "input" ? false : r.state.hidden || e.type === "hidden" || hide;
  if (!q) return hidden === r.state.hidden ? r : done({ ...r.state, hidden }, r.effects);
  const keep = q.keepSelection ? r.state.keep : null;
  return done({ ...r.state, hidden, keep, queried: { text: q.text, scope: q.scope } }, r.effects);
}

function step(s: State, e: Event): Result {
  switch (e.type) {
    case "shown":
      // The empty text is queried so providers can offer rows for an empty bar
      // (the first-start welcome). `awaiting` stays false: an Enter on the empty
      // bar is not queued, and typing right away supersedes this query. Past
      // searches are reloaded: none are recalled until they arrive (history may
      // have been switched off or cleared since the last show).
      return done({ ...initial, configError: s.configError, error: s.configError }, [
        { type: "query", text: "", scope: null },
        { type: "loadPast" },
      ]);
    case "hidden":
      return done({ ...s, hidden: true });
    case "blurred":
      return done({ ...s, armed: null, queued: null, menu: null });
    case "answersReady": {
      // A rate or forecast arrived: ask again, but only when the text is still
      // what was asked and the bar is showing it.
      const q = s.queried;
      if (s.hidden || !q || q.text !== s.text || q.scope !== (s.scope?.name ?? null)) return done(s);
      if (s.text.trim() === "" && !s.scope) return done(s);
      const sel = s.items[s.selected];
      // While typed text is still waiting for its answer, the rows are for older text.
      const keep = sel && !s.awaiting ? { provider: sel.provider, id: sel.id } : null;
      return done({ ...s, keep }, [{ type: "query", text: q.text, scope: q.scope, keepSelection: true }]);
    }
    case "menuClose":
      return done({ ...s, menu: null });
    case "hover":
      if (s.menu || !s.items[e.index] || e.index === s.selected) return done(s);
      return done(fit({ ...s, selected: e.index, armed: null }));
    case "click": {
      if (s.menu) return done({ ...s, menu: null });
      if (!s.items[e.index]) return done(s);
      const sel = e.index === s.selected ? s : fit({ ...s, selected: e.index, armed: null });
      // A click is an explicit choice: it opens the clicked row now, never a
      // queued pick from results still on their way.
      const modifier: Modifier = e.ctrl && e.shift ? "ctrl_shift" : e.ctrl ? "ctrl" : "none";
      return enter({ ...sel, queued: null }, modifier);
    }
    case "menu": {
      const item = s.items[e.index];
      if (!item) return done(s);
      const actions: MenuAction[] = [];
      if (item.pinnable) actions.push(item.pinned ? "unpin" : "pin");
      if (item.revealable) actions.push("reveal");
      if (item.elevatable) actions.push("admin");
      const sel = e.index === s.selected ? s : fit({ ...s, selected: e.index, armed: null });
      if (actions.length === 0) return done({ ...sel, menu: null });
      return done({ ...sel, menu: { index: e.index, actions, selected: 0 } });
    }
    case "menuPick": {
      const m = s.menu;
      const item = m && s.items[m.index];
      const closed = { ...s, menu: null };
      if (!m || !item || !m.actions.includes(e.action)) return done(closed);
      switch (e.action) {
        case "pin":
        case "unpin":
          return done(closed, [{ type: "togglePin", item }]);
        case "reveal":
          return done(closed, [{ type: "reveal", item }]);
        case "admin":
          return enter({ ...closed, selected: m.index }, "ctrl_shift");
      }
    }
    case "configError":
      return done(fit({ ...s, configError: e.message, error: s.failure ?? e.message }));
    case "input":
      return done(
        fit({
          ...s,
          text: e.text,
          recall: -1,
          menu: null,
          selected: 0,
          top: 0,
          armed: null,
          failure: null,
          error: s.configError,
          awaiting: true,
          queued: null,
        }),
        [{ type: "query", text: e.text, scope: s.scope?.name ?? null }],
      );
    case "results": {
      // The empty unscoped bar opens on the latest recent launch, unless a
      // welcome row is shown: that stays on top.
      let selected = 0;
      if (s.keep && !s.queued) {
        // The answers' re-query: stay on the row the user was on, if it is still there,
        // and keep the window where it was.
        const k = s.keep;
        const at = e.items.findIndex((i) => i.provider === k.provider && i.id === k.id);
        const armed = at >= 0 && s.armed === k.id ? s.armed : null;
        return done(fit({ ...s, items: e.items, selected: Math.max(0, at), armed, awaiting: false, menu: null, keep: null }));
      }
      if (s.text === "" && !s.scope && !e.items.some((i) => i.provider === "welcome")) {
        selected = Math.max(0, e.items.findIndex((i) => i.origin === "recent"));
      }
      const next: State = fit({ ...s, items: e.items, selected, top: 0, awaiting: false, queued: null, menu: null, keep: null });
      // An Enter pressed before these results arrived acts on them now.
      return s.queued ? enter(next, s.queued) : done(next);
    }
    case "past":
      return done({ ...s, past: e.items });
    case "pinned": {
      const next = {
        ...s,
        items: s.items.map((i) => (i.provider === e.provider && i.id === e.id ? { ...i, pinned: e.pinned } : i)),
      };
      // The empty bar is laid out by pins: ask for it again.
      const relayout = s.text === "" && !s.scope;
      return done(next, relayout ? [{ type: "query", text: "", scope: null }] : []);
    }
    case "failed":
      // A failed activation brings the bar back.
      return done(fit({ ...s, armed: null, pending: false, hidden: false, failure: e.message, error: e.message }));
    case "queryFailed":
      // Leaves `pending` alone: an activation may still be in flight.
      return done(fit({ ...s, armed: null, awaiting: false, queued: null, failure: e.message, error: e.message }));
    case "outcome": {
      if (e.outcome.kind === "fill") return fill(s, e.outcome.text);
      if (e.outcome.kind !== "step") return done({ ...s, pending: false });
      const { prompt, scope } = e.outcome;
      return done(
        {
          ...s,
          scope: { name: scope, prompt },
          text: "",
          recall: -1,
          items: [],
          selected: 0,
          top: 0,
          armed: null,
          pending: false,
          awaiting: true,
          queued: null,
        },
        [{ type: "query", text: "", scope }],
      );
    }
    case "key":
      return onKey(s, e);
  }
}

function onMenuKey(s: State, m: Menu, e: Extract<Event, { type: "key" }>): Result {
  const n = m.actions.length;
  switch (e.key) {
    case "ArrowUp":
    case "ArrowDown": {
      const step = e.key === "ArrowDown" ? 1 : n - 1;
      return done({ ...s, menu: { ...m, selected: (m.selected + step) % n } });
    }
    case "Enter":
      return reduce(s, { type: "menuPick", action: m.actions[m.selected] });
    case "Escape":
      return done({ ...s, menu: null });
    default:
      return done(s);
  }
}

function onKey(s: State, e: Extract<Event, { type: "key" }>): Result {
  if (s.menu) return onMenuKey(s, s.menu, e);
  switch (e.key) {
    case "ArrowUp":
    case "ArrowDown": {
      if (s.past.length > 0 && !s.scope && (s.text === "" || s.recall >= 0) && (e.key === "ArrowUp" || s.recall >= 0)) {
        return recallStep(s, e.key === "ArrowUp" ? 1 : -1);
      }
      const n = s.items.length;
      if (n === 0) return done({ ...s, armed: null });
      const step = e.key === "ArrowDown" ? 1 : n - 1;
      return done(fit({ ...s, selected: (s.selected + step) % n, armed: null }));
    }
    case "p": {
      const item = s.items[s.selected];
      if (!e.ctrl || !item || !item.pinnable) return done(s);
      return done(s, [{ type: "togglePin", item }]);
    }
    case "F1": {
      // Unlike the fill outcome, this is not an activation finishing: `pending` stays.
      // Help is the whole list, so it leaves a pick list's scope.
      const r = fill({ ...s, scope: null }, "help");
      return done({ ...r.state, pending: s.pending }, r.effects);
    }
    case "Enter": {
      if (s.pending) return done(s);
      const modifier: Modifier = e.ctrl && e.shift ? "ctrl_shift" : e.ctrl ? "ctrl" : "none";
      // The shown items are for older text: wait for the answer to what was typed.
      // Not for a pick list still loading: its first entry was never seen.
      if (s.awaiting) return done({ ...s, queued: s.scope && s.text === "" ? null : modifier });
      return enter(s, modifier);
    }
    case "Escape": {
      const t = { ...s, queued: null };
      if (t.armed) return done({ ...t, armed: null });
      if (t.scope) {
        return done(
          { ...t, scope: null, text: "", recall: -1, items: [], selected: 0, top: 0, awaiting: true },
          [{ type: "query", text: "", scope: null }],
        );
      }
      return done(t, [{ type: "hide" }]);
    }
  }
}

// Up/Down through past searches: the recalled text is queried like typed text.
function recallStep(s: State, by: 1 | -1): Result {
  const recall = Math.min(s.recall + by, s.past.length - 1);
  if (recall === s.recall) return done(s);
  const text = recall < 0 ? "" : s.past[recall];
  return done(
    fit({
      ...s,
      text,
      recall,
      selected: 0,
      top: 0,
      armed: null,
      failure: null,
      error: s.configError,
      awaiting: true,
      queued: null,
    }),
    [{ type: "query", text, scope: null }],
  );
}

// Put `text` in the bar as if typed: nothing armed, queued or pending survives.
function fill(s: State, text: string): Result {
  const next: State = {
    ...s,
    text,
    recall: -1,
    items: [],
    selected: 0,
    top: 0,
    armed: null,
    pending: false,
    queued: null,
    failure: null,
    error: s.configError,
    awaiting: true,
  };
  return done(next, [{ type: "query", text, scope: s.scope?.name ?? null }]);
}

// Enter on the selected item: a confirm item arms first, anything else activates.
function enter(s: State, modifier: Modifier): Result {
  const item = s.items[s.selected];
  if (!item || s.pending) return done(s);
  if (item.confirm && s.armed !== item.id) return done({ ...s, armed: item.id });
  return done({ ...s, armed: null, pending: true }, [{ type: "activate", item, modifier, text: s.text, scope: s.scope?.name ?? null }]);
}
