import type { DirectHotkeyDef } from "./api";

export type HkForm = { keys: string; item: { provider: string; id: string; title: string } | null };

export const emptyHkForm = (): HkForm => ({ keys: "", item: null });
export const isHkDirty = (saved: HkForm | null, form: HkForm): boolean =>
  JSON.stringify(saved ?? emptyHkForm()) !== JSON.stringify(form);

export type HkSync = { kind: "none" } | { kind: "note" } | { kind: "reload"; form: HkForm } | { kind: "close" };

/** What an open hotkey form does when the config file changed under it. */
export function hkSync(
  open: boolean,
  original: string | null,
  dirty: boolean,
  saved: HkForm | null,
  defs: DirectHotkeyDef[],
): HkSync {
  if (!open || original === null) return { kind: "none" };
  const def = defs.find((d) => d.keys === original);
  const next: HkForm | null = def ? { keys: def.keys, item: def.item } : null;
  if (next && saved && !isHkDirty(saved, next)) return { kind: "none" };
  if (dirty) return { kind: "note" };
  return next ? { kind: "reload", form: next } : { kind: "close" };
}
