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
