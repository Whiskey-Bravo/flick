import { useEffect, useState } from "preact/hooks";
import {
  COLOR_NAMES,
  colorsToSave,
  contrastWarnings,
  draftAfterReload,
  normalizeColor,
  type ColorName,
  type Colors,
} from "../appearance";
import type { CustomTheme, SettingsState, ThemeInfo } from "./api";
import { useSaver } from "./saver";
import "./appearance.css";

const THEME_LABELS: Record<string, string> = {
  dark: "Dark",
  light: "Light",
  midnight: "Midnight",
  frost: "Frost",
  ember: "Ember",
  ocean: "Ocean",
  forest: "Forest",
  grape: "Grape",
  rose: "Rose",
  sand: "Sand",
  mint: "Mint",
  sakura: "Sakura",
  glass: "Glass",
  custom: "Custom",
};

const COLOR_LABELS: Record<ColorName, [string, string]> = {
  background: ["Background", "the bar itself"],
  surface: ["Selected row", "also buttons and boxes"],
  border: ["Border", "the edge and the dividing lines"],
  text: ["Text", "titles and what you type"],
  muted: ["Muted text", "subtitles and hints"],
  accent: ["Accent", "highlights and links"],
  error: ["Error", "error messages"],
  warning: ["Warning", "rows waiting for a second Enter"],
};

const ANIMATIONS: [string, string, string][] = [
  ["minimal", "Minimal", "Nothing moves: the bar and its rows change at once."],
  ["medium", "Medium", "The bar fades in, the highlight glides, the list eases to its new height."],
  ["high", "High", "The bar rises into place, the highlight glides with a little bounce, new rows fade in one after another."],
  ["wobble", "Wobble", "Motion with weight: the highlight and the list carry their speed from one move to the next and settle softly."],
];

/** A small picture of the bar in a theme's colours. */
function Swatch({ colors, glass }: { colors: Colors; glass: boolean }) {
  const row = glass ? "rgba(255, 255, 255, 0.18)" : colors.surface;
  return (
    <span class={glass ? "ap-swatch ap-glass" : "ap-swatch"} style={{ background: glass ? undefined : colors.background, borderColor: colors.border, color: colors.text }}>
      <span class="ap-line" style={{ borderColor: glass ? "rgba(255, 255, 255, 0.2)" : colors.border }}>
        fire
      </span>
      <span class="ap-row" style={{ background: row }}>
        <span class="ap-dot" style={{ background: colors.accent }} />
        Firefox
        <span class="ap-sub" style={{ color: colors.muted }}>
          App
        </span>
      </span>
      <span class="ap-row">
        <span class="ap-dot" style={{ background: colors.muted }} />
        Firewall
        <span class="ap-sub" style={{ color: colors.muted }}>
          Settings
        </span>
      </span>
    </span>
  );
}

/** The custom colours as the config stores them: only what differs from nothing. */
function toCustom(colors: Colors): CustomTheme {
  const out: CustomTheme = {};
  for (const name of COLOR_NAMES) out[name] = colors[name];
  return out;
}

