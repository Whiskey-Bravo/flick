import type { ShortcutDef } from "./api";

export type Kind = "open" | "run" | "wsl";

/** A shortcut as the editor holds it: everything is text or a switch. */
export type Form = {
  name: string;
  aliases: string;
  about: string;
  kind: Kind;
  target: string;
  /** null = not set (the default of the kind applies). */
  terminal: boolean | null;
  admin: boolean;
  listMode: "none" | "pick" | "choices";
  pick: string;
  /** One per line. */
  choices: string;
  /** "" = none: the built-in icon of the kind. */
  icon: string;
};

export const emptyForm = (): Form => ({
  name: "",
  aliases: "",
  about: "",
  kind: "open",
  target: "",
  terminal: null,
  admin: false,
  listMode: "none",
  pick: "",
  choices: "",
  icon: "",
});

export function toForm(def: ShortcutDef): Form {
  const kind: Kind = def.wsl !== undefined ? "wsl" : def.run !== undefined ? "run" : "open";
  const listMode = def.pick !== undefined ? "pick" : def.choices.length > 0 ? "choices" : "none";
  return {
    name: def.name,
    aliases: def.aliases.join(", "),
    about: def.about,
    kind,
    target: def[kind] ?? "",
    terminal: def.terminal ?? null,
    admin: def.admin,
    listMode,
    pick: def.pick ?? "",
    choices: def.choices.join("\n"),
    icon: def.icon ?? "",
  };
}

export function toDef(f: Form): ShortcutDef {
  const def: ShortcutDef = {
    name: f.name.trim(),
    aliases: f.aliases.split(/[,\s]+/).filter((a) => a !== ""),
    about: f.about.trim(),
    admin: f.admin,
    choices: f.listMode === "choices" ? f.choices.split(/\r?\n/).map((c) => c.trim()).filter((c) => c !== "") : [],
  };
  def[f.kind] = f.target.trim();
  if (f.terminal !== null && f.kind !== "open") def.terminal = f.terminal;
  const pick = f.pick.trim();
  if (f.listMode === "pick" && pick !== "") def.pick = pick;
  const icon = f.icon.trim();
  if (icon !== "") def.icon = icon;
  return def;
}

const PLACEHOLDER = /\{(arg|name|path)\}/;

export type DisabledField = "admin" | "terminal" | "pick" | "choices" | "wsl";

/** Fields the rules forbid for this form, each with the reason shown beside it. */
export function disabledReasons(f: Form, wsl: boolean): Partial<Record<DisabledField, string>> {
  const out: Partial<Record<DisabledField, string>> = {};
  // A field that is already set stays enabled, so it can be cleared.
  if (!f.admin) {
    if (f.kind === "wsl") out.admin = "not available for WSL commands";
    else if (PLACEHOLDER.test(f.target)) out.admin = "can't be combined with placeholders";
  }
  // Meaningless for `open`; toDef drops it.
  if (f.kind === "open") out.terminal = "only for commands";
  if (/\{arg\}/.test(f.target)) {
    if (f.listMode !== "pick") out.pick = "can't be combined with {arg}";
    if (f.listMode !== "choices") out.choices = "can't be combined with {arg}";
  }
  if (!wsl) out.wsl = "WSL is not installed";
  return out;
}

/** The icon key of a row found in the item picker as a shortcut's `icon`, or null when the row has no icon of its own. */
export function iconFromRow(rowIcon: string): string | null {
  return rowIcon.startsWith("file:") && rowIcon.length > 5 ? rowIcon.slice(5) : null;
}

/** `saved` is null for a shortcut not yet created (compared with a blank form). */
export function isDirty(saved: Form | null, current: Form): boolean {
  return JSON.stringify(saved ?? emptyForm()) !== JSON.stringify(current);
}
