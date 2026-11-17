import { useEffect, useState } from "preact/hooks";
import { onSettingsChanged, settingsOpenFile, settingsState, type SettingsState } from "./api";
import { TABS, TAB_LABELS, nextTab, type Tab } from "./tabs";

export function App() {
  const [state, setState] = useState<SettingsState | null>(null);
  const [tab, setTab] = useState<Tab>("general");

  useEffect(() => {
    const load = () => {
      settingsState().then(setState, () => {});
    };
    load();
    const unlisten = onSettingsChanged(load);
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

  const broken = state?.file_error != null;

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
      {broken && (
        <div class="banner" role="alert">
          <span>config.toml has a problem: {state?.file_error}</span>
          <button onClick={() => settingsOpenFile().catch(() => {})}>Open the file</button>
        </div>
      )}
      {/* While the file is broken every input inside is disabled. */}
      <fieldset class="page" disabled={broken}>
        <h1>{TAB_LABELS[tab]}</h1>
        <p class="placeholder">Coming soon.</p>
      </fieldset>
    </div>
  );
}
