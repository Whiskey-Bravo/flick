import { useEffect, useRef, useState } from "preact/hooks";
import { onSettingsChanged, settingsOpenFile, settingsState, type SettingsState } from "./api";
import { Shortcuts, type LeaveGuard } from "./Shortcuts";
import { TABS, TAB_LABELS, nextTab, type Tab } from "./tabs";

export function App() {
  const [state, setState] = useState<SettingsState | null>(null);
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
        void goTo(nextTab(tabRef.current, e.shiftKey ? -1 : 1));
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
            onClick={() => void goTo(t)}
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
        {tab === "shortcuts" && state ? (
          <Shortcuts state={state} guard={shortcutsGuard} />
        ) : (
          <p class="placeholder">Coming soon.</p>
        )}
      </fieldset>
    </div>
  );
}
