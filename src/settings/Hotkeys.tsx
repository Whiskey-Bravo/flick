import { useEffect, useRef, useState } from "preact/hooks";
import {
  onSettingsChanged,
  settingsDeleteHotkey,
  settingsHotkeyStatus,
  settingsPauseHotkey,
  settingsSaveHotkey,
  type KeyStatusRow,
  type PickRow,
  type SettingsState,
} from "./api";
import { DIRECT_HINT, WEAK_COMBO, directComboOk, hotkeyFromEvent, hotkeyKeys } from "./hotkey";
import { emptyHkForm as empty, hkSync, isHkDirty, keepRemoveError, type HkForm, type RemoveError } from "./hotkeyForm";
import { ItemPicker } from "./ItemPicker";
import type { LeaveGuard } from "./Shortcuts";
import "./shortcuts.css";
import "./workspaces.css";

type Choice = "save" | "discard" | "stay";
const message = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : "Could not save.");

let calls: Promise<unknown> = Promise.resolve();

function Caps({ keys }: { keys: string }) {
  return (
    <>
      {hotkeyKeys(keys).map((k, i) => (
        <>
          {i > 0 && <span class="plus">+</span>}
          <kbd>{k}</kbd>
        </>
      ))}
    </>
  );
}

export function Hotkeys({ state, guard }: { state: SettingsState; guard: LeaveGuard }) {
  const defs = state.config.direct_hotkey;
  const [status, setStatus] = useState<KeyStatusRow[]>([]);
  const [open, setOpen] = useState(false);
  const [original, setOriginal] = useState<string | null>(null);
  const [form, setForm] = useState<HkForm>(empty());
  const [saved, setSaved] = useState<HkForm | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [removeError, setRemoveError] = useState<RemoveError | null>(null);
  const [changedNote, setChangedNote] = useState(false);
  const [asking, setAsking] = useState<((c: Choice) => void) | null>(null);
  const [capturing, setCapturing] = useState(false);
  const paused = useRef(false);
  const askRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (asking) askRef.current?.scrollIntoView({ block: "nearest" });
  }, [asking !== null]);

  // The status line follows every reload.
  useEffect(() => {
    let gone = false;
    const load = () => {
      settingsHotkeyStatus().then(
        (rows) => !gone && setStatus(rows),
        () => {},
      );
    };
    load();
    const unlisten = onSettingsChanged(load).catch(() => () => {});
    return () => {
      gone = true;
      unlisten.then((f) => f());
    };
  }, []);

  // Hotkeys are released while capturing, or pressing the combination would fire it.
  const pause = (on: boolean) => {
    if (paused.current === on) return;
    paused.current = on;
    calls = calls.then(() => settingsPauseHotkey(on)).catch(() => {});
  };
  const startCapture = () => {
    pause(true);
    setCapturing(true);
  };
  const stopCapture = () => {
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
    if (!open) stopCapture();
  }, [open]);

  const onKey = (e: KeyboardEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.code === "Escape") {
      stopCapture();
      return;
    }
    const combo = hotkeyFromEvent(e);
    if (combo === null) return;
    setForm((f) => ({ ...f, keys: combo }));
    setSaveError(null);
    stopCapture();
  };

  const dirty = open && isHkDirty(saved, form);
  const weak = form.keys !== "" && !directComboOk(form.keys);
  const valid = form.keys !== "" && !weak && form.item !== null;

  // The file changed: follow it unless there is an unsaved edit.
  const latest = useRef({ open, dirty, saved, original });
  latest.current = { open, dirty, saved, original };
  useEffect(() => {
    const { open, dirty, saved, original } = latest.current;
    setRemoveError((e) => keepRemoveError(e, defs));
    const act = hkSync(open, original, dirty, saved, defs);
    if (act.kind === "note") {
      setChangedNote(true);
    } else if (act.kind === "reload") {
      setSaved(act.form);
      setForm(act.form);
    } else if (act.kind === "close") {
      setOpen(false);
      setOriginal(null);
      setSaved(null);
      setForm(empty());
    }
  }, [state]);

  const doSave = async (): Promise<boolean> => {
    if (!form.item) return false;
    setSaveError(null);
    try {
      await settingsSaveHotkey(original, { keys: form.keys, item: form.item });
      setSaved(form);
      setOriginal(form.keys);
      setChangedNote(false);
      setRemoveError(null);
      return true;
    } catch (e) {
      setSaveError(message(e));
      return false;
    }
  };

  const pending = useRef<Promise<boolean> | null>(null);
  const mayLeave = (): Promise<boolean> => {
    if (!dirty) return Promise.resolve(true);
    if (pending.current) return pending.current;
    const p = (async () => {
      const c = await new Promise<Choice>((resolve) => setAsking(() => resolve));
      setAsking(null);
      if (c === "stay") return false;
      if (c === "save") return await doSave();
      return true;
    })();
    pending.current = p;
    void p.finally(() => (pending.current = null));
    return p;
  };
  guard.current = mayLeave;
  useEffect(() => () => void (guard.current = null), []);

  const show = (def: { keys: string; item: HkForm["item"] } | null) => {
    const f: HkForm = def ? { keys: def.keys, item: def.item } : empty();
    setOpen(true);
    setOriginal(def ? def.keys : null);
    setSaved(def ? f : null);
    setForm(f);
    setSaveError(null);
    setRemoveError(null);
    setChangedNote(false);
  };

  const select = async (def: { keys: string; item: HkForm["item"] } | null) => {
    if (def && open && original === def.keys) return;
    if (await mayLeave()) show(def);
  };

  const remove = async (keys: string) => {
    setRemoveError(null);
    try {
      await settingsDeleteHotkey(keys);
      if (open && original === keys) {
        setOpen(false);
        setOriginal(null);
        setSaved(null);
        setForm(empty());
      }
      setSaveError(null);
    } catch (e) {
      setRemoveError({ keys, text: message(e) });
    }
  };

  const pickItem = (row: PickRow) => {
    setForm((f) => ({ ...f, item: { provider: row.provider, id: row.id, title: row.title } }));
    setSaveError(null);
  };

  return (
    <div class="sc">
      <div class="sc-list hk-list">
        <button class="sc-add" onClick={() => select(null)}>
          + Add hotkey
        </button>
        {defs.map((d) => {
          const msg = status.find((s) => s.keys === d.keys);
          return (
            <div key={d.keys} class={open && original === d.keys ? "sc-row hk-row active" : "sc-row hk-row"}>
              <button class="hk-main" onClick={() => select(d)}>
                <span class="sc-name">
                  <Caps keys={d.keys} />
                </span>
                <span class="sc-about">{d.item.title}</span>
                {msg && msg.state !== "ok" && <span class="hk-status">{msg.message}</span>}
              </button>
              <button class="hk-remove" onClick={() => void remove(d.keys)}>
                Remove
              </button>
            </div>
          );
        })}
        {defs.length === 0 && <p class="sc-hint">No hotkeys yet.</p>}
        {removeError !== null && (
          <p class="sc-error" role="alert">
            {removeError.text}
          </p>
        )}
      </div>

      {!open ? (
        <div class="sc-editor sc-empty">Pick a hotkey, or add a new one.</div>
      ) : (
        <div class="sc-editor">
          {asking && (
            <div class="sc-ask" role="alertdialog" ref={askRef}>
              <span>This shortcut has unsaved changes.</span>
              <button disabled={!valid} onClick={() => asking("save")}>
                Save
              </button>
              <button onClick={() => asking("discard")}>Discard</button>
              <button onClick={() => asking("stay")}>Stay</button>
            </div>
          )}

          {changedNote && <p class="sc-note">The config file changed. Your unsaved edit is kept.</p>}

          <div class="sc-field">
            <span>Keys</span>
            <button
              class={capturing ? "hotkey capturing hk-capture" : "hotkey hk-capture"}
              onClick={startCapture}
              onBlur={stopCapture}
              onKeyDown={capturing ? onKey : undefined}
            >
              {capturing ? "Press the keys\u2026" : form.keys === "" ? "Click, then press the keys" : <Caps keys={form.keys} />}
            </button>
            {weak && (
              <p class="sc-error" role="alert">
                {WEAK_COMBO}
              </p>
            )}
            <small>{DIRECT_HINT}</small>
          </div>

          <div class="sc-field">
            <span>Opens</span>
            {form.item && <div class="hk-chosen">{form.item.title}</div>}
            <ItemPicker forWorkspace={false} onPick={pickItem} />
          </div>

          {saveError !== null && (
            <p class="sc-error" role="alert">
              {saveError}
            </p>
          )}

          <div class="sc-actions">
            <button class="primary" disabled={!valid || !dirty} onClick={() => void doSave()}>
              Save
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
