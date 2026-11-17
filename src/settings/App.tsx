import { useEffect, useState } from "preact/hooks";
import { onSettingsChanged, settingsOpenFile, settingsState, type SettingsState } from "./api";
import { Answers } from "./Answers";
import { General } from "./General";
import { Search } from "./Search";
import { TABS, TAB_LABELS, nextTab, type Tab } from "./tabs";

export function App() {
  const [state, setState] = useState<SettingsState | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("general");

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

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.key === "Tab") {
        e.preventDefault();
        setTab((t) => nextTab(t, e.shiftKey ? -1 : 1));
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
            onClick={() => setTab(t)}
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
        {tab !== "general" && tab !== "search" && tab !== "answers" && <p class="placeholder">Coming soon.</p>}
      </fieldset>
    </div>
  );
}
