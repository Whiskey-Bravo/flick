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
  actions: Action[]; // what the row's actions menu offers, in order
  complete: string; // text Tab puts in the bar, "" = none
  panel: string; // SVG markup from Flick's backend to show large instead of the list, "" = none
  live: boolean; // the row changes by itself: ask again every second while it shows
  folder: string; // absolute path ending in a backslash when the row is a folder that can unfold, "" = not
};
export type Action = "reveal" | "open_folder" | "copy_path" | "admin" | "uninstall" | "close" | "close_window" | "copy" | "save_picture" | "open_terminal" | "compress" | "properties" | "open_with" | "copy_file" | "rename" | "delete" | "keep_offline" | "free_up_space" | "more_options";
export type Modifier = "none" | "ctrl" | "ctrl_shift";
export type MenuAction = Action | "pin" | "unpin";
export type Menu = { index: number; actions: MenuAction[]; selected: number; at: "pointer" | "row" };

// A row's menu: its own actions, then pin or unpin.
export function menuActions(item: Item): MenuAction[] {
  const actions: MenuAction[] = [...item.actions];
  if (item.pinnable) actions.push(item.pinned ? "unpin" : "pin");
  return actions;
}

/** True while the bar shows a row that changes by itself (the clock). No ticks
 *  while the actions menu is open. */
export function wantsTick(s: State): boolean {
  return !s.hidden && !s.menu && s.items.some((i) => i.live);
}

// The row shown as a panel (a QR code): the only item, with markup.
export function panelItem(s: State): Item | null {
  return s.items.length === 1 && s.items[0].panel !== "" ? s.items[0] : null;
}

// An unfolded folder: what the back end answered for it, and the path it was asked for.
export type Fold = { path: string; state: "reading" | "ready" | "missing" | "denied"; rows: Item[]; more: number; typed: string };
// A row as shown: a result, or a row inside an unfolded folder. `chain` is the
// key of the folder row it is inside ("" for a result), so the same folder
// shown twice is two rows; `key` is its identity among the shown rows.
export type Row = Item & { depth: number; chain: string; key: string; fold: "none" | "folded" | "unfolded"; parent: string | null };

// At most this many rows of an unfolded folder show; "N more…" stands for the rest.
const FOLD_ROWS = 15;
const SEP = "\u0001";
const rowKey = (chain: string, provider: string, id: string): string => `${chain}${SEP}${provider}${SEP}${id}`;
// A row the page makes for a folder's state (provider "fold").
const madeRow = (id: string, title: string): Item => ({
  id, provider: "fold", title, subtitle: "", confirm: false, icon: "", origin: "search", pinned: false, pinnable: false,
  actions: [], complete: "", panel: "", live: false, folder: "",
});

function addRows(out: Row[], items: Item[], depth: number, parent: string | null, unfolded: Record<string, Fold>): void {
  const chain = parent ?? "";
  for (const item of items) {
    const key = rowKey(chain, item.provider, item.id);
    const f = item.folder !== "" ? unfolded[key] : undefined;
    out.push({ ...item, depth, chain, key, parent, fold: item.folder === "" ? "none" : f ? "unfolded" : "folded" });
    if (!f) continue;
    if (f.state === "ready") addRows(out, f.rows.slice(0, FOLD_ROWS), depth + 1, key, unfolded);
    const made =
      f.state === "reading" ? madeRow("reading", "Reading…")
      : f.state === "missing" ? madeRow("missing", "Folder not found")
      : f.state === "denied" ? madeRow("denied", "Can't open this folder")
      : f.more > 0 ? madeRow("more", `${f.more} more…`)
      : f.rows.length === 0 ? madeRow("empty", "Empty folder")
      : null;
    if (made) out.push({ ...made, depth: depth + 1, chain: key, key: rowKey(key, made.provider, made.id), parent: key, fold: "none" });
  }
}

// The last answer, so rendering and every key do not build the rows again.
let memo: { items: Item[]; unfolded: Record<string, Fold>; rows: Row[] } | null = null;
/** The rows as shown: `items` in order, each unfolded folder's rows right after
 *  it, recursively. `selected`, `top` and the menu index count these. */
export function shownRows(s: State): Row[] {
  if (memo && memo.items === s.items && memo.unfolded === s.unfolded) return memo.rows;
  const rows: Row[] = [];
  addRows(rows, s.items, 0, null, s.unfolded);
  memo = { items: s.items, unfolded: s.unfolded, rows };
  return rows;
}
// A shown row as the back end knows it: the row's place in the tree stripped.
const plain = (row: Row): Item => {
  const { depth, chain, key, fold, parent, ...item } = row;
  return item;
};
// The folds still shown over these results: one whose folder row is gone goes,
// and every fold under it with it. The same object when nothing goes.
function foldsShown(items: Item[], unfolded: Record<string, Fold>): Record<string, Fold> {
  const keys = Object.keys(unfolded);
  if (keys.length === 0) return unfolded;
  const rows: Row[] = [];
  addRows(rows, items, 0, null, unfolded);
  const open = new Set(rows.filter((r) => r.fold === "unfolded").map((r) => r.key));
  return open.size === keys.length ? unfolded : Object.fromEntries(keys.filter((k) => open.has(k)).map((k) => [k, unfolded[k]]));
}
// What the folder of a row inside would be typed as: as path browsing writes it,
// or its full path before the back end said.
const typedOf = (f: Fold): string => f.typed || f.path;

