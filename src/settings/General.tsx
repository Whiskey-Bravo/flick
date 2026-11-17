import { useState } from "preact/hooks";
import { settingsClearHistory, settingsSetStartAtLogin, type SettingsState } from "./api";
import { hotkeyFromEvent, hotkeyKeys } from "./hotkey";
import { useSaver } from "./saver";

export function General({ state }: { state: SettingsState }) {
  const { config, distros } = state;
  const { errors, save } = useSaver(state);
  const [capturing, setCapturing] = useState(false);
  const [login, setLogin] = useState<boolean | null>(null);
  const [loginError, setLoginError] = useState("");
  const [cleared, setCleared] = useState(false);

  const onKey = (e: KeyboardEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.code === "Escape") {
      setCapturing(false);
      return;
    }
    const combo = hotkeyFromEvent(e);
    if (combo === null) return;
    setCapturing(false);
    if (combo !== config.hotkey) void save("hotkey", { key: "hotkey", value: combo });
  };

  const setStartAtLogin = async (on: boolean) => {
    setLogin(on);
    try {
      setLogin(await settingsSetStartAtLogin(on));
      setLoginError("");
    } catch (e) {
      setLogin(null);
      setLoginError(String(e));
    }
  };

  const clear = async () => {
    try {
      await settingsClearHistory();
      setCleared(true);
    } catch {
      setCleared(false);
    }
  };

  return (
    <div>
      <div class="field">
        <label for="hotkey">Hotkey</label>
        <button
          id="hotkey"
          class={capturing ? "hotkey capturing" : "hotkey"}
          onClick={() => setCapturing(true)}
          onBlur={() => setCapturing(false)}
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
            checked={login ?? state.start_at_login}
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
            checked={config.history}
            onChange={(e) => void save("history", { key: "history", value: e.currentTarget.checked })}
          />
          Remember past searches and launches
        </label>
        <div class="row">
          <button onClick={clear}>Clear history</button>
          {cleared && <span class="ok">Cleared</span>}
        </div>
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
