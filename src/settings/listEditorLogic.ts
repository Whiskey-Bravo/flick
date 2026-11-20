/**
 * The pure part of the list-and-editor pages (Shortcuts, Snippets,
 * Workspaces, Hotkeys): the leave question and following the config file.
 * `useListEditor` wires these into Preact state.
 */

/** Asks the editor whether the tab may be left; resolves false when the user chose Stay. */
export type LeaveGuard = { current: (() => Promise<boolean>) | null };

/** The answers to "This ... has unsaved changes." */
export type Choice = "save" | "discard" | "stay";

/** The text of a failed call. */
export const errorMessage = (e: unknown): string =>
  typeof e === "string" ? e : e instanceof Error ? e.message : "Could not save.";

/**
 * Resolves true when the editor may be left or replaced. A clean editor goes
 * at once; a dirty one asks Save / Discard / Stay. A second leave request while
 * the question is open (a second click on the window's X) waits for the same
 * answer instead of asking again. `answered` runs as soon as the answer is in,
 * before a Save starts.
 */
export function askToLeave(
  pending: { current: Promise<boolean> | null },
  dirty: boolean,
  ask: () => Promise<Choice>,
  answered: () => void,
  save: () => Promise<boolean>,
): Promise<boolean> {
  if (!dirty) return Promise.resolve(true);
  if (pending.current) return pending.current;
  const p = (async () => {
    const c = await ask();
    answered();
    if (c === "stay") return false;
    if (c === "save") return await save();
    return true;
  })();
  pending.current = p;
  void p.finally(() => (pending.current = null));
  return p;
}

/** What an open editor does when the config file changed under it. */
export type FileSync<F> = { kind: "none" } | { kind: "note" } | { kind: "reload"; form: F } | { kind: "close" };

export type FileSyncEditor<F> = {
  open: boolean;
  /** The entry's key in the file; null while adding a new one. */
  original: string | null;
  dirty: boolean;
  saved: F | null;
};

export type FileSyncRules<F, D> = {
  keyOf: (d: D) => string;
  toForm: (d: D) => F;
  isDirty: (saved: F | null, form: F) => boolean;
};

/**
 * Follow the file unless there is an unsaved edit: nothing when the file still
 * holds what was saved, a note when the edit is kept, a reload of a clean form,
 * or close when a clean form's entry left the file.
 */
export function followFile<F, D>(ed: FileSyncEditor<F>, defs: D[], rules: FileSyncRules<F, D>): FileSync<F> {
  if (!ed.open || ed.original === null) return { kind: "none" };
  const def = defs.find((d) => rules.keyOf(d) === ed.original);
  const next = def ? rules.toForm(def) : null;
  if (next && ed.saved && !rules.isDirty(ed.saved, next)) return { kind: "none" };
  if (ed.dirty) return { kind: "note" };
  return next ? { kind: "reload", form: next } : { kind: "close" };
}
