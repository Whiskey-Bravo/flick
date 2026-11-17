/** The slice of a KeyboardEvent the capture needs. */
export type KeyInfo = {
  key: string;
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
};

const NAMED: Record<string, string> = {
  Space: "Space",
  Enter: "Enter",
  Tab: "Tab",
  Backspace: "Backspace",
  Delete: "Delete",
  Insert: "Insert",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
};

/** The name the hotkey parser knows for the physical key, or null. */
function keyName(code: string): string | null {
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter) return letter[1];
  const digit = /^Digit([0-9])$/.exec(code);
  if (digit) return digit[1];
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  return NAMED[code] ?? null;
}

/**
 * The accelerator string for a key press ("Alt+Space"), or null when the press
 * is not a usable hotkey: a lone modifier, Escape (cancels), or a key without
 * Ctrl/Alt/Win (function keys excepted). Modifier order: Ctrl+Alt+Shift+Super.
 */
export function hotkeyFromEvent(e: KeyInfo): string | null {
  if (e.code === "Escape") return null;
  const name = keyName(e.code);
  if (name === null) return null;
  const isFunction = /^F\d+$/.test(name);
  if (!isFunction && !e.ctrlKey && !e.altKey && !e.metaKey) return null;
  const parts: string[] = [];
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  if (e.metaKey) parts.push("Super");
  parts.push(name);
  return parts.join("+");
}

/** The keys of an accelerator, for display as key caps. */
export function hotkeyKeys(s: string): string[] {
  return s.split("+").map((p) => p.trim()).filter((p) => p !== "");
}