/** Left and Right act on the list only with the caret at the end of the text,
 *  nothing selected and no modifier held; otherwise they move the caret. */
export function arrowActsOnList(_key: string, selStart: number | null, selEnd: number | null, length: number, mods: boolean): boolean {
  return !mods && selStart === length && selEnd === length;
}

// The mouse wheel scrolls the list by whole rows: the remainder carries over.
export const WHEEL_PX_PER_ROW = 40;
export function rowsFromWheel(acc: number, deltaY: number): { rows: number; acc: number } {
  const total = acc + deltaY;
  const rows = Math.trunc(total / WHEEL_PX_PER_ROW);
  return { rows, acc: total - rows * WHEEL_PX_PER_ROW };
}
export type State = {
  text: string;
  items: Item[];
  selected: number; // index into the shown rows (`shownRows`)
  armed: string | null; // id awaiting a second Enter
  armedAction: "uninstall" | "delete" | null; // what the armed second Enter does, besides activating
  scope: { name: string; prompt: string } | null;
  error: string | null; // what the UI should display
  configError: string | null; // last config error from the back end
  failure: string | null; // last activation failure, until next input/shown
  pending: boolean; // an activate is in flight
  awaiting: boolean; // a query is in flight; `items` belong to older text
  queued: Modifier | null; // Enter pressed while awaiting, run on the next results
  top: number; // index of the first visible shown row
  past: string[]; // past searches, newest first
  recall: number; // index into `past` being shown, -1 = not recalling
  menu: Menu | null; // the right-click menu, when open
  hidden: boolean; // the bar was hidden (Esc, focus loss) and not shown or typed into since
  queried: { text: string; scope: string | null } | null; // what the last query asked for
  keep: { provider: string; id: string; chain?: string; index?: number } | null; // row to stay on when the re-query's results arrive; `chain` only for a row inside a folder; `index`: where to go when that row is gone (a deleted one)
  browsed: boolean; // the rows were moved through (arrows or mouse) since the bar opened or the text changed: Up no longer recalls
  kept: boolean; // `items` are the last show's empty-bar rows, on screen until the fresh ones arrive
  tabCompletion: boolean; // the "Tab completion" setting: a grey preview, and no highlight until the rows are moved through
  unfolded: Record<string, Fold>; // the unfolded folders, by their row's key
  rename: { item: Item; before: { text: string; selected: number; top: number } } | null; // the row being renamed in the bar, and what to restore
};
export type Effect =
  | { type: "query"; text: string; scope: string | null; keepSelection?: true }
  | { type: "activate"; item: Item; modifier: Modifier; text: string; scope: string | null }
  | { type: "loadPast" }
  | { type: "togglePin"; item: Item }
  | { type: "act"; item: Item; action: Action }
  | { type: "liveRow"; provider: string; id: string }
  | { type: "rename"; item: Item; name: string }
  | { type: "selectName"; start: number; end: number }
  | { type: "unfold"; key: string; path: string }
  | { type: "hide" };
export type Event =
  | { type: "shown" }
  | { type: "armed"; item: Item }
  | { type: "reopen"; text: string; error: string }
  | { type: "scoped"; prompt: string; scope: string }
  | { type: "blurred" }
  | { type: "hidden" }
  | { type: "answersReady" }
  | { type: "tick" }
  | { type: "liveRowResult"; item: Item }
  | { type: "updateChanged" }
  | { type: "input"; text: string; composing?: boolean }
  | { type: "results"; items: Item[] }
  | { type: "past"; items: string[] }
  | { type: "hover"; index: number }
  | { type: "click"; index: number; ctrl: boolean; shift: boolean }
  | { type: "menu"; index: number }
  | { type: "menuPick"; action: MenuAction }
  | { type: "savePanel" }
  | { type: "menuClose" }
  | { type: "wheel"; rows: number }
  | { type: "pinned"; provider: string; id: string; pinned: boolean }
  | { type: "key"; key: "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" | "Enter" | "Escape" | "F1" | "p" | "Tab"; ctrl: boolean; shift: boolean }
  | { type: "unfolded"; key: string; path: string; reply: Omit<Fold, "path"> }
  | { type: "fold"; index: number }
  | {
      type: "outcome";
      outcome:
        | { kind: "hide" }
        | { kind: "keep" }
        | { kind: "step"; prompt: string; scope: string }
        | { kind: "fill"; text: string };
    }
  | { type: "failed"; message: string }
  | { type: "renamed"; id: string }
  | { type: "deleted"; item: Item }
  | { type: "queryFailed"; message: string }
  | { type: "configError"; message: string | null }
  | { type: "options"; tabCompletion: boolean };

