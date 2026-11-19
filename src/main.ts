/// <reference types="vite/client" />
import "./styles.css";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { DARK, DEFAULT_APPEARANCE, GLASS_DARK_INK, applyAppearance, svgDataUrl, themedSvg, type Appearance } from "./appearance";
import { inkIsDark, installLens } from "./glass";
import { HIGHLIGHT, LIST, pressed, settled, stepSpring, stretch, type Spring } from "./spring";
import { builtinName, fallbackName, pickIconSrc, serverUrl } from "./icons";
import {
  capOf, failureText, initial, movedEnough, reduce, rowsFromWheel, completion, highlighted, visibleItems,
  panelItem, queryArgs, queryReply, wantsTick, type Effect, type Event, type Item, type MenuAction, type Point, type State,
} from "./bar";

type Event_ = MouseEvent;
type Outcome = Extract<Event, { type: "outcome" }>["outcome"];

const panel = document.getElementById("app") as HTMLElement;
panel.className = "panel";

const row = document.createElement("div");
row.className = "input-row";
const pill = document.createElement("span");
pill.className = "pill";
const input = document.createElement("input");
input.type = "text";
input.spellcheck = false;
input.autocomplete = "off";
input.autofocus = true;
input.setAttribute("aria-label", "Search");
// Tab completion's grey preview, laid over the input in the same type: the
// typed text (invisible, to push the rest to the right place) and the part
// Tab would add.
const inputWrap = document.createElement("div");
inputWrap.className = "input-wrap";
const ghost = document.createElement("div");
ghost.className = "ghost";
ghost.setAttribute("aria-hidden", "true");
const ghostTyped = document.createElement("span");
ghostTyped.className = "ghost-typed";
const ghostRest = document.createElement("span");
ghost.append(ghostTyped, ghostRest);
inputWrap.append(input, ghost);
row.append(pill, inputWrap);

const errorLine = document.createElement("div");
errorLine.className = "error";
const list = document.createElement("ul");
list.className = "results";
const listWrap = document.createElement("div");
listWrap.className = "list-wrap";
const track = document.createElement("div");
track.className = "scroll-track";
const thumb = document.createElement("div");
thumb.className = "scroll-thumb";
track.append(thumb);
// The moving highlight of the medium and high animation levels; under
// Minimal it is hidden and each selected row paints its own background.
const marker = document.createElement("div");
marker.className = "sel-marker";
listWrap.append(marker, list, track);
const menuEl = document.createElement("div");
menuEl.className = "menu";
menuEl.setAttribute("role", "menu");
menuEl.hidden = true;
const qrHolder = document.createElement("div");
qrHolder.className = "qr-svg";
const qrCaption = document.createElement("div");
qrCaption.className = "qr-caption";
const qrPanel = document.createElement("div");
qrPanel.className = "qr-panel";
qrPanel.hidden = true;
qrPanel.append(qrHolder, qrCaption);
// The Glass theme's layers, under everything else in the panel and shown
// only by that theme: what is behind the bar (bent near the rim), and a tint
// with the lit edge that keeps the text readable.
const glassLayers = ["glass-back", "glass-tint"].map((name) => {
  const layer = document.createElement("div");
  layer.className = name;
  layer.setAttribute("aria-hidden", "true");
  return layer;
});
glassLayers[0].append(document.createElement("i"));
panel.append(...glassLayers, row, errorLine, listWrap, qrPanel, menuEl);
const updateLens = installLens(panel, glassLayers[0], "flick-lens");

// Bundled SVGs by name (src/icons/<name>.svg), as text: they are repainted in
// the theme's text colour and shown as data URLs, made once per theme.
const svgs = import.meta.glob<string>("./icons/*.svg", { eager: true, query: "?raw", import: "default" });
let iconInk = DARK.text;
const svgUrls = new Map<string, string>();
const svgUrl = (name: string): string => {
  let url = svgUrls.get(name);
  if (url === undefined) {
    const raw = svgs[`./icons/${name}.svg`] ?? svgs["./icons/file.svg"];
    url = svgDataUrl(themedSvg(raw, iconInk));
    svgUrls.set(name, url);
  }
  return url;
};