export function Appearance({ state }: { state: SettingsState }) {
  const { config, themes } = state;
  const { errors, save } = useSaver(state);
  const theme = config.theme || "dark";
  const animations = config.animations || "minimal";
  const info = (name: string): ThemeInfo | undefined => themes.find((t) => t.name === name);
  const saved = info("custom")?.colors;

  // The custom colours as edited: typed values may be half-finished, so they
  // are kept as text and saved only once they are colours. A reload keeps them.
  const [draft, setDraft] = useState<Colors | null>(saved ?? null);
  useEffect(() => {
    setDraft((d) => draftAfterReload(saved, d));
  }, [JSON.stringify(saved)]);

  const pickTheme = (name: string) => void save("theme", { key: "theme", value: name === "dark" ? "" : name });

  // A colour still being typed in another field does not hold this one back.
  const saveCustom = (colors: Colors) => {
    const clean = colorsToSave(colors, saved);
    if (clean) void save("custom_theme", { key: "custom_theme", value: toCustom(clean) });
  };
  const setColor = (name: ColorName, value: string, commit: boolean) => {
    if (!draft) return;
    const next = { ...draft, [name]: value };
    setDraft(next);
    if (commit) saveCustom(next);
  };
  const startFrom = (name: string) => {
    const from = info(name)?.colors;
    if (!from) return;
    setDraft(from);
    saveCustom(from);
  };

  const warnings = draft ? contrastWarnings(draft) : [];

  return (
    <>
      <div class="field ap-wide">
        <label>Theme</label>
        <div class="ap-themes" role="radiogroup" aria-label="Theme">
          {themes.map((t) => (
            <button
              key={t.name}
              role="radio"
              aria-checked={t.name === theme}
              class={t.name === theme ? "ap-theme active" : "ap-theme"}
              onClick={() => pickTheme(t.name)}
            >
              <Swatch colors={t.colors} glass={t.glass} />
              <span class="ap-name">{THEME_LABELS[t.name] ?? t.name}</span>
            </button>
          ))}
        </div>
        {theme === "glass" && <p class="hint">Glass shows what is behind the bar. This window stays dark.</p>}
        {errors.theme && <p class="error">{errors.theme}</p>}
      </div>

      {theme === "custom" && draft && (
        <div class="field ap-wide">
          <label>Your colours</label>
          <div class="row">
            <span>Start from</span>
            <select value="" onChange={(e) => startFrom(e.currentTarget.value)}>
              <option value="">Choose a theme…</option>
              {themes
                .filter((t) => t.name !== "custom" && !t.glass)
                .map((t) => (
                  <option key={t.name} value={t.name}>
                    {THEME_LABELS[t.name] ?? t.name}
                  </option>
                ))}
            </select>
          </div>
          <div class="ap-colors">
            {COLOR_NAMES.map((name) => {
              const value = draft[name];
              const valid = normalizeColor(value);
              return (
                <div key={name} class="ap-color">
                  <input
                    type="color"
                    aria-label={COLOR_LABELS[name][0]}
                    value={valid ?? "#000000"}
                    onInput={(e) => setColor(name, e.currentTarget.value, false)}
                    onChange={(e) => setColor(name, e.currentTarget.value, true)}
                  />
                  <input
                    class={valid ? "ap-hex" : "ap-hex invalid"}
                    aria-label={`${COLOR_LABELS[name][0]} as a hex value`}
                    value={value}
                    maxLength={7}
                    spellcheck={false}
                    onInput={(e) => setColor(name, e.currentTarget.value, false)}
                    onChange={(e) => setColor(name, e.currentTarget.value, true)}
                  />
                  <span class="ap-what">
                    <span>{COLOR_LABELS[name][0]}</span>
                    <small>{COLOR_LABELS[name][1]}</small>
                  </span>
                </div>
              );
            })}
          </div>
          {warnings.map((w) => (
            <p key={w} class="ap-warning">
              {w}
            </p>
          ))}
          {errors.custom_theme && <p class="error">{errors.custom_theme}</p>}
        </div>
      )}

      <div class="field ap-wide">
        <label>Animations</label>
        <div role="radiogroup" aria-label="Animations">
          {ANIMATIONS.map(([value, label, what]) => (
            <label key={value} class="check ap-motion">
              <input
                type="radio"
                name="animations"
                checked={animations === value}
                onChange={() => void save("animations", { key: "animations", value: value === "minimal" ? "" : value })}
              />
              <span>
                {label}
                <small>{what}</small>
              </span>
            </label>
          ))}
        </div>
        <p class="hint">With Windows' animation effects switched off, nothing moves whatever is chosen here.</p>
        {errors.animations && <p class="error">{errors.animations}</p>}
      </div>
    </>
  );
}