export const initial: State = {
  text: "",
  items: [],
  selected: 0,
  armed: null,
  armedAction: null,
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
  browsed: false,
  kept: false,
  tabCompletion: false,
  unfolded: {},
  rename: null,
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

/** What the bar selects when a rename starts: the name without its extension
 *  (up to the last dot), the whole name for a folder or a name with no dot
 *  after its first character. */
export function nameSelection(title: string, isFolder: boolean): [number, number] {
  const dot = title.lastIndexOf(".");
  return [0, isFolder || dot <= 0 ? title.length : dot];
}

// A rename ends: the text, the selection and the window are what they were.
const leaveRename = (s: State): State =>
  s.rename === null ? s : { ...s, rename: null, text: s.rename.before.text, selected: s.rename.before.selected, top: s.rename.before.top };

// The window shows 8 rows, or 7 when the error line takes a row's worth of space.
export const MAX_ROWS = 8;
export const capOf = (s: State): number => (s.error ? MAX_ROWS - 1 : MAX_ROWS);
export function visibleItems(s: State): Row[] {
  return shownRows(s).slice(s.top, s.top + capOf(s));
}
// Keep `selected` in range and the window on it, moving `top` the minimum needed.
const fit = (s: State): State => {
  const cap = capOf(s);
  const n = shownRows(s).length;
  const selected = Math.min(s.selected, Math.max(0, n - 1));
  let top = Math.min(s.top, Math.max(0, n - cap));
  if (selected < top) top = selected;
  else if (selected >= top + cap) top = selected - cap + 1;
  return top === s.top && selected === s.selected ? s : { ...s, selected, top };
};

// The empty unscoped bar opens on your first pin, or without pins on the
// latest recent launch, so Down walks every pin and then the recent ones. A
// row above those (an update waiting) is never what Enter opens unasked. A
// welcome row stays on top.
const emptyBarSelection = (items: Item[]): number =>
  items.some((i) => i.provider === "welcome") ? 0 : Math.max(0, items.findIndex((i) => i.origin === "pin" || i.origin === "recent"));

// Whether a row is shown highlighted. With tab completion on, none is until
// the rows are moved through (arrows or mouse), a row is armed, or a row's
// menu is open; Enter opens `selected` either way.
export const highlighted = (s: State): boolean => !s.tabCompletion || s.browsed || s.armed !== null || s.menu !== null;

// Tab completion: the whole text Tab would put in the bar, or null when there
// is nothing to offer. It is the top result's name (or its own completion
// text) when that starts with what is typed, whatever the case; the bar shows
// the part not typed yet in grey. Nothing is offered once a row is
// highlighted: Tab then opens that row's menu, as without the setting.
// The providers whose rows are found by their name: typing the title finds
// the row again. Elsewhere the title is an answer ("Timer 30 min, rings at
// 22:09" for `timer 30`), which is nothing to type.
const NAMED = new Set([
  "apps", "files", "games", "bookmarks", "settings", "power", "system", "chores",
  "snippets", "windows", "recent", "downloads", "flick",
]);

export function completion(s: State): string | null {
  if (!s.tabCompletion || highlighted(s) || s.text === "" || s.recall >= 0) return null;
  const top = shownRows(s)[s.selected];
  if (!top) return null;
  if (top.complete === "" && !NAMED.has(top.provider)) return null;
  const whole = top.complete !== "" ? top.complete : top.title;
  return whole.length > s.text.length && whole.toLowerCase().startsWith(s.text.toLowerCase()) ? whole : null;
}

type Result = { state: State; effects: Effect[] };
// The row to stay on across a re-query: its chain only when it is inside a folder.
const keepOf = (row: Row): NonNullable<State["keep"]> =>
  row.chain === "" ? { provider: row.provider, id: row.id } : { provider: row.provider, id: row.id, chain: row.chain };
const done = (state: State, effects: Effect[] = []): Result => ({ state, effects });

// Remembers what the last query asked for, and drops a pending "keep this row"
// unless the query is the re-query that asked for it.
export function reduce(s: State, e: Event): Result {
  const stepped = step(s, e);
  // A second Enter's meaning goes when the row is no longer armed.
  const r = stepped.state.armed === null && stepped.state.armedAction !== null
    ? done({ ...stepped.state, armedAction: null }, stepped.effects)
    : stepped;
  const q = [...r.effects].reverse().find((f): f is Extract<Effect, { type: "query" }> => f.type === "query");
  const hide = r.effects.some((f) => f.type === "hide");
  const hidden = e.type === "shown" || e.type === "armed" || e.type === "reopen" || e.type === "scoped" || e.type === "input" ? false : r.state.hidden || e.type === "hidden" || hide;
  if (!q) return hidden === r.state.hidden ? r : done({ ...r.state, hidden }, r.effects);
  const keep = q.keepSelection ? r.state.keep : null;
  return done({ ...r.state, hidden, keep, queried: { text: q.text, scope: q.scope } }, r.effects);
}

function step(s: State, e: Event): Result {
  switch (e.type) {
    case "shown": {
      // The empty text is queried so providers can offer rows for an empty bar
      // (the first-start welcome). Typing right away supersedes this query. Past
      // searches are reloaded: none are recalled until they arrive (history may
      // have been switched off or cleared since the last show).
      const fresh: State = { ...initial, tabCompletion: s.tabCompletion, configError: s.configError, error: s.configError };
      const effects: Effect[] = [
        { type: "query", text: "", scope: null },
        { type: "loadPast" },
      ];
      // Closed as an empty bar showing the empty query's rows: they stay up
      // until the fresh ones replace them, so the list doesn't vanish and come
      // back as the bar opens. An Enter meanwhile waits for the fresh rows and
      // runs only if the row it was pressed on is still there (see "results").
      // Otherwise (typed text, a pick list, a direct hotkey's row, rows still
      // waiting for the empty query's answer) the bar opens with no rows and
      // an Enter does nothing until they arrive.
      const emptyRows = s.queried?.text === "" && s.queried.scope === null && !s.awaiting;
      if (s.text === "" && !s.scope && !s.armed && emptyRows && s.items.length > 0) {
        return done(
          fit({ ...fresh, items: s.items, selected: emptyBarSelection(s.items), awaiting: true, kept: true }),
          effects,
        );
      }
      return done(fresh, effects);
    }
    case "armed":
      // A direct hotkey pointed at an item that needs a second Enter: it is the
      // only row, armed. No query, so nothing replaces it.
      return done({ ...initial, tabCompletion: s.tabCompletion, configError: s.configError, error: s.configError, items: [e.item], selected: 0, armed: e.item.id }, [
        { type: "loadPast" },
      ]);
    case "reopen":
      // A workspace run had failures: the bar comes back with its text and the
      // error, which stays through the results and goes at the next input. An
      // empty error (a direct hotkey on an item that needs typing) shows none.
      return done(
        { ...initial, tabCompletion: s.tabCompletion, configError: s.configError, text: e.text, failure: e.error || null, error: e.error || s.configError },
        [
          { type: "query", text: e.text, scope: null },
          { type: "loadPast" },
        ],
      );
    case "scoped":
      // A direct hotkey on an item that asks to choose: the bar opens in its
      // pick list, as after Enter on its row.
      return done(
        { ...initial, tabCompletion: s.tabCompletion, configError: s.configError, error: s.configError, scope: { name: e.scope, prompt: e.prompt }, awaiting: true },
        [
          { type: "query", text: "", scope: e.scope },
          { type: "loadPast" },
        ],
      );
    case "hidden":
      return done({ ...leaveRename(s), hidden: true, unfolded: {} });
    case "blurred":
      return done(fit({ ...leaveRename(s), armed: null, queued: null, menu: null }));
    case "answersReady": {
      // A folder read finished, or a rate or forecast arrived: ask again for
      // every folder still being read, and for the rows when the text is still
      // what was asked and the bar is showing it.
      const again = s.hidden
        ? []
        : Object.entries(s.unfolded)
            .filter(([, f]) => f.state === "reading")
            .map(([key, f]): Effect => ({ type: "unfold", key, path: f.path }));
      const q = s.queried;
      if (s.hidden || s.rename || !q || q.text !== s.text || q.scope !== (s.scope?.name ?? null)) return done(s, again);
      if (s.text.trim() === "" && !s.scope) return done(s, again);
      const sel = shownRows(s)[s.selected];
      // While typed text is still waiting for its answer, the rows are for older text.
      const keep = sel && !s.awaiting ? keepOf(sel) : null;
      return done({ ...s, keep }, [{ type: "query", text: q.text, scope: q.scope, keepSelection: true }, ...again]);
    }
    case "unfolded": {
      // Only the answer for a folder still unfolded from that very path, and
      // still being read: a folded folder, changed text or a closed bar drop it,
      // and a late "reading" never undoes an answer.
      const f = s.unfolded[e.key];
      if (!f || f.path !== e.path || f.state !== "reading") return done(s);
      const next = { ...s, unfolded: { ...s.unfolded, [e.key]: { path: e.path, ...e.reply } } };
      // The rows below change: the selection stays on its row, a menu only on an unmoved row.
      const before = shownRows(s);
      const after = shownRows(next);
      const was = before[s.selected];
      const at = was ? after.findIndex((r) => r.key === was.key) : -1;
      const m = s.menu;
      const menu = m && before[m.index]?.key === after[m.index]?.key ? m : null;
      return done(fit({ ...next, selected: at >= 0 ? at : s.selected, menu }));
    }
    case "fold": {
      // The chevron: unfolds or folds that row, as Right or Left on it would.
      if (s.rename) return done(s);
      const row = shownRows(s)[e.index];
      if (!row || row.fold === "none" || foldBlocked(s)) return done(s);
      return side(fit({ ...s, selected: e.index }), row.fold === "folded");
    }
    case "tick": {
      // The clock's second: ask only for the live rows on screen, never the whole query.
      if (!wantsTick(s)) return done(s);
      const effects = visibleItems(s)
        .filter((i) => i.live)
        .map((i): Effect => ({ type: "liveRow", provider: i.provider, id: i.id }));
      return done(s, effects);
    }
    case "liveRowResult": {
      // A live row's fresh text replaces the old in place. A row no longer there,
      // one that is not live any more, a hidden bar or no change: the same state.
      const at = s.items.findIndex((i) => i.provider === e.item.provider && i.id === e.item.id);
      if (s.hidden || at < 0 || !e.item.live) return done(s);
      const old = s.items[at];
      if (old.title === e.item.title && old.subtitle === e.item.subtitle) return done(s);
      const items = s.items.slice();
      items[at] = { ...old, title: e.item.title, subtitle: e.item.subtitle };
      return done({ ...s, items });
    }
    case "updateChanged": {
      // The update row (found, downloading, failed) lives in the empty bar only.
      if (s.hidden || s.rename || s.text.trim() !== "" || s.scope) return done(s);
      const sel = shownRows(s)[s.selected];
      const keep = sel ? keepOf(sel) : null;
      return done({ ...s, keep }, [{ type: "query", text: "", scope: null, keepSelection: true }]);
    }
    case "menuClose":
      return done({ ...s, menu: null });
    case "hover":
      if (s.menu || s.rename || !shownRows(s)[e.index] || e.index === s.selected) return done(s);
      return done(fit({ ...s, selected: e.index, armed: null, browsed: true }));
    case "click": {
      if (s.rename) return done(s);
      if (s.menu) return done({ ...s, menu: null });
      if (!shownRows(s)[e.index]) return done(s);
      const sel = e.index === s.selected ? s : fit({ ...s, selected: e.index, armed: null });
      // A click is an explicit choice: it opens the clicked row now, never a
      // queued pick from results still on their way.
      const modifier: Modifier = e.ctrl && e.shift ? "ctrl_shift" : e.ctrl ? "ctrl" : "none";
      return enter({ ...sel, queued: null }, modifier);
    }
    case "menu": {
      const item = shownRows(s)[e.index];
      if (!item || s.rename) return done(s);
      const actions = menuActions(item);
      const sel = e.index === s.selected ? s : fit({ ...s, selected: e.index, armed: null });
      if (actions.length === 0) return done({ ...sel, menu: null });
      return done({ ...sel, menu: { index: e.index, actions, selected: 0, at: "pointer" } });
    }
    case "savePanel": {
      // Ctrl+S on the QR panel: its "Save as picture" without the menu.
      const item = panelItem(s);
      if (!item || s.rename || s.pending || !item.actions.includes("save_picture")) return done(s);
      return done({ ...s, menu: null, pending: true }, [{ type: "act", item, action: "save_picture" }]);
    }
    case "menuPick": {
      const m = s.menu;
      const row = m && shownRows(s)[m.index];
      const item = row && plain(row);
      // Only picking Uninstall or Delete keeps (or sets) the arm: any other pick must never fire it.
      const closed = { ...s, menu: null, ...(e.action === "uninstall" || e.action === "delete" ? {} : { armed: null, armedAction: null }) };
      if (!m || !item || !m.actions.includes(e.action)) return done(closed);
      switch (e.action) {
        case "pin":
        case "unpin":
          return done(closed, [{ type: "togglePin", item }]);
        case "admin":
          return enter({ ...closed, selected: m.index }, "ctrl_shift");
        case "uninstall":
          return done({ ...closed, selected: m.index, armed: item.id, armedAction: "uninstall" });
        case "delete":
          return done({ ...closed, selected: m.index, armed: item.id, armedAction: "delete" });
        case "rename": {
          if (s.pending) return done(closed);
          const before = { text: s.text, selected: m.index, top: s.top };
          const [start, end] = nameSelection(item.title, item.id.endsWith("\\"));
          return done({ ...closed, selected: m.index, rename: { item, before }, text: item.title, failure: null, error: s.configError }, [{ type: "selectName", start, end }]);
        }
        default:
          if (s.pending) return done(closed);
          return done({ ...closed, pending: true }, [{ type: "act", item, action: e.action }]);
      }
    }
    case "wheel": {
      // A row-anchored menu would be left in the wrong place, so it closes.
      const top = Math.min(Math.max(0, s.top + e.rows), Math.max(0, shownRows(s).length - capOf(s)));
      return top === s.top ? done({ ...s, menu: null }) : done({ ...s, top, menu: null });
    }
    case "configError":
      return done(fit({ ...s, configError: e.message, error: s.failure ?? e.message }));
    case "input": {
      // Rename mode: typing edits the name only, nothing is searched.
      if (s.rename) return done(fit({ ...s, text: e.text, failure: null, error: s.configError }));
      // Text added at the end while the selection is inside an unfolded folder:
      // the bar switches to that folder's path, followed by what was added.
      const row = shownRows(s)[s.selected];
      const f = row && row.parent !== null && s.scope === null ? s.unfolded[row.parent] : undefined;
      const text = f && !e.composing && e.text.length > s.text.length && e.text.startsWith(s.text) ? typedOf(f) + e.text.slice(s.text.length) : e.text;
      return done(
        fit({
          ...s,
          text,
          unfolded: {},
          recall: -1,
          browsed: false,
          kept: false,
          menu: null,
          selected: 0,
          top: 0,
          armed: null,
          failure: null,
          error: s.configError,
          awaiting: true,
          queued: null,
        }),
        [{ type: "query", text, scope: s.scope?.name ?? null }],
      );
    }
    case "results": {
      // Folds stay over results for the same text (the text changing folds
      // everything first), except those whose folder row is gone.
      const before = shownRows(s);
      const fresh = { ...s, items: e.items, unfolded: foldsShown(e.items, s.unfolded) };
      const rows = shownRows(fresh);
      if (s.keep && !s.queued) {
        // The answers' re-query: stay on the row the user was on, if it is still there,
        // and keep the window where it was.
        const k = s.keep;
        const kept = rowKey(k.chain ?? "", k.provider, k.id);
        const at = rows.findIndex((r) => r.key === kept);
        const armed = at >= 0 && s.armed === k.id ? s.armed : null;
        // A menu opened after the tick was sent stays when its row is still at that place.
        const was = s.menu ? before[s.menu.index] : undefined;
        const now = s.menu ? rows[s.menu.index] : undefined;
        const menu = s.menu && was && now && was.key === now.key ? s.menu : null;
        // A result that is gone: back to the first. A row inside a folder that
        // folded: the row that took its place.
        const selected = at >= 0 ? at : k.index ?? (k.chain !== undefined ? s.selected : 0);
        return done(fit({ ...fresh, selected, armed, awaiting: false, menu, keep: null }));
      }
      // The result at this index of `e.items`, counted in shown rows.
      const shownAt = (i: number): number => {
        let n = -1;
        return rows.findIndex((r) => r.depth === 0 && ++n === i);
      };
      let selected = 0;
      if (s.text === "" && !s.scope) selected = shownAt(emptyBarSelection(e.items));
      // Rows kept from the last show were on screen: a row the user moved to
      // or pressed Enter on stays selected if the fresh rows still have it.
      let gone = false;
      if (s.kept && (s.browsed || s.queued)) {
        const was = before[s.selected];
        const at = was ? rows.findIndex((r) => r.key === was.key) : -1;
        if (at >= 0) selected = at;
        else gone = true;
      }
      const next: State = fit({
        ...fresh,
        selected: Math.max(0, selected),
        top: 0,
        awaiting: false,
        queued: null,
        menu: null,
        keep: null,
        kept: false,
      });
      // An Enter pressed before these results arrived acts on them now, unless
      // it was pressed on a kept row that is no longer there.
      return s.queued && !gone ? enter(next, s.queued) : done(next);
    }
    case "past":
      return done({ ...s, past: e.items });
    case "pinned": {
      const mark = (i: Item): Item => (i.provider === e.provider && i.id === e.id ? { ...i, pinned: e.pinned } : i);
      const folds = Object.entries(s.unfolded);
      const next = {
        ...s,
        items: s.items.map(mark),
        // Rows inside unfolded folders are pinned too.
        unfolded: folds.length === 0 ? s.unfolded : Object.fromEntries(folds.map(([k, f]) => [k, { ...f, rows: f.rows.map(mark) }])),
      };
      // The empty bar is laid out by pins: ask for it again.
      const relayout = s.text === "" && !s.scope;
      return done(next, relayout ? [{ type: "query", text: "", scope: null }] : []);
    }
    case "failed":
      // A failed activation brings the bar back.
      return done(fit({ ...s, armed: null, pending: false, hidden: false, failure: e.message, error: e.message }));
    case "renamed": {
      // The new name took: leave rename mode and ask again for the text from
      // before it, staying on the renamed row (or where it was).
      const r = s.rename;
      if (!r) {
        // Cancelled while the rename was on its way: the list still needs the new name.
        return done({ ...s, pending: false, keep: { provider: "files", id: e.id, index: s.selected } }, [
          { type: "query", text: s.text, scope: s.scope?.name ?? null, keepSelection: true },
        ]);
      }
      const keep = { provider: r.item.provider, id: e.id, index: r.before.selected };
      const left = leaveRename(s);
      return done(fit({ ...left, pending: false, failure: null, error: s.configError, keep }), [
        { type: "query", text: left.text, scope: s.scope?.name ?? null, keepSelection: true },
      ]);
    }
    case "deleted": {
      // The file went to the Recycle Bin: ask again for the same text; the
      // selection stays at its index (the list's end clamps it).
      const keep = { provider: e.item.provider, id: e.item.id, index: s.selected };
      return done({ ...s, pending: false, armed: null, armedAction: null, menu: null, keep }, [
        { type: "query", text: s.text, scope: s.scope?.name ?? null, keepSelection: true },
      ]);
    }
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
          unfolded: {},
          selected: 0,
          top: 0,
          armed: null,
          pending: false,
          awaiting: true,
          queued: null,
          kept: false,
          browsed: false,
        },
        [{ type: "query", text: "", scope }],
      );
    }
    case "options":
      return done({ ...s, tabCompletion: e.tabCompletion });
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
    case "Tab":
      return done({ ...s, menu: null });
    default:
      return done(s);
  }
}