// Opt-in timing (the back end's FLICK_TIMING): asked once; off means no calls at all.
let timingOn = false;
invoke<boolean>("timing_enabled").then((on) => (timingOn = on), () => {});
/// Reports `name` with the time since `since` (a `performance.now()` value) once the next frame comes.
const reportAtFrame = (name: "page.shown_to_paint" | "page.query_to_paint" | "page.tick", since: number) => {
  if (!timingOn) return;
  requestAnimationFrame(() => {
    invoke("timing_mark", { name, micros: Math.round((performance.now() - since) * 1000) }).catch(() => {});
  });
};

let state: State = initial;
// Rises with every search and starts from the clock, so a reloaded page never
// restarts below the numbers the back end has already seen.
let querySeq = Date.now();
/// Bumped on every `bar:shown`: an activation answered after the bar was
/// shown again belongs to an old session and is dropped.
let shows = 0;

// A fixed 24x24 box, so a late image never moves the row. Server icons fade in
// once loaded; a 404 swaps in the bundled SVG for the kind of thing.
const loadedIcons = new Set<string>();
const failedIcons = new Set<string>();

function renderIcon(item: Item): HTMLSpanElement {
  const box = document.createElement("span");
  box.className = "icon";
  const img = document.createElement("img");
  img.alt = "";
  img.draggable = false;
  const name = builtinName(item.icon);
  const url = serverUrl(item.icon, window.devicePixelRatio || 1);
  if (name !== null) {
    img.src = svgUrl(name);
    img.className = "loaded";
  } else {
    const fallback = svgUrl(fallbackName(item));
    const pick = pickIconSrc(url, loadedIcons, failedIcons, fallback);
    if (pick.src === url) {
      img.onload = () => {
        loadedIcons.add(url);
        img.classList.add("loaded");
      };
      img.onerror = () => {
        img.onerror = null;
        img.onload = () => img.classList.add("loaded");
        failedIcons.add(url);
        img.src = fallback;
      };
    } else {
      img.onload = () => img.classList.add("loaded");
    }
    img.src = pick.src;
    if (pick.visible) img.classList.add("loaded");
  }
  box.append(img);
  return box;
}

// ---- Wobble: the highlight and the list's height as weights on springs ----
// Under the Wobble level these two are simulated (spring.ts) instead of
// eased: each move starts from the current speed, so quick moves carry their
// momentum, and the highlight stretches with its speed. Every other level
// leaves both to CSS.
const wobbling = (): boolean =>
  document.documentElement.dataset.motion === "wobble" && !matchMedia("(prefers-reduced-motion: reduce)").matches;
const ROW = 44;
// The list's border and padding around its rows.
const LIST_EDGES = 9;
let highlight: Spring = { x: 5, v: 0 };
let highlightTo = 5;
let listTall: Spring = { x: 0, v: 0 };
let listTallTo = 0;
// The position of the last row, where the highlight is squashed instead of passing.
let lastRowAt = 5;
let frame = 0;
let lastFrame = 0;
// The next move is a jump, not a glide: the bar has just opened, or the list has just appeared.
let jumpNext = true;

function paintSprings() {
  // Past the first or last row the highlight is pressed against that edge
  // and squashed; in between it stretches with its speed.
  const p = pressed(highlight.x, 5, lastRowAt, ROW);
  marker.style.translate = `0 ${p.at.toFixed(2)}px`;
  if (p.edge !== 0) {
    marker.style.transformOrigin = p.edge < 0 ? "50% 0" : "50% 100%";
    marker.style.scale = `${(1 + (1 - p.tall) * 0.12).toFixed(4)} ${p.tall.toFixed(4)}`;
  } else {
    const s = stretch(highlight.v);
    marker.style.transformOrigin = "";
    marker.style.scale = s === 0 ? "" : `${(1 - s * 0.25).toFixed(4)} ${(1 + s).toFixed(4)}`;
  }
  list.style.height = `${Math.max(0, listTall.x).toFixed(2)}px`;
}

