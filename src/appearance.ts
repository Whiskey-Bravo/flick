// The theme and animation level as the pages apply them. Pure helpers plus
// one function that writes them onto a root element.

export type Colors = {
  background: string;
  surface: string;
  border: string;
  text: string;
  muted: string;
  accent: string;
  error: string;
  warning: string;
};

export type Appearance = {
  theme: string;
  colors: Colors;
  glass: boolean;
  animations: string;
};

export const COLOR_NAMES = ["background", "surface", "border", "text", "muted", "accent", "error", "warning"] as const;
export type ColorName = (typeof COLOR_NAMES)[number];

// What the pages look like until the back end answers: today's dark look.
export const DARK: Colors = {
  background: "#1b1b1f",
  surface: "#2a2a31",
  border: "#2e2e35",
  text: "#ececf1",
  muted: "#a4a4b0",
  accent: "#6d8bff",
  error: "#f0717a",
  warning: "#f2b441",
};

export const DEFAULT_APPEARANCE: Appearance = { theme: "dark", colors: DARK, glass: false, animations: "minimal" };

// The CSS variables a theme sets. Every other shade is derived from these in
// the stylesheets. `--panel` is the Settings window's older name for `--surface`.
export function cssVars(c: Colors): Record<string, string> {
  return {
    "--bg": c.background,
    "--surface": c.surface,
    "--panel": c.surface,
    "--border": c.border,
    "--text": c.text,
    "--muted": c.muted,
    "--accent": c.accent,
    "--error": c.error,
    "--warning": c.warning,
  };
}

// The Settings window is never see-through: under Glass it uses the dark look.
export function settingsColors(a: Appearance): Colors {
  return a.glass ? DARK : a.colors;
}

export function applyAppearance(a: Appearance, colors: Colors, root: HTMLElement): void {
  for (const [name, value] of Object.entries(cssVars(colors))) root.style.setProperty(name, value);
  root.dataset.theme = a.theme;
  root.dataset.glass = String(a.glass);
  root.dataset.motion = a.animations;
  // Text on an accent-coloured button: dark on a light accent, white otherwise.
  root.style.setProperty("--on-accent", onAccent(colors.accent));
  // Native controls (selects, checkboxes, scrollbars) follow the background.
  root.style.colorScheme = isLight(colors.background) ? "light" : "dark";
}

// The bundled icons are drawn in the dark theme's text colour: repaint those
// strokes in the theme's, so they stay visible on a light panel.
const ICON_INK = /#ececf1/gi;
export function themedSvg(raw: string, text: string): string {
  return raw.replace(ICON_INK, text);
}
export function svgDataUrl(svg: string): string {
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

function channels(color: string): [number, number, number] | null {
  const m = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(color.trim());
  if (!m) return null;
  const hex = m[1].length === 3 ? [...m[1]].map((c) => c + c).join("") : m[1];
  const n = parseInt(hex, 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

// WCAG relative luminance, 0 (black) to 1 (white); null for a value that is not a colour.
export function luminance(color: string): number | null {
  const c = channels(color);
  if (!c) return null;
  const [r, g, b] = c.map((v) => {
    const s = v / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

// WCAG contrast ratio, 1 (the same) to 21 (black on white).
export function contrast(a: string, b: string): number | null {
  const la = luminance(a);
  const lb = luminance(b);
  if (la === null || lb === null) return null;
  const [light, dark] = la > lb ? [la, lb] : [lb, la];
  return (light + 0.05) / (dark + 0.05);
}

// The text colour that reads on an accent-coloured button.
export function onAccent(accent: string): string {
  return (luminance(accent) ?? 0) > 0.3 ? "#10131f" : "#ffffff";
}

export function isLight(color: string): boolean {
  return (luminance(color) ?? 0) > 0.4;
}

// What is hard to read in a set of colours, as lines for Settings; empty when fine.
export function contrastWarnings(c: Colors): string[] {
  const out: string[] = [];
  const low = (what: string, color: string, on: string, least: number) => {
    const ratio = contrast(color, on);
    if (ratio !== null && ratio < least) out.push(`${what} is hard to read on the background.`);
  };
  low("Text", c.text, c.background, 4.5);
  low("Muted text", c.muted, c.background, 3);
  const onRow = contrast(c.text, c.surface);
  if (onRow !== null && onRow < 4.5) out.push("Text is hard to read on the selected row.");
  return out;
}

// `#rgb` or `#rrggbb` as lower-case `#rrggbb` (what a colour input needs); null otherwise.
export function normalizeColor(color: string): string | null {
  const c = channels(color);
  return c ? `#${c.map((v) => v.toString(16).padStart(2, "0")).join("")}` : null;
}