// Rename mode owns Enter and Esc; the list keys do nothing, the rest is the text field's.
function onRenameKey(s: State, r: NonNullable<State["rename"]>, e: Extract<Event, { type: "key" }>): Result {
  if (e.key === "Enter") {
    if (s.pending) return done(s);
    return done({ ...s, pending: true, failure: null, error: s.configError }, [{ type: "rename", item: r.item, name: s.text }]);
  }
  if (e.key === "Escape") return done(fit({ ...leaveRename(s), pending: false, failure: null, error: s.configError }));
  return done(s);
}

function onKey(s: State, e: Extract<Event, { type: "key" }>): Result {
  if (s.rename) return onRenameKey(s, s.rename, e);
  if (s.menu) return onMenuKey(s, s.menu, e);
  switch (e.key) {
    case "ArrowUp":
    case "ArrowDown": {
      // Up on the empty bar recalls past searches, but only as the first move:
      // once the rows were moved through (arrows or mouse), the arrows stay in
      // the rows and wrap at the ends, so going back up past a pin never
      // drops into old searches. Typing, or opening the bar again, starts over.
      const n = shownRows(s).length;
      const inRows = s.recall < 0 && s.browsed && n > 0;
      if (!inRows && s.past.length > 0 && !s.scope && (s.text === "" || s.recall >= 0) && (e.key === "ArrowUp" || s.recall >= 0)) {
        return recallStep(s, e.key === "ArrowUp" ? 1 : -1);
      }
      if (n === 0) return done({ ...s, armed: null });
      // Tab completion: the first arrow only brings the highlight out. Down
      // shows it on the row Enter would open; Up on the last row.
      if (!highlighted(s)) {
        return done(fit({ ...s, selected: e.key === "ArrowDown" ? s.selected : n - 1, browsed: true }));
      }
      const step = e.key === "ArrowDown" ? 1 : n - 1;
      return done(fit({ ...s, selected: (s.selected + step) % n, armed: null, browsed: true }));
    }
    case "ArrowLeft":
    case "ArrowRight":
      if (e.ctrl || e.shift || foldBlocked(s)) return done(s);
      return side(s, e.key === "ArrowRight");
    case "p": {
      const row = shownRows(s)[s.selected];
      if (!e.ctrl || !row || !row.pinnable) return done(s);
      return done(s, [{ type: "togglePin", item: plain(row) }]);
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
    case "Tab": {
      // Tab completion: take the grey preview. Like F1, not an activation
      // finishing: `pending` stays.
      const whole = completion(s);
      if (whole !== null) {
        const r = fill(s, whole);
        return done({ ...r.state, pending: s.pending }, r.effects);
      }
      const item = shownRows(s)[s.selected];
      if (s.awaiting || !item) return done(s);
      if (item.complete !== "") {
        // Like F1, not an activation finishing: `pending` stays.
        const r = fill(s, item.complete);
        return done({ ...r.state, pending: s.pending }, r.effects);
      }
      // Path browsing's own rows: no menu. Rows inside an unfolded folder have one.
      if (item.depth === 0 && s.items.some((i) => i.provider === "path")) return done(s);
      const actions = menuActions(item);
      if (actions.length === 0) return done(s);
      return done({ ...s, menu: { index: s.selected, actions, selected: 0, at: "row" } });
    }
    case "Escape": {
      const t = { ...s, queued: null };
      if (t.armed) return done({ ...t, armed: null });
      if (t.scope) {
        return done(
          { ...t, scope: null, text: "", recall: -1, items: [], unfolded: {}, selected: 0, top: 0, awaiting: true, browsed: false },
          [{ type: "query", text: "", scope: null }],
        );
      }
      return done({ ...t, unfolded: {} }, [{ type: "hide" }]);
    }
  }
}

// Right and Left (and the chevron) are ignored while a menu is open, a row is
// armed, older results are awaited, an activation is pending, or a panel shows.
const foldBlocked = (s: State): boolean => s.rename !== null || s.menu !== null || s.armed !== null || s.awaiting || s.pending || panelItem(s) !== null;

// True exactly when Right or Left would change something; otherwise the key is
// left to the text field (the caret moves).
export function sideActs(s: State, right: boolean): boolean {
  if (foldBlocked(s)) return false;
  const row = shownRows(s)[s.selected];
  if (!row) return false;
  if (right) return row.fold !== "none" || (row.depth > 0 && row.provider === "fold" && row.id === "more");
  return row.fold === "unfolded" || row.parent !== null;
}

// Right unfolds a folded folder (the selection stays), steps into an unfolded
// one, or opens "N more…". Left folds an unfolded folder with everything
// unfolded under it, or goes from a row inside to its folder's row.
function side(s: State, right: boolean): Result {
  const rows = shownRows(s);
  const row = rows[s.selected];
  if (!row) return done(s);
  if (right) {
    if (row.fold === "folded") {
      const reading: Fold = { path: row.folder, state: "reading", rows: [], more: 0, typed: "" };
      return done({ ...s, unfolded: { ...s.unfolded, [row.key]: reading }, browsed: true }, [{ type: "unfold", key: row.key, path: row.folder }]);
    }
    if (row.fold === "unfolded") return done(fit({ ...s, selected: s.selected + 1, browsed: true }));
    if (row.depth > 0 && row.provider === "fold" && row.id === "more") return enter(s, "none");
    return done(s);
  }
  if (row.fold === "unfolded") {
    const under = row.key + SEP;
    const unfolded = Object.fromEntries(Object.entries(s.unfolded).filter(([k]) => k !== row.key && !k.startsWith(under)));
    return done(fit({ ...s, unfolded, browsed: true }));
  }
  if (row.parent !== null) return done(fit({ ...s, selected: rows.findIndex((r) => r.key === row.parent), browsed: true }));
  return done(s);
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
      unfolded: {},
      selected: 0,
      top: 0,
      armed: null,
      failure: null,
      error: s.configError,
      awaiting: true,
      queued: null,
      kept: false,
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
    unfolded: {},
    selected: 0,
    top: 0,
    armed: null,
    pending: false,
    queued: null,
    failure: null,
    error: s.configError,
    awaiting: true,
    kept: false,
    browsed: false,
  };
  return done(next, [{ type: "query", text, scope: s.scope?.name ?? null }]);
}