function onFrame(now: number) {
  const seconds = (now - lastFrame) / 1000;
  lastFrame = now;
  highlight = stepSpring(highlight, highlightTo, seconds, HIGHLIGHT);
  listTall = stepSpring(listTall, listTallTo, seconds, LIST);
  if (settled(highlight, highlightTo) && settled(listTall, listTallTo)) {
    highlight = { x: highlightTo, v: 0 };
    listTall = { x: listTallTo, v: 0 };
    frame = 0;
  } else {
    frame = requestAnimationFrame(onFrame);
  }
  paintSprings();
}

// Sends the highlight to `y` and the list to the height of `rows` rows.
function moveTo(y: number, rows: number) {
  if (!wobbling()) {
    if (frame) cancelAnimationFrame(frame);
    frame = 0;
    marker.style.translate = `0 ${y}px`;
    marker.style.scale = "";
    marker.style.transformOrigin = "";
    list.style.height = "";
    highlight = { x: y, v: 0 };
    listTall = { x: rows * ROW + LIST_EDGES, v: 0 };
    return;
  }
  const tall = rows === 0 ? 0 : rows * ROW + LIST_EDGES;
  lastRowAt = 5 + Math.max(0, rows - 1) * ROW;
  // A list that was empty has no place for the highlight to come from.
  if (jumpNext || listTallTo === 0) highlight = { x: y, v: 0 };
  if (jumpNext) listTall = { x: tall, v: 0 };
  jumpNext = false;
  highlightTo = y;
  listTallTo = tall;
  if (!frame && !(settled(highlight, y) && settled(listTall, tall))) {
    lastFrame = performance.now();
    frame = requestAnimationFrame(onFrame);
  }
  paintSprings();
}

// Rows on screen at the last render, so only rows that are new fade in.
let shownRows = new Set<string>();
const rowKey = (item: Item): string => `${item.provider}\u0000${item.id}`;

function renderItem(item: Item, index: number, fresh: boolean): HTMLLIElement {
  const li = document.createElement("li");
  li.className = "result";
  li.dataset.index = String(index);
  li.dataset.key = rowKey(item);
  if (fresh) {
    li.classList.add("fresh");
    li.style.setProperty("--at", String(index - state.top));
  }
  if (index === state.selected && highlighted(state)) li.classList.add("selected");
  const armed = state.armed === item.id;
  if (armed) li.classList.add("armed");
  const title = document.createElement("span");
  title.className = "title";
  title.textContent = item.title;
  const sub = document.createElement("span");
  sub.className = "subtitle";
  sub.textContent = !armed
    ? item.subtitle
    : state.armedAction === "uninstall"
      ? `Uninstall ${item.title}? Enter again`
      : "Press Enter again to confirm";
  li.append(renderIcon(item), title, sub);
  if (item.pinned) {
    const star = document.createElement("img");
    star.className = "star";
    star.alt = "Pinned";
    star.src = svgUrl("star");
    li.append(star);
  }
  return li;
}

const MENU_LABELS: Record<MenuAction, string> = {
  pin: "Pin",
  unpin: "Unpin",
  reveal: "Open file location",
  open_folder: "Open in Explorer",
  copy_path: "Copy path",
  admin: "Run as administrator",
  uninstall: "Uninstall",
  close: "Close",
  close_window: "Close window",
  copy: "Copy",
  save_picture: "Save as picture",
};
/// Where the right-click happened, in window coordinates.
let menuAt: Point = { x: 0, y: 0 };

