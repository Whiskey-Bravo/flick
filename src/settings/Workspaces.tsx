import { useEffect, useRef, useState } from "preact/hooks";
import {
  settingsCheckWorkspace,
  settingsDeleteWorkspace,
  settingsItemsFound,
  settingsSaveWorkspace,
  type SettingsState,
  type WorkspaceDef,
} from "./api";
import { ItemPicker } from "./ItemPicker";
import { oneAtATime } from "./oneAtATime";
import type { LeaveGuard } from "./Shortcuts";
import {
  addItem,
  emptyWsForm,
  isWsDirty,
  moveItem,
  removeItem,
  toWsDef,
  toWsForm,
  type WsForm,
} from "./workspaceForm";
import "./shortcuts.css";
import "./workspaces.css";

type Choice = "save" | "discard" | "stay";

const message = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : "Could not save.");

export function Workspaces({ state, guard }: { state: SettingsState; guard: LeaveGuard }) {
  const defs = state.config.workspace;

  /** The name the open workspace has in the file; null while adding a new one. */
  const [original, setOriginal] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [form, setForm] = useState<WsForm>(emptyWsForm());
  const [saved, setSaved] = useState<WsForm | null>(null);
  const [check, setCheck] = useState<{ key: string; error: string | null } | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [changedNote, setChangedNote] = useState(false);
  const [asking, setAsking] = useState<((c: Choice) => void) | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [found, setFound] = useState<{ key: string; list: boolean[] } | null>(null);
  const askRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (asking) askRef.current?.scrollIntoView({ block: "nearest" });
  }, [asking !== null]);

  const dirty = open && isWsDirty(saved, form);
  const key = JSON.stringify(form);
  const itemsKey = JSON.stringify(form.items);

  // Live check, debounced.
  useEffect(() => {
    if (!open) return;
    let stale = false;
    const t = setTimeout(() => {
      settingsCheckWorkspace(original, toWsDef(form)).then(
        (error) => !stale && setCheck({ key, error }),
        () => {},
      );
    }, 200);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [open, original, key]);

  // Which items can still be rebuilt.
  useEffect(() => {
    if (!open || form.items.length === 0) {
      setFound(null);
      return;
    }
    let stale = false;
    settingsItemsFound(form.items).then(
      (list) => !stale && setFound({ key: itemsKey, list }),
      () => {},
    );
    return () => {
      stale = true;
    };
  }, [open, itemsKey, state]);

  // The file changed: follow it unless there is an unsaved edit.
  const latest = useRef({ open, dirty, saved, original });
  latest.current = { open, dirty, saved, original };
  useEffect(() => {
    const { open, dirty, saved, original } = latest.current;
    if (!open || original === null) return;
    const def = defs.find((d) => d.name === original);
    const next = def ? toWsForm(def) : null;
    if (next && saved && !isWsDirty(saved, next)) return;
    if (dirty) {
      setChangedNote(true);
    } else if (next) {
      setSaved(next);
      setForm(next);
    } else {
      setOpen(false);
      setOriginal(null);
    }
  }, [state]);

  // The editor as it is now: the leave question's Save (made by an older
  // render) sends what the editor shows when it is clicked.
  const now = useRef({ form, original });
  now.current = { form, original };
  const [saving, setSaving] = useState(false);
  const [once] = useState(() => oneAtATime(setSaving));

  const doSave = (): Promise<boolean> => once(async () => {
    const { form, original } = now.current;
    setSaveError(null);
    try {
      await settingsSaveWorkspace(original, toWsDef(form));
      const norm = toWsForm(toWsDef(form));
      setSaved(norm);
      setForm(norm);
      setOriginal(norm.name);
      setChangedNote(false);
      return true;
    } catch (e) {
      setSaveError(message(e));
      return false;
    }
  });

  const valid = check?.key === key && check.error === null && form.name.trim() !== "";

  // A second leave request while the question is open waits for the same answer.
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

  const show = (def: WorkspaceDef | null) => {
    const f = def ? toWsForm(def) : emptyWsForm();
    setOpen(true);
    setOriginal(def ? def.name : null);
    setSaved(def ? f : null);
    setForm(f);
    setCheck(null);
    setSaveError(null);
    setChangedNote(false);
    setConfirmDelete(false);
  };

  const select = async (def: WorkspaceDef | null) => {
    if (def && open && original === def.name) return;
    if (await mayLeave()) show(def);
  };

  const remove = async () => {
    if (original === null) return;
    try {
      await settingsDeleteWorkspace(original);
      setOpen(false);
      setOriginal(null);
      setSaved(null);
      setForm(emptyWsForm());
    } catch (e) {
      setSaveError(message(e));
    }
    setConfirmDelete(false);
  };

  const patch = (p: Partial<WsForm>) => {
    setForm((f) => ({ ...f, ...p }));
    setSaveError(null);
  };

  const shownCheck = check?.key === key ? check.error : null;
  const foundList = found?.key === itemsKey ? found.list : null;

  return (
    <div class="sc">
      <div class="sc-list">
        <button class="sc-add" onClick={() => select(null)}>
          + Add workspace
        </button>
        {defs.map((d) => (
          <button
            key={d.name}
            class={open && original === d.name ? "sc-row active" : "sc-row"}
            onClick={() => select(d)}
          >
            <span class="sc-name">{d.name}</span>
            <span class="sc-about">{d.items.length === 1 ? "1 item" : `${d.items.length} items`}</span>
          </button>
        ))}
        {defs.length === 0 && <p class="sc-hint">No workspaces yet.</p>}
      </div>

      {!open ? (
        <div class="sc-editor sc-empty">Pick a workspace, or add a new one.</div>
      ) : (
        <div class="sc-editor">
          {asking && (
            <div class="sc-ask" role="alertdialog" ref={askRef}>
              <span>This workspace has unsaved changes.</span>
              <button disabled={!valid || saving} onClick={() => asking("save")}>
                Save
              </button>
              <button onClick={() => asking("discard")}>Discard</button>
              <button onClick={() => asking("stay")}>Stay</button>
            </div>
          )}
          {changedNote && <p class="sc-note">The config file changed. Your unsaved edit is kept.</p>}

          <label class="sc-field">
            <span>Name</span>
            <input value={form.name} onInput={(e) => patch({ name: e.currentTarget.value })} />
            <small>Type its name in the bar and press Enter to open every item.</small>
          </label>
          <label class="sc-field">
            <span>Other names</span>
            <input value={form.aliases} onInput={(e) => patch({ aliases: e.currentTarget.value })} />
          </label>

          <div class="sc-field">
            <span>Items</span>
            {form.items.length > 0 && (
              <ul class="ws-items">
                {form.items.map((it, i) => (
                  <li key={it.provider + "\u0000" + it.id} class="ws-item">
                    <span class="ws-title">{it.title}</span>
                    {foundList && foundList[i] === false && <span class="ws-missing">Not found</span>}
                    <button disabled={i === 0} onClick={() => patch({ items: moveItem(form.items, i, true) })}>
                      Up
                    </button>
                    <button
                      disabled={i === form.items.length - 1}
                      onClick={() => patch({ items: moveItem(form.items, i, false) })}
                    >
                      Down
                    </button>
                    <button onClick={() => patch({ items: removeItem(form.items, i) })}>Remove</button>
                  </li>
                ))}
              </ul>
            )}
            <ItemPicker forWorkspace={true} onPick={(row) => patch({ items: addItem(form.items, row) })} />
          </div>

          {shownCheck !== null && (
            <p class="sc-error" role="alert">
              {shownCheck}
            </p>
          )}
          {saveError !== null && (
            <p class="sc-error" role="alert">
              {saveError}
            </p>
          )}

          <div class="sc-actions">
            <button class="primary" disabled={!valid || !dirty || saving} onClick={() => void doSave()}>
              Save
            </button>
            {original !== null &&
              (confirmDelete ? (
                <>
                  <span>Delete "{original}"?</span>
                  <button class="danger" onClick={() => void remove()}>
                    Delete
                  </button>
                  <button onClick={() => setConfirmDelete(false)}>Cancel</button>
                </>
              ) : (
                <button onClick={() => setConfirmDelete(true)}>Delete workspace</button>
              ))}
          </div>
        </div>
      )}
    </div>
  );
}