// Enter on the selected item: a confirm item arms first, anything else activates.
function enter(s: State, modifier: Modifier): Result {
  const row = shownRows(s)[s.selected];
  if (!row || s.pending) return done(s);
  if (row.depth > 0 && row.provider === "fold") {
    // The page's own rows: "N more…" puts the folder's path in the bar, the others do nothing.
    const f = row.parent !== null ? s.unfolded[row.parent] : undefined;
    return row.id === "more" && f ? fill(s, typedOf(f)) : done(s);
  }
  const item = plain(row);
  if (s.armed === item.id && (s.armedAction === "uninstall" || s.armedAction === "delete")) {
    return done({ ...s, armed: null, armedAction: null, pending: true }, [{ type: "act", item, action: s.armedAction }]);
  }
  if (item.confirm && s.armed !== item.id) return done({ ...s, armed: item.id });
  return done({ ...s, armed: null, pending: true }, [{ type: "activate", item, modifier, text: s.text, scope: s.scope?.name ?? null }]);
}

/** The arguments of the back end's `query` command: the page's own number for
 *  this search rides along, so only a higher number can cancel an older one. */
export function queryArgs(effect: { text: string; scope: string | null }, seq: number) {
  return { text: effect.text, scope: effect.scope, seq };
}

/** The arguments of the one retry: no number (sequence 0), which the back end
 *  never cancels, so the retry cannot be answered with null and cannot loop. */
export function retryArgs(effect: { text: string; scope: string | null }) {
  return queryArgs(effect, 0);
}

/** What to do with the answer to the CURRENT search. The back end answers null
 *  only for a search a higher number overtook, which cannot be the current one;
 *  should it happen anyway, ask again once, unnumbered (see `retryArgs`), rather
 *  than leave `awaiting` set for ever. */
export function queryReply(items: Item[] | null): { type: "results"; items: Item[] } | "ask again" {
  return items ? { type: "results", items } : "ask again";
}