function renderMenu() {
  const m = state.menu;
  menuEl.hidden = !m;
  if (!m) {
    menuEl.replaceChildren();
    return;
  }
  menuEl.replaceChildren(
    ...m.actions.map((action, i) => {
      const entry = document.createElement("div");
      entry.className = "menu-item";
      entry.setAttribute("role", "menuitem");
      if (i === m.selected) entry.classList.add("selected");
      entry.dataset.action = action;
      entry.textContent = MENU_LABELS[action];
      return entry;
    }),
  );
  let at = menuAt;
  if (m.at === "row") {
    // Under the selected row, past its icon.
    const li = list.querySelector<HTMLElement>("li.result.selected");
    const icon = li?.querySelector<HTMLElement>(".icon");
    if (panelItem(state)) {
      // The QR panel stands in for its row (which is rendered but hidden, so
      // it has no box): the menu opens in the panel's top-left corner, clear
      // of the input.
      const box = qrPanel.getBoundingClientRect();
      at = { x: box.left + 12, y: box.top + 12 };
    } else if (li) {
      const box = li.getBoundingClientRect();
      at = { x: box.left + (icon?.offsetWidth ?? 0), y: box.bottom };
    }
  }
  // Flip up / shift left when the menu would run past the window edge.
  const w = menuEl.offsetWidth;
  const h = menuEl.offsetHeight;
  const left = at.x + w > window.innerWidth ? at.x - w : at.x;
  const top = at.y + h > window.innerHeight ? at.y - h : at.y;
  menuEl.style.left = `${Math.max(0, left)}px`;
  menuEl.style.top = `${Math.max(0, top)}px`;
}

// Only an <svg> built by Flick's backend is injected: anything else is dropped.
let qrMarkup = "";
function renderQr(item: Item) {
  if (item.panel !== qrMarkup) {
    qrMarkup = item.panel;
    const doc = new DOMParser().parseFromString(item.panel, "image/svg+xml");
    const svg = doc.documentElement;
    const safe =
      svg.nodeName === "svg" &&
      !doc.querySelector("parsererror") &&
      !svg.querySelector("script, foreignObject, image, a, style, use, iframe") &&
      ![svg, ...svg.querySelectorAll("*")].some((el) => [...el.attributes].some((a) => /^on/i.test(a.name) || /^(javascript|data):/i.test(a.value.trim())));
    if (safe) qrHolder.replaceChildren(document.importNode(svg, true));
    else qrHolder.replaceChildren();
  }
  qrCaption.textContent = item.title;
}

// Live rows (the clock) ask again at the start of each wall-clock second. One
// timer at most; it clears itself once no live row shows.
let tickTimer: number | null = null;

function scheduleTick() {
  if (!wantsTick(state)) {
    if (tickTimer !== null) clearTimeout(tickTimer);
    tickTimer = null;
    return;
  }
  if (tickTimer !== null) return;
  tickTimer = window.setTimeout(() => {
    tickTimer = null;
    dispatch({ type: "tick" });
  }, 1000 - (Date.now() % 1000) + 5);
}

function render() {
  if (input.value !== state.text) {
    input.value = state.text;
    input.setSelectionRange(state.text.length, state.text.length);
  }
  pill.hidden = !state.scope;
  pill.textContent = state.scope?.prompt ?? "";
  input.placeholder = state.scope ? "" : "Search apps and settings";
  errorLine.hidden = !state.error;
  errorLine.textContent = state.error ?? "";
  const rows = visibleItems(state);
  const qr = panelItem(state);
  qrPanel.hidden = !qr;
  if (qr) renderQr(qr);
  listWrap.hidden = rows.length === 0 || !!qr;
  list.replaceChildren(...rows.map((item, i) => renderItem(item, state.top + i, !shownRows.has(rowKey(item)))));
  shownRows = new Set(rows.map(rowKey));
  list.style.setProperty("--rows", String(rows.length));
  // 5px: the list's top border and padding. Out of view: parked on the nearest edge.
  const at = Math.max(0, Math.min(state.selected - state.top, Math.max(0, rows.length - 1)));
  // Coming out of hiding (tab completion: the first arrow), the highlight
  // appears on its row instead of gliding there from where it last was.
  const markerHidden = rows.length === 0 || !highlighted(state);
  if (marker.hidden && !markerHidden) jumpNext = true;
  marker.hidden = markerHidden;
  moveTo(5 + at * 44, rows.length);
  // The grey preview, unless the text is too long to line it up with.
  const whole = completion(state);
  const fits = input.scrollWidth <= input.clientWidth;
  ghost.hidden = whole === null || !fits;
  ghostTyped.textContent = whole === null ? "" : state.text;
  ghostRest.textContent = whole === null ? "" : whole.slice(state.text.length);
  const cap = capOf(state);
  const len = state.items.length;
  track.hidden = len <= cap;
  if (len > cap) {
    thumb.style.height = `${(cap / len) * 100}%`;
    thumb.style.top = `${(state.top / len) * 100}%`;
  }
  renderMenu();
  scheduleTick();
  // Invisible while hidden (medium and high levels), so the next open starts
  // from nothing; anything that brings the bar back clears it.
  panel.classList.toggle("closed", state.hidden);
}

