import type { SnippetDef } from "./api";

/** A snippet as the editor holds it. */
export type SnippetForm = { name: string; text: string };

export const emptySnippetForm = (): SnippetForm => ({ name: "", text: "" });

export const toSnippetForm = (def: SnippetDef): SnippetForm => ({ name: def.name, text: def.text });

/** The name loses the blanks around it; the text is kept exactly as typed. */
export const toSnippetDef = (f: SnippetForm): SnippetDef => ({ name: f.name.trim(), text: f.text });

/** `saved` is null for a snippet not yet created (compared with a blank form). */
export const isSnippetDirty = (saved: SnippetForm | null, form: SnippetForm): boolean => {
  const base = saved ?? emptySnippetForm();
  return base.name !== form.name || base.text !== form.text;
};

/** The text on one line, cut to `max` characters, for the list. */
export function preview(text: string, max = 40): string {
  const flat = text.trim().replace(/\s+/g, " ");
  return flat.length > max ? flat.slice(0, max) + "…" : flat;
}
