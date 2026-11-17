// Which image a row's icon key stands for. Pure: no DOM, no Tauri.
import type { Item } from "./bar";

export const ICON_HOST = "http://flick-icon.localhost";

// `builtin:<name>` is served from the bundled SVGs, never from the icon server.
export const builtinName = (icon: string): string | null => (icon.startsWith("builtin:") ? icon.slice(8) : null);

export function serverUrl(icon: string, dpr: number): string {
  return `${ICON_HOST}/${encodeURIComponent(icon)}?px=${Math.round(32 * dpr)}`;
}

// The bundled SVG shown when the server has no icon (404): by kind of thing.
export function fallbackName(item: Pick<Item, "icon" | "provider">): string {
  if (item.icon === "folder") return "folder";
  if (item.icon.startsWith("ext:")) return "file";
  if (item.icon.startsWith("file:")) return item.provider === "files" ? "file" : "app";
  return "file";
}

// Rows are rebuilt on every render, so remember how each server URL fared:
// a known failure goes straight to the fallback, a known success is shown at
// once, and only an unknown URL is loaded (and faded in).
export function pickIconSrc(
  url: string,
  loaded: ReadonlySet<string>,
  failed: ReadonlySet<string>,
  fallback: string,
): { src: string; visible: boolean } {
  if (failed.has(url)) return { src: fallback, visible: true };
  return { src: url, visible: loaded.has(url) };
}