/// A live row's new text, written into its element: the rows, the marker, the
/// icons and the animations stay as they are. False when the row's element is
/// not on screen (the caller then renders normally).
function patchLiveRow(item: Item): boolean {
  const key = rowKey(item);
  const li = Array.from(list.children).find((c): c is HTMLElement => c instanceof HTMLElement && c.dataset.key === key);
  const shown = state.items.find((i) => rowKey(i) === key);
  const title = li?.querySelector(".title");
  const sub = li?.querySelector(".subtitle");
  if (!shown || !title || !sub) return false;
  title.textContent = shown.title;
  if (state.armed !== shown.id) sub.textContent = shown.subtitle;
  return true;
}

function dispatch(event: Event) {
  const { state: next, effects } = reduce(state, event);
  const before = state;
  state = next;
  if (next !== before) {
    if (event.type === "liveRowResult" && patchLiveRow(event.item)) scheduleTick();
    else render();
  } else scheduleTick();
  effects.forEach(run);
}

/// Rows with a `liveRow` request on its way: a slow reply is never overtaken by another ask.
const liveInFlight = new Set<string>();

function run(effect: Effect) {
  switch (effect.type) {
    case "query": {
      const seq = ++querySeq;
      const sent = performance.now();
      invoke<Item[] | null>("query", queryArgs(effect, seq))
        .then((items) => {
          if (seq !== querySeq) return;
          const reply = queryReply(items);
          if (reply === "ask again") return run(effect);
          dispatch(reply);
          reportAtFrame("page.query_to_paint", sent);
        })
        .catch((err) => {
          if (seq === querySeq) dispatch({ type: "queryFailed", message: String(err) });
        });
      break;
    }
    case "liveRow": {
      const key = `${effect.provider}\u0000${effect.id}`;
      if (liveInFlight.has(key)) break;
      liveInFlight.add(key);
      const at = shows;
      const sent = performance.now();
      invoke<Item | null>("live_row", { provider: effect.provider, id: effect.id })
        .then((item) => {
          liveInFlight.delete(key);
          if (at !== shows || !item) return;
          dispatch({ type: "liveRowResult", item });
          reportAtFrame("page.tick", sent);
        })
        .catch(() => liveInFlight.delete(key));
      break;
    }
    case "activate": {
      const at = shows;
      invoke<Outcome>("activate", {
        provider: effect.item.provider,
        id: effect.item.id,
        modifier: effect.modifier,
        text: effect.text,
        scope: effect.scope,
      })
        .then((outcome) => {
          if (at === shows) dispatch({ type: "outcome", outcome });
        })
        .catch((err) => {
          if (at === shows) dispatch({ type: "failed", message: failureText(effect.item, err) });
        });
      break;
    }
    case "loadPast":
      invoke<string[]>("past_searches")
        .then((items) => dispatch({ type: "past", items }))
        .catch(() => {});
      break;
    case "togglePin":
      invoke<boolean>("toggle_pin", { provider: effect.item.provider, id: effect.item.id })
        .then((pinned) => dispatch({ type: "pinned", provider: effect.item.provider, id: effect.item.id, pinned }))
        .catch((err) => dispatch({ type: "failed", message: failureText(effect.item, err) }));
      break;
    case "act": {
      const at = shows;
      invoke<Outcome>("act", { provider: effect.item.provider, id: effect.item.id, action: effect.action })
        .then((outcome) => {
          if (at === shows) dispatch({ type: "outcome", outcome });
        })
        .catch((err) => {
          if (at === shows) dispatch({ type: "failed", message: failureText(effect.item, err) });
        });
      break;
    }
    case "hide":
      invoke("hide").catch(() => {});
      break;
  }
}

