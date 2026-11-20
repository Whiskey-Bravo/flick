import { useEffect, useRef, useState } from "preact/hooks";
import {
  onSettingsChanged,
  settingsDeleteHotkey,
  settingsHotkeyStatus,
  settingsPauseHotkey,
  settingsSaveHotkey,
  type DirectHotkeyDef,
  type KeyStatusRow,
  type PickRow,
  type SettingsState,
} from "./api";
import { DIRECT_HINT, WEAK_COMBO, directComboOk, hotkeyFromEvent, hotkeyKeys } from "./hotkey";
import {
  emptyHkForm as empty,
  isHkDirty,
  keepRemoveError,
  toHkForm,
  type HkForm,
  type RemoveError,
} from "./hotkeyForm";
import { ItemPicker } from "./ItemPicker";
import { EditorActions, EditorNotices, ErrorLine, ListLayout } from "./ListEditor";
import { errorMessage, type LeaveGuard } from "./listEditorLogic";
import { useListEditor } from "./useListEditor";
import "./shortcuts.css";
import "./workspaces.css";

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
  const [removeError, setRemoveError] = useState<RemoveError | null>(null);
  const [capturing, setCapturing] = useState(false);
  const paused = useRef(false);
  const ed = useListEditor<HkForm, DirectHotkeyDef>({
    state,
    guard,
    defs,
    keyOf: (d) => d.keys,
    formKey: (f) => f.keys,
    empty,
    toForm: toHkForm,
    isDirty: isHkDirty,
    // Saved as sent: the form is not replaced, and the Save button is off without an item.
    canSave: (form) => form.item !== null,
    write: (original, form) => settingsSaveHotkey(original, { keys: form.keys, item: form.item! }),
    onSaved: () => setRemoveError(null),
    onShow: () => setRemoveError(null),
    // The Remove error stays only while its row is still in the file.
    onFileChanged: () => setRemoveError((e) => keepRemoveError(e, defs)),
  });
  const { open, original, form } = ed;

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
    ed.patch({ keys: combo });
    stopCapture();
  };

  const weak = form.keys !== "" && !directComboOk(form.keys);
  const valid = form.keys !== "" && !weak && form.item !== null;

  const remove = async (keys: string) => {
    setRemoveError(null);
    try {
      await settingsDeleteHotkey(keys);
      if (open && original === keys) ed.close();
      ed.setSaveError(null);
    } catch (e) {
      setRemoveError({ keys, text: errorMessage(e) });
    }
  };

  const pickItem = (row: PickRow) => ed.patch({ item: { provider: row.provider, id: row.id, title: row.title } });

  return (
    <ListLayout
      listClass="hk-list"
      addLabel="+ Add hotkey"
      onAdd={() => ed.select(null)}
      rows={defs.map((d) => {
        const msg = status.find((s) => s.keys === d.keys);
        return (
          <div key={d.keys} class={ed.isOpen(d.keys) ? "sc-row hk-row active" : "sc-row hk-row"}>
            <button class="hk-main" onClick={() => ed.select(d)}>
              <span class="sc-name">
                <Caps keys={d.keys} />
              </span>
              <span class="sc-about">{d.item.title}</span>
              {msg && msg.state !== "ok" && <span class="hk-status">{msg.message}</span>}
            </button>
            {/* Removes at once, without the Delete confirm the other pages ask. */}
            <button class="hk-remove" onClick={() => void remove(d.keys)}>
              Remove
            </button>
          </div>
        );
      })}
      count={defs.length}
      emptyHint="No hotkeys yet."
      listEnd={removeError !== null && <ErrorLine>{removeError.text}</ErrorLine>}
      open={open}
      idle="Pick a hotkey, or add a new one."
    >
      <EditorNotices ed={ed} noun="hotkey" valid={valid} />

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
        {weak && <ErrorLine>{WEAK_COMBO}</ErrorLine>}
        <small>{DIRECT_HINT}</small>
      </div>

      <div class="sc-field">
        <span>Opens</span>
        {form.item && <div class="hk-chosen">{form.item.title}</div>}
        <ItemPicker forWorkspace={false} onPick={pickItem} />
      </div>

      {ed.saveError !== null && <ErrorLine>{ed.saveError}</ErrorLine>}

      <EditorActions ed={ed} valid={valid} />
    </ListLayout>
  );
}
