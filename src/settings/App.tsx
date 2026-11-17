import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useRef, useState } from "preact/hooks";
import { onSettingsChanged, settingsOpenFile, settingsState, type SettingsState } from "./api";
import { Answers } from "./Answers";
import { General } from "./General";
import { Search } from "./Search";
import { Shortcuts, type LeaveGuard } from "./Shortcuts";
import { Updates } from "./Updates";
import { TABS, TAB_LABELS, nextTab, type Tab } from "./tabs";

export function App() {
  const [state, setState] = useState<SettingsState | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("general");
  // The Shortcuts editor sets this while it is open; it asks Save / Discard / Stay.
  const shortcutsGuard: LeaveGuard = useRef(null);
  const tabRef = useRef(tab);
  tabRef.current = tab;
  const goTo = async (next: Tab) => {
    if (next === tabRef.current) return;
    if (tabRef.current === "shortcuts" && shortcutsGuard.current && !(await shortcutsGuard.current())) return;
    setTab(next);
  };

  useEffect(() => {
    const load = () => {
      settingsState().then(
        (s) => {
          setLoadError(null);
          setState(s);
        },
        (e) => setLoadError(String(e)),
      );
    };
    // Listen first, so a change between the first load and the listener is not missed.
    const unlisten = onSettingsChanged(load).then(
      (f) => {
        load();
        return f;
      },
      () => {
        load();
        return () => {};
      },
    );
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  // Closing the window leaves the editor too: ask first, as a tab switch does.
  useEffect(() => {
    let off = () => {};
    let gone = false;
    getCurrentWindow()
      .onCloseRequested(async (e) => {
        if (shortcutsGuard.current && !(await shortcutsGuard.current())) e.preventDefault();
      })
      .then((f) => (gone ? f() : (off = f)))
      .catch(() => {});
    return () => {
      gone = true;
      off();
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.key === "Tab") {
        e.preventDefault();
        void goTo(nextTab(tabRef.current, e.shiftKey ? -1 : 1));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const broken = state === null || state.file_error != null;

  return (
    <div class="settings">
      <div class="tabs" role="tablist">
        {TABS.map((t) => (
          <button
            key={t}
            role="tab"
            class={t === tab ? "tab active" : "tab"}
            aria-selected={t === tab}
            onClick={() => void goTo(t)}
          >
            {TAB_LABELS[t]}
          </button>
        ))}
      </div>
      {loadError !== null && (
        <div class="banner" role="alert">
          <span>Could not read the settings: {loadError}</span>
        </div>
      )}
      {state?.file_error != null && (
        <div class="banner" role="alert">
          <span>config.toml has a problem: {state?.file_error}</span>
          <button onClick={() => settingsOpenFile().catch(() => {})}>Open the file</button>
        </div>
      )}
      {/* While the file is broken every input inside is disabled. */}
      <fieldset class="page" disabled={broken}>
        <h1>{TAB_LABELS[tab]}</h1>
        {state !== null && tab === "general" && <General state={state} />}
        {state !== null && tab === "search" && <Search state={state} />}
        {state !== null && tab === "answers" && <Answers state={state} />}
        {state !== null && tab === "shortcuts" && <Shortcuts state={state} guard={shortcutsGuard} />}
        {tab === "updates" && <Updates />}
      </fieldset>
    </div>
  );
}