input.addEventListener("input", () => dispatch({ type: "input", text: input.value }));

const KEYS = ["ArrowUp", "ArrowDown", "Enter", "Escape", "F1", "p", "Tab"] as const;
// Browser defaults that make no sense in a launcher: focus change, help, reload, print, find.
const SWALLOWED = ["Tab", "F1", "F5"];
const SWALLOWED_CTRL = ["p", "f", "r"];
document.addEventListener("keydown", (e) => {
  if (SWALLOWED.includes(e.key) || (e.ctrlKey && SWALLOWED_CTRL.includes(e.key.toLowerCase()))) e.preventDefault();
  if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "s" && !e.repeat) {
    e.preventDefault();
    dispatch({ type: "savePanel" });
  }
});
const rowIndex = (e: Event_): number | null => {
  const li = (e.target as Element | null)?.closest<HTMLElement>("li.result");
  return li?.dataset.index === undefined ? null : Number(li.dataset.index);
};
document.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  if ((e.target as Element | null)?.closest(".qr-panel") && panelItem(state)) {
    menuAt = { x: e.clientX, y: e.clientY };
    dispatch({ type: "menu", index: 0 });
    return;
  }
  const index = rowIndex(e);
  if (index === null) {
    if (state.menu) dispatch({ type: "menuClose" });
    return;
  }
  menuAt = { x: e.clientX, y: e.clientY };
  dispatch({ type: "menu", index });
});
window.addEventListener("blur", () => dispatch({ type: "blurred" }));
// The window got the keyboard (back): typing goes to the input again.
window.addEventListener("focus", () => input.focus());

input.addEventListener("keydown", (e) => {
  const key = KEYS.find((k) => k === (e.key.length === 1 ? e.key.toLowerCase() : e.key));
  if (!key || e.isComposing) return;
  if (key === "p" && !e.ctrlKey) return; // plain p is text
  e.preventDefault();
  // Held Enter/Escape must not repeat: it would skip the two-step confirm.
  if (e.repeat && (key === "Enter" || key === "Escape" || key === "F1" || key === "p" || key === "Tab")) return;
  dispatch({ type: "key", key, ctrl: e.ctrlKey, shift: e.shiftKey });
});

// The wheel scrolls the list by whole rows; leftover pixels carry to the next tick.
let wheelAcc = 0;
listWrap.addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    const r = rowsFromWheel(wheelAcc, e.deltaY);
    wheelAcc = r.acc;
    if (r.rows !== 0) dispatch({ type: "wheel", rows: r.rows });
  },
  { passive: false },
);

// Hover selects only when the pointer really moved: rows replaced under a still
// pointer fire mouse events too, and must not steal the keyboard selection.
let lastPointer: Point | null = null;
list.addEventListener("mousemove", (e) => {
  const at = { x: e.clientX, y: e.clientY };
  if (!movedEnough(lastPointer, at)) return;
  lastPointer = at;
  const index = rowIndex(e);
  // Dispatching re-renders the rows, which would swallow a click in progress.
  if (index !== null && index !== state.selected && !state.menu) dispatch({ type: "hover", index });
});
document.addEventListener("click", (e) => {
  if (e.button !== 0) return;
  const target = e.target as Element | null;
  // The window is taller than the bar, to leave room for the rows; a click
  // on that empty part is a click beside the bar, and closes it like one.
  if (!target || !panel.contains(target)) {
    if (state.menu) dispatch({ type: "menuClose" });
    else invoke("hide").catch(() => {});
    return;
  }
  const entry = target?.closest<HTMLElement>(".menu-item");
  if (entry) {
    dispatch({ type: "menuPick", action: entry.dataset.action as MenuAction });
    return;
  }
  if (target?.closest(".qr-panel") && panelItem(state)) {
    dispatch({ type: "click", index: 0, ctrl: e.ctrlKey, shift: e.shiftKey });
    return;
  }
  const index = rowIndex(e);
  if (index !== null) dispatch({ type: "click", index, ctrl: e.ctrlKey, shift: e.shiftKey });
  else if (state.menu) dispatch({ type: "menuClose" });
});

