import { useEffect, useRef, useState } from "preact/hooks";
import type { SettingsState } from "./api";
import { askToLeave, errorMessage, followFile, type Choice, type LeaveGuard } from "./listEditorLogic";
import { oneAtATime } from "./oneAtATime";

export type ListEditorOptions<F, D, E, C> = {
  state: SettingsState;
  guard: LeaveGuard;
  /** The entries in the file, as the list shows them. */
  defs: D[];
  /** An entry's key in the file (its name, or a hotkey's keys). */
  keyOf: (d: D) => string;
  /** The key a saved form has in the file. */
  formKey: (f: F) => string;
  empty: () => F;
  toForm: (d: D) => F;
  isDirty: (saved: F | null, form: F) => boolean;
  /** Writes the form to the file; `original` is its key there, null for a new entry. */
  write: (original: string | null, form: F) => Promise<unknown>;
  /** The form as the file gives it back. When set, a save also replaces the
   * form with it; without it the saved copy is the form as sent and the
   * fields are left alone. */
  normalize?: (f: F) => F;
  /** False skips the save at once (answers false, keeps the shown error). */
  canSave?: (f: F) => boolean;
  /** How a failed save shows; the text of the error by default. */
  saveError?: (e: unknown) => E;
  /** The live check, run 200 ms after the last edit; null when fine. */
  check?: (original: string | null, form: F) => Promise<C | null>;
  /** Deletes the open entry (the Delete confirm under the editor). */
  remove?: (original: string) => Promise<unknown>;
  /** Clears the page's own state when another entry opens. */
  onShow?: () => void;
  /** Runs after a save went through. */
  onSaved?: () => void;
  /** Runs on every config change, before the editor follows the file. */
  onFileChanged?: () => void;
};

/**
 * The list-and-editor logic the Shortcuts, Snippets, Workspaces and Hotkeys
 * pages share: which entry is open, the unsaved-changes question on leaving,
 * following the config file, one save at a time, the debounced live check and
 * the Delete confirm.
 */
export function useListEditor<F, D, E = string, C = string>(o: ListEditorOptions<F, D, E, C>) {
  const { state, guard, defs } = o;

  /** The key the open entry has in the file; null while adding a new one. */
  const [original, setOriginal] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [form, setForm] = useState<F>(o.empty);
  const [saved, setSaved] = useState<F | null>(null);
  const [check, setCheck] = useState<{ key: string; error: C | null } | null>(null);
  const [saveError, setSaveError] = useState<E | string | null>(null);
  const [changedNote, setChangedNote] = useState(false);
  const [asking, setAsking] = useState<((c: Choice) => void) | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);

  const dirty = open && o.isDirty(saved, form);
  const key = JSON.stringify(form);

  // Live check, debounced.
  useEffect(() => {
    if (!open || !o.check) return;
    let stale = false;
    const t = setTimeout(() => {
      o.check!(original, form).then(
        (error) => !stale && setCheck({ key, error }),
        () => {},
      );
    }, 200);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [open, original, key]);

  const close = () => {
    setOpen(false);
    setOriginal(null);
    setSaved(null);
    setForm(o.empty());
  };

  // The file changed: follow it unless there is an unsaved edit.
  const latest = useRef({ open, dirty, saved, original });
  latest.current = { open, dirty, saved, original };
  useEffect(() => {
    o.onFileChanged?.();
    const act = followFile(latest.current, defs, o);
    if (act.kind === "note") {
      setChangedNote(true);
    } else if (act.kind === "reload") {
      setSaved(act.form);
      setForm(act.form);
    } else if (act.kind === "close") {
      close();
    }
  }, [state]);

  // The editor as it is now: the leave question's Save (made by an older
  // render) sends what the editor shows when it is clicked.
  const now = useRef({ form, original });
  now.current = { form, original };
  const [saving, setSaving] = useState(false);
  const [once] = useState(() => oneAtATime(setSaving));

  const doSave = (): Promise<boolean> =>
    once(async () => {
      const { form, original } = now.current;
      if (o.canSave && !o.canSave(form)) return false;
      setSaveError(null);
      try {
        await o.write(original, form);
        const norm = o.normalize ? o.normalize(form) : form;
        setSaved(norm);
        if (o.normalize) setForm(norm);
        setOriginal(o.formKey(norm));
        setChangedNote(false);
        o.onSaved?.();
        return true;
      } catch (e) {
        setSaveError(o.saveError ? o.saveError(e) : errorMessage(e));
        return false;
      }
    });

  const pending = useRef<Promise<boolean> | null>(null);
  /** Resolves true when the editor may be left or replaced. */
  const mayLeave = (): Promise<boolean> =>
    askToLeave(
      pending,
      dirty,
      () => new Promise<Choice>((resolve) => setAsking(() => resolve)),
      () => setAsking(null),
      doSave,
    );
  guard.current = mayLeave;
  useEffect(() => () => void (guard.current = null), []);

  const show = (def: D | null) => {
    const f = def ? o.toForm(def) : o.empty();
    setOpen(true);
    setOriginal(def ? o.keyOf(def) : null);
    setSaved(def ? f : null);
    setForm(f);
    setCheck(null);
    setSaveError(null);
    o.onShow?.();
    setChangedNote(false);
    setConfirmDelete(false);
  };

  /** Opens an entry (null: a new one), after the leave question if needed. */
  const select = async (def: D | null) => {
    if (def && open && original === o.keyOf(def)) return;
    if (await mayLeave()) show(def);
  };

  /** The Delete confirm's Delete: removes the open entry. */
  const remove = async () => {
    if (original === null || !o.remove) return;
    try {
      await o.remove(original);
      close();
    } catch (e) {
      setSaveError(errorMessage(e));
    }
    setConfirmDelete(false);
  };

  /** Changes fields of the form; a changed form drops the last save's error. */
  const patch = (p: Partial<F>) => {
    setForm((f) => ({ ...f, ...p }));
    setSaveError(null);
  };

  const fresh = check?.key === key;
  return {
    open,
    original,
    form,
    dirty,
    /** True when the entry with this key is the one open. */
    isOpen: (k: string) => open && original === k,
    patch,
    saveError,
    setSaveError,
    changedNote,
    asking,
    saving,
    doSave,
    select,
    close,
    /** The live check's error for the form as it is now; null when fine or not checked yet. */
    checkError: fresh ? check.error : null,
    /** The live check passed for the form as it is now. */
    checkPassed: fresh && check.error === null,
    confirmDelete,
    setConfirmDelete,
    remove,
  };
}

/** The part of an editor the shared pieces in ListEditor.tsx draw. */
export type ListEditor = {
  asking: ((c: Choice) => void) | null;
  changedNote: boolean;
  saving: boolean;
  dirty: boolean;
  doSave: () => Promise<boolean>;
  original: string | null;
  confirmDelete: boolean;
  setConfirmDelete: (on: boolean) => void;
  remove: () => Promise<void>;
};
