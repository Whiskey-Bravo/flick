import type { DirectHotkeyDef } from "./api";
import { followFile, type FileSync } from "./listEditorLogic";

export type HkForm = { keys: string; item: { provider: string; id: string; title: string } | null };

export const emptyHkForm = (): HkForm => ({ keys: "", item: null });
export const toHkForm = (d: DirectHotkeyDef): HkForm => ({ keys: d.keys, item: d.item });
export const isHkDirty = (saved: HkForm | null, form: HkForm): boolean =>
  JSON.stringify(saved ?? emptyHkForm()) !== JSON.stringify(form);

export type RemoveError = { keys: string; text: string };

/** The Remove error stays only while the row it belongs to is still in the file. */
export const keepRemoveError = (err: RemoveError | null, defs: DirectHotkeyDef[]): RemoveError | null =>
  err && defs.some((d) => d.keys === err.keys) ? err : null;

export type HkSync = FileSync<HkForm>;

/** What an open hotkey form does when the config file changed under it. */
export function hkSync(
  open: boolean,
  original: string | null,
  dirty: boolean,
  saved: HkForm | null,
  defs: DirectHotkeyDef[],
): HkSync {
  return followFile({ open, original, dirty, saved }, defs, {
    keyOf: (d) => d.keys,
    toForm: toHkForm,
    isDirty: isHkDirty,
  });
}
