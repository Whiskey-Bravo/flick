import { useEffect, useRef, useState } from "preact/hooks";
import {
  onSettingsChanged,
  settingsClearHistory,
  settingsPauseHotkey,
  settingsSetStartAtLogin,
  settingsWinKeyStatus,
  type SettingsState,
} from "./api";
import { hotkeyFromEvent, hotkeyKeys } from "./hotkey";
import { useSaver } from "./saver";

let calls: Promise<unknown> = Promise.resolve();

export function General({ state }: { state: SettingsState }) {
  const { config, distros } = state;
  const { errors, save } = useSaver(state);
  const [capturing, setCapturing] = useState(false);
  const paused = useRef(false);
  // A reload (a tray toggle too) replaces what the call returned.
  const [login, setLogin] = useState<{ on: boolean; base: SettingsState } | null>(null);
  const [loginError, setLoginError] = useState("");
  const [cleared, setCleared] = useState(false);
  const [clearError, setClearError] = useState("");
  const [winKeyStatus, setWinKeyStatus] = useState<string | null>(null);

  // Asked again whenever the config reloads; a rejection (no answer yet) reads as "fine".
  useEffect(() => {
    let alive = true;
    const ask = () => {
      settingsWinKeyStatus().then(
        (s) => alive && setWinKeyStatus(s),
        () => alive && setWinKeyStatus(null),
      );
    };
    ask();
    const unlisten = onSettingsChanged(ask);
    return () => {
      alive = false;
      void unlisten.then((u) => u(), () => {});
    };
  }, []);

  // The global hotkey is released while capturing, or pressing it would toggle the bar.
  const pause = (on: boolean) => {
    if (paused.current === on) return;
    paused.current = on;
    // One chain, so a quick pause then resume reach Rust in that order.
    calls = calls.then(() => settingsPauseHotkey(on)).catch(() => {});
  };
  // Capturing as it is now, for a save that ends after a new capture started.
  const capturingNow = useRef(false);
  const startCapture = () => {
    pause(true);
    capturingNow.current = true;
    setCapturing(true);
  };
  const stopCapture = () => {
    capturingNow.current = false;
    setCapturing(false);
    pause(false);
  };
  useEffect(() => {
    const resume = () => pause(false);
    window.addEventListener("beforeunload", resume);
    return () => {
      window.removeEventListener("beforeunload", resume);
      resume();
    };
  }, []);
  useEffect(() => {
    if (!cleared) return;
    const t = setTimeout(() => setCleared(false), 3000);
    return () => clearTimeout(t);
  }, [cleared]);

  const onKey = (e: KeyboardEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.code === "Escape") {
      stopCapture();
      return;
    }
    const combo = hotkeyFromEvent(e);
    if (combo === null) return;
    capturingNow.current = false;
    setCapturing(false);
    const done = combo !== config.hotkey ? save("hotkey", { key: "hotkey", value: combo }) : Promise.resolve();
    // The hotkey stays released when the button was clicked again meanwhile.
    void done.then(() => {
      if (!capturingNow.current) pause(false);
    });
  };

  const setStartAtLogin = async (on: boolean) => {
    setLogin({ on, base: state });
    try {
      setLogin({ on: await settingsSetStartAtLogin(on), base: state });
      setLoginError("");
    } catch (e) {
      setLogin(null);
      setLoginError(String(e));
    }
  };

  const clear = async () => {
    setCleared(false);
    try {
      await settingsClearHistory();
      setClearError("");
      setCleared(true);
    } catch (e) {
      setClearError(String(e));
    }
  };

  return (
    <div>
      <div class="field">
        <label for="hotkey">Hotkey</label>
        <button
          id="hotkey"
          class={capturing ? "hotkey capturing" : "hotkey"}
          onClick={startCapture}
          onBlur={stopCapture}
          onKeyDown={capturing ? onKey : undefined}
        >
          {capturing
            ? "Press the new combination…"
            : hotkeyKeys(config.hotkey).map((k, i) => (
                <>
                  {i > 0 && <span class="plus">+</span>}
                  <kbd>{k}</kbd>
                </>
              ))}
        </button>
        <p class="hint">{capturing ? "Esc cancels." : "Click, then press the combination."}</p>
        {errors.hotkey && <p class="error">{errors.hotkey}</p>}
      </div>

      <div class="field">
        <label class="check">
          <input
            type="checkbox"
            checked={login !== null && login.base === state ? login.on : state.start_at_login}
            onChange={(e) => void setStartAtLogin(e.currentTarget.checked)}
          />
          Start at login
        </label>
        {loginError && <p class="error">{loginError}</p>}
      </div>

      <div class="field">
        <label class="check">
          <input
            type="checkbox"
            checked={config.win_key}
            onChange={(e) => void save("win_key", { key: "win_key", value: e.currentTarget.checked })}
          />
          Win key opens Flick
        </label>
        <p class="hint">Ctrl+Esc or the Start menu row still open Start.</p>
        {config.win_key && winKeyStatus && <p class="error">{winKeyStatus}</p>}
        {errors.win_key && <p class="error">{errors.win_key}</p>}
      </div>

      <div class="field">
        <label class="check">
          <input
            type="checkbox"
            checked={config.show_taskbar}
            onChange={(e) => void save("show_taskbar", { key: "show_taskbar", value: e.currentTarget.checked })}
          />
          Show the taskbar while Flick is open
        </label>
        <p class="hint">For a taskbar that hides by itself. A Flick button sits on it while the bar is open.</p>
        {errors.show_taskbar && <p class="error">{errors.show_taskbar}</p>}
      </div>

      <div class="field">
        <label class="check">
          <input
            type="checkbox"
            checked={config.tab_completion}
            onChange={(e) => void save("tab_completion", { key: "tab_completion", value: e.currentTarget.checked })}
          />
          Tab completion
        </label>
        <p class="hint">
          The rest of the top result's name shows in grey as you type, and Tab takes it. No row is highlighted until you
          press Down; Enter still opens the top result.
        </p>
        {errors.tab_completion && <p class="error">{errors.tab_completion}</p>}
      </div>

      <div class="field">
        <label class="check">
          <input
            type="checkbox"
            checked={config.history}
            onChange={(e) => void save("history", { key: "history", value: e.currentTarget.checked })}
          />
          Remember past searches and launches
        </label>
        <div class="row">
          <button onClick={clear}>Clear history</button>
          {cleared && <span class="ok">Cleared</span>}
        </div>
        {clearError && <p class="error">{clearError}</p>}
        {errors.history && <p class="error">{errors.history}</p>}
      </div>

      {distros !== null && (
        <div class="field">
          <label for="distro">WSL distro for &gt;</label>
          <select
            id="distro"
            value={config.wsl_distro}
            onChange={(e) => void save("wsl_distro", { key: "wsl_distro", value: e.currentTarget.value })}
          >
            <option value="">WSL default</option>
            {distros.map((d) => (
              <option key={d} value={d}>
                {d}
              </option>
            ))}
          </select>
          {errors.wsl_distro && <p class="error">{errors.wsl_distro}</p>}
        </div>
      )}
    </div>
  );
}
