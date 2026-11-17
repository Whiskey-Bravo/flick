import type { ItemRef, PickRow, WorkspaceDef } from "./api";

/** A workspace as the editor holds it. */
export type WsForm = { name: string; aliases: string; items: ItemRef[] };

export const emptyWsForm = (): WsForm => ({ name: "", aliases: "", items: [] });

export const toWsForm = (def: WorkspaceDef): WsForm => ({
  name: def.name,
  aliases: def.aliases.join(", "),
  items: def.items.map((i) => ({ ...i })),
});

export const toWsDef = (f: WsForm): WorkspaceDef => ({
  name: f.name.trim(),
  aliases: f.aliases.split(/[,\s]+/).filter((a) => a !== ""),
  items: f.items.map((i) => ({ provider: i.provider, id: i.id, title: i.title })),
});

/** `saved` is null for a workspace not yet created (compared with a blank form). */
export const isWsDirty = (saved: WsForm | null, form: WsForm): boolean =>
  JSON.stringify(saved ?? emptyWsForm()) !== JSON.stringify(form);

export function addItem(items: ItemRef[], row: PickRow): ItemRef[] {
  if (items.some((i) => i.provider === row.provider && i.id === row.id)) return items;
  return [...items, { provider: row.provider, id: row.id, title: row.title }];
}

export function moveItem(items: ItemRef[], index: number, up: boolean): ItemRef[] {
  const to = up ? index - 1 : index + 1;
  if (index < 0 || index >= items.length || to < 0 || to >= items.length) return items;
  const out = items.slice();
  [out[index], out[to]] = [out[to], out[index]];
  return out;
}

export const removeItem = (items: ItemRef[], index: number): ItemRef[] => items.filter((_, i) => i !== index);
