import { useEffect, useRef, useState } from "preact/hooks";
import {
  settingsCheckSnippet,
  settingsDeleteSnippet,
  settingsSaveSnippet,
  type SettingsState,
  type SnippetDef,
} from "./api";
import type { LeaveGuard } from "./Shortcuts";
import {
  emptySnippetForm,
  isSnippetDirty,
  preview,
  toSnippetDef,
  toSnippetForm,
  type SnippetForm,
} from "./snippetForm";
import "./shortcuts.css";

type Choice = "save" | "discard" | "stay";

const message = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : "Could not save.");

export function Snippets({ state, guard }: { state: SettingsState; guard: LeaveGuard }) {
  const defs = state.config.snippet;

  /** The name the open snippet has in the file; null while adding a new one. */
  const [original, setOriginal] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [form, setForm] = useState<SnippetForm>(emptySnippetForm());
  const [saved, setSaved] = useState<SnippetForm | null>(null);
  const [check, setCheck] = useState<{ key: string; error: string | null } | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [changedNote, setChangedNote] = useState(false);
  const [asking, setAsking] = useState<((c: Choice) => void) | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const askRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (asking) askRef.current?.scrollIntoView({ block: "nearest" });
  }, [asking !== null]);

  const dirty = open && isSnippetDirty(saved, form);
  const key = JSON.stringify(form);
  /** Nothing typed yet: no complaint about the empty fields. */
  const blank = form.name === "" && form.text === "";

  // Live check, debounced.
  useEffect(() => {
    if (!open) return;
    let stale = false;
    const t = setTimeout(() => {
      settingsCheckSnippet(original, toSnippetDef(form)).then(
        (error) => !stale && setCheck({ key, error }),
        () => {},
      );
    }, 200);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [open, original, key]);

  // The file changed: follow it unless there is an unsaved edit.
  const latest = useRef({ open, dirty, saved, original });
  latest.current = { open, dirty, saved, original };
  useEffect(() => {
    const { open, dirty, saved, original } = latest.current;
    if (!open || original === null) return;
    const def = defs.find((d) => d.name === original);
    const next = def ? toSnippetForm(def) : null;
    if (next && saved && !isSnippetDirty(saved, next)) return;
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

  const doSave = async (): Promise<boolean> => {
    setSaveError(null);
    try {
      await settingsSaveSnippet(original, toSnippetDef(form));
      const norm = toSnippetForm(toSnippetDef(form));
      setSaved(norm);
      setForm(norm);
      setOriginal(norm.name);
      setChangedNote(false);
      return true;
    } catch (e) {
      setSaveError(message(e));
      return false;
    }
  };

  const valid = check?.key === key && check.error === null;

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

  const show = (def: SnippetDef | null) => {
    const f = def ? toSnippetForm(def) : emptySnippetForm();
    setOpen(true);
    setOriginal(def ? def.name : null);
    setSaved(def ? f : null);
    setForm(f);
    setCheck(null);
    setSaveError(null);
    setChangedNote(false);
    setConfirmDelete(false);
  };

  const select = async (def: SnippetDef | null) => {
    if (def && open && original === def.name) return;
    if (await mayLeave()) show(def);
  };

  const remove = async () => {
    if (original === null) return;
    try {
      await settingsDeleteSnippet(original);
      setOpen(false);
      setOriginal(null);
      setSaved(null);
      setForm(emptySnippetForm());
    } catch (e) {
      setSaveError(message(e));
    }
    setConfirmDelete(false);
  };

  const patch = (p: Partial<SnippetForm>) => {
    setForm((f) => ({ ...f, ...p }));
    setSaveError(null);
  };

  const shownCheck = check?.key === key && !blank ? check.error : null;

  return (
    <div class="sc">
      <div class="sc-list">
        <button class="sc-add" onClick={() => select(null)}>
          + Add snippet
        </button>
        {defs.map((d) => (
          <button
            key={d.name}
            class={open && original === d.name ? "sc-row active" : "sc-row"}
            onClick={() => select(d)}
          >
            <span class="sc-name">{d.name}</span>
            <span class="sc-about">{preview(d.text)}</span>
          </button>
        ))}
        {defs.length === 0 && <p class="sc-hint">No snippets yet.</p>}
      </div>

      {!open ? (
        <div class="sc-editor sc-empty">
          A snippet is a text you use often: an address, a signature, a reply. Type its name in the bar and press
          Enter to paste it into the window you were in.
        </div>
      ) : (
        <div class="sc-editor">
          {asking && (
            <div class="sc-ask" role="alertdialog" ref={askRef}>
              <span>This snippet has unsaved changes.</span>
              <button disabled={!valid} onClick={() => asking("save")}>
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
            <small>One word. Type it in the bar and press Enter to paste the text; Ctrl+Enter only copies it.</small>
          </label>
          <label class="sc-field">
            <span>Text</span>
            <textarea
              class="sn-text"
              rows={8}
              value={form.text}
              onInput={(e) => patch({ text: e.currentTarget.value })}
            />
            <small>Saved in your config file as plain text, so not the place for passwords.</small>
          </label>

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
            <button class="primary" disabled={!valid || !dirty} onClick={() => void doSave()}>
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
                <button onClick={() => setConfirmDelete(true)}>Delete snippet</button>
              ))}
          </div>
        </div>
      )}
    </div>
  );
}
