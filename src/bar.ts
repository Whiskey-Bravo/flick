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
};
export type Effect =
  | { type: "query"; text: string; scope: string | null }
  | { type: "activate"; item: Item; modifier: Modifier; text: string; scope: string | null }
  | { type: "loadPast" }
  | { type: "togglePin"; item: Item }
  | { type: "hide" };
export type Event =
  | { type: "shown" }
  | { type: "blurred" }
  | { type: "input"; text: string }
  | { type: "results"; items: Item[] }
  | { type: "past"; items: string[] }
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
};

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

export function reduce(s: State, e: Event): Result {
  switch (e.type) {
    case "shown":
      // The empty text is queried so providers can offer rows for an empty bar
      // (the first-start welcome). `awaiting` stays false: an Enter on the empty
      // bar is not queued, and typing right away supersedes this query.
      return done({ ...initial, past: s.past, configError: s.configError, error: s.configError }, [
        { type: "query", text: "", scope: null },
        { type: "loadPast" },
      ]);
    case "blurred":
      return done({ ...s, armed: null, queued: null });
    case "configError":
      return done(fit({ ...s, configError: e.message, error: s.failure ?? e.message }));
    case "input":
      return done(
        fit({
          ...s,
          text: e.text,
          recall: -1,
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
      if (s.text === "" && !s.scope && !e.items.some((i) => i.provider === "welcome")) {
        selected = Math.max(0, e.items.findIndex((i) => i.origin === "recent"));
      }
      const next: State = fit({ ...s, items: e.items, selected, top: 0, awaiting: false, queued: null });
      // An Enter pressed before these results arrived acts on them now.
      return s.queued ? enter(next, s.queued) : done(next);
    }
    case "past":
      return done({ ...s, past: e.items });
    case "pinned":
      return done({
        ...s,
        items: s.items.map((i) => (i.provider === e.provider && i.id === e.id ? { ...i, pinned: e.pinned } : i)),
      });
    case "failed":
      return done(fit({ ...s, armed: null, pending: false, failure: e.message, error: e.message }));
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

function onKey(s: State, e: Extract<Event, { type: "key" }>): Result {
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
