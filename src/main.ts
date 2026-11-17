/// <reference types="vite/client" />
import "./styles.css";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { builtinName, fallbackName, pickIconSrc, serverUrl } from "./icons";
import {
  capOf, failureText, initial, movedEnough, reduce, rowsFromWheel, visibleItems,
  type Effect, type Event, type Item, type MenuAction, type Point, type State,
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
row.append(pill, input);

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
listWrap.append(list, track);
const menuEl = document.createElement("div");
menuEl.className = "menu";
menuEl.setAttribute("role", "menu");
menuEl.hidden = true;
panel.append(row, errorLine, listWrap, menuEl);

// Bundled SVGs by name (src/icons/<name>.svg), resolved to URLs by Vite.
const svgs = import.meta.glob<string>("./icons/*.svg", { eager: true, query: "?url", import: "default" });
const svgUrl = (name: string): string => svgs[`./icons/${name}.svg`] ?? svgs["./icons/file.svg"];

let state: State = initial;
let querySeq = 0;
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

function renderItem(item: Item, index: number): HTMLLIElement {
  const li = document.createElement("li");
  li.className = "result";
  li.dataset.index = String(index);
  if (index === state.selected) li.classList.add("selected");
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
    if (li) {
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
  listWrap.hidden = rows.length === 0;
  list.replaceChildren(...rows.map((item, i) => renderItem(item, state.top + i)));
  const cap = capOf(state);
  const len = state.items.length;
  track.hidden = len <= cap;
  if (len > cap) {
    thumb.style.height = `${(cap / len) * 100}%`;
    thumb.style.top = `${(state.top / len) * 100}%`;
  }
  renderMenu();
}

function dispatch(event: Event) {
  const { state: next, effects } = reduce(state, event);
  state = next;
  render();
  effects.forEach(run);
}

function run(effect: Effect) {
  switch (effect.type) {
    case "query": {
      const seq = ++querySeq;
      invoke<Item[]>("query", { text: effect.text, scope: effect.scope })
        .then((items) => {
          if (seq === querySeq) dispatch({ type: "results", items });
        })
        .catch((err) => {
          if (seq === querySeq) dispatch({ type: "queryFailed", message: String(err) });
        });
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
});
const rowIndex = (e: Event_): number | null => {
  const li = (e.target as Element | null)?.closest<HTMLElement>("li.result");
  return li?.dataset.index === undefined ? null : Number(li.dataset.index);
};
document.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  const index = rowIndex(e);
  if (index === null) {
    if (state.menu) dispatch({ type: "menuClose" });
    return;
  }
  menuAt = { x: e.clientX, y: e.clientY };
  dispatch({ type: "menu", index });
});
window.addEventListener("blur", () => dispatch({ type: "blurred" }));

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
  const entry = target?.closest<HTMLElement>(".menu-item");
  if (entry) {
    dispatch({ type: "menuPick", action: entry.dataset.action as MenuAction });
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

const shownListener = listen("bar:shown", () => {
  shows++; // drop activation results from before this show
  querySeq++; // drop answers to queries from before this show
  dispatch({ type: "shown" });
  input.focus();
});
shownListener.catch(listenFailed);
// A rate or forecast arrived: the bar asks again if the text is still the same.
listen("answers:ready", () => dispatch({ type: "answersReady" })).catch(listenFailed);
listen("update:changed", () => dispatch({ type: "updateChanged" })).catch(listenFailed);
// The back end hid the bar (Esc, focus loss, an activation): no live updates until it is shown again.
listen("bar:hidden", () => dispatch({ type: "hidden" })).catch(listenFailed);
listen<string | null>("config:error", (e) => dispatch({ type: "configError", message: e.payload })).catch(listenFailed);

render();
input.focus();

// The first show (first start) can fire before the listener above is in place,
// so its `bar:shown` is lost. Ask for the empty-bar rows once the listener is
// registered. A typed query bumps `querySeq` and so wins over a late answer.
void shownListener.then(() => run({ type: "query", text: "", scope: null }), () => {});
