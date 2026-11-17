import { useEffect, useState } from "preact/hooks";
import { builtinName, serverUrl } from "../icons";
import { settingsMovePin, settingsPickFolder, settingsPins, togglePin, type PinRow, type SettingsState } from "./api";
import { useSaver } from "./saver";

export function Search({ state }: { state: SettingsState }) {
  const roots = state.config.file_roots;
  const { errors, save } = useSaver(state);
  const [pins, setPins] = useState<PinRow[]>([]);
  const [pinError, setPinError] = useState("");
  const [folderError, setFolderError] = useState("");

  const loadPins = () => {
    settingsPins().then(
      (p) => {
        setPins(p);
        setPinError("");
      },
      (e) => setPinError(String(e)),
    );
  };
  useEffect(loadPins, [state]);

  const setRoots = (value: string[]) => void save("file_roots", { key: "file_roots", value });
  const add = async () => {
    setFolderError("");
    try {
      const picked = await settingsPickFolder();
      if (picked !== null && !roots.includes(picked)) setRoots([...roots, picked]);
    } catch (e) {
      setFolderError(String(e));
    }
  };

  const act = (run: () => Promise<unknown>) => run().then(loadPins, (e) => setPinError(String(e)));

  return (
    <div>
      <div class="field">
        <h2>Folders to search</h2>
        {roots.length === 0 ? (
          <p class="hint">Empty: your whole user folder is searched</p>
        ) : (
          <ul class="list">
            {roots.map((r) => (
              <li key={r}>
                <span class="grow path">{r}</span>
                <button onClick={() => setRoots(roots.filter((x) => x !== r))}>Remove</button>
              </li>
            ))}
          </ul>
        )}
        <div class="row">
          <button onClick={add}>Add folder…</button>
        </div>
        {(errors.file_roots || folderError) && <p class="error">{errors.file_roots || folderError}</p>}
      </div>

      <div class="field">
        <h2>Pins</h2>
        {pins.length === 0 ? (
          <p class="hint">Nothing pinned yet.</p>
        ) : (
          <ul class="list">
            {pins.map((p, i) => (
              <li key={`${p.provider}:${p.id}`} class={p.found ? "" : "missing"}>
                <PinIcon pin={p} />
                <span class="grow">
                  <span class="title">{p.title}</span>
                  {p.found ? p.subtitle && <span class="sub">{p.subtitle}</span> : <span class="sub">not found</span>}
                </span>
                <button disabled={i === 0} aria-label="Move up" onClick={() => act(() => settingsMovePin(p.provider, p.id, true))}>
                  ↑
                </button>
                <button
                  disabled={i === pins.length - 1}
                  aria-label="Move down"
                  onClick={() => act(() => settingsMovePin(p.provider, p.id, false))}
                >
                  ↓
                </button>
                <button onClick={() => act(() => togglePin(p.provider, p.id))}>Unpin</button>
              </li>
            ))}
          </ul>
        )}
        {pinError && <p class="error">{pinError}</p>}
      </div>
    </div>
  );
}

function PinIcon({ pin }: { pin: PinRow }) {
  if (!pin.found || pin.icon === "" || builtinName(pin.icon) !== null) return <span class="icon" />;
  return (
    <img
      class="icon"
      src={serverUrl(pin.icon, window.devicePixelRatio || 1)}
      alt=""
      onError={(e) => {
        e.currentTarget.style.visibility = "hidden";
      }}
    />
  );
}