document.addEventListener("mousedown", (e) => {
  e.preventDefault(); // keep focus in the input
  input.focus();
});

const listenFailed = (err: unknown) => dispatch({ type: "failed", message: `Event listener failed: ${err}` });

const afterShow = (event: Event) => {
  const handled = performance.now();
  shows++; // drop activation results from before this show
  querySeq++; // drop answers to queries from before this show
  jumpNext = true; // nothing glides in from where it was when the bar closed
  dispatch(event);
  reportAtFrame("page.shown_to_paint", handled);
  input.focus();
  // The opening animation of the medium and high levels, played from its
  // start on every show (a no-op under Minimal).
  panel.classList.remove("opening");
  void panel.offsetWidth;
  panel.classList.add("opening");
};
const shownListener = listen("bar:shown", () => afterShow({ type: "shown" }));
// A direct hotkey's item needs a second Enter: the bar opens with it armed.
listen<Item>("bar:armed", (e) => afterShow({ type: "armed", item: e.payload })).catch(listenFailed);
// A workspace run had failures: the bar opens with the text and the error.
listen<{ text: string; error: string }>("bar:reopen", (e) => afterShow({ type: "reopen", text: e.payload.text, error: e.payload.error })).catch(listenFailed);
// A direct hotkey's item asks to choose: the bar opens in its pick list.
listen<{ prompt: string; scope: string }>("bar:step", (e) => afterShow({ type: "scoped", prompt: e.payload.prompt, scope: e.payload.scope })).catch(listenFailed);
shownListener.catch(listenFailed);
// A rate or forecast arrived: the bar asks again if the text is still the same.
listen("answers:ready", () => dispatch({ type: "answersReady" })).catch(listenFailed);
listen("update:changed", () => dispatch({ type: "updateChanged" })).catch(listenFailed);
// The back end hid the bar (Esc, focus loss, an activation): no live updates until it is shown again.
listen("bar:hidden", () => dispatch({ type: "hidden" })).catch(listenFailed);
listen<string | null>("config:error", (e) => dispatch({ type: "configError", message: e.payload })).catch(listenFailed);

// The theme and animation level: asked once, then pushed on every change.
let appearance: Appearance = DEFAULT_APPEARANCE;
// Glass over a light picture writes in dark ink.
let darkInk = false;
const paint = () => {
  const colors = appearance.glass && darkInk ? GLASS_DARK_INK : appearance.colors;
  applyAppearance(appearance, colors, document.documentElement);
  if (colors.text !== iconInk) {
    iconInk = colors.text;
    svgUrls.clear();
  }
  updateLens();
  render();
};
const themed = (a: Appearance) => {
  appearance = a;
  dispatch({ type: "options", tabCompletion: a.tab_completion });
  paint();
};
listen<Appearance>("appearance:changed", (e) => themed(e.payload)).catch(listenFailed);
invoke<Appearance>("appearance").then(themed, () => {});

// Glass only: the back end takes a picture of what is behind the bar just
// before each show and says so here; the glass layers draw it. It is let go
// when the bar hides.
const setBackdrop = (id: number) => {
  const url = id > 0 ? `url("http://flick-backdrop.localhost/${id}")` : "none";
  document.documentElement.style.setProperty("--backdrop", url);
};
listen<{ id: number; brightness: number }>("bar:backdrop", (e) => {
  setBackdrop(e.payload.id);
  const dark = e.payload.id > 0 && inkIsDark(e.payload.brightness);
  if (dark !== darkInk) {
    darkInk = dark;
    paint();
  }
}).catch(listenFailed);
listen("bar:hidden", () => setBackdrop(0)).catch(listenFailed);

render();
input.focus();

// The first show (first start) can fire before the listener above is in place,
// so its `bar:shown` is lost. Ask for the empty-bar rows once the listener is
// registered. A typed query bumps `querySeq` and so wins over a late answer.
void shownListener.then(() => run({ type: "query", text: "", scope: null }), () => {});
