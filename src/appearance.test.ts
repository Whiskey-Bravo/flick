import { describe, it, expect } from "vitest";
import {
  DARK,
  DEFAULT_APPEARANCE,
  layoutHeight, menuViewHeight,
  applyAppearance,
  colorsToSave,
  contrast,
  contrastWarnings,
  cssVars,
  draftAfterReload,
  isLight,
  onAccent,
  normalizeColor,
  settingsColors,
  svgDataUrl,
  themedSvg,
  type Appearance,
} from "./appearance";

describe("appearance", () => {
  it("a theme sets the eight colours, with the Settings alias", () => {
    const vars = cssVars(DARK);
    expect(vars["--bg"]).toBe("#1b1b1f");
    expect(vars["--surface"]).toBe("#2a2a31");
    expect(vars["--panel"]).toBe(vars["--surface"]);
    expect(Object.keys(vars).sort()).toEqual(
      ["--accent", "--bg", "--border", "--error", "--muted", "--panel", "--surface", "--text", "--warning"].sort(),
    );
  });

  it("Settings uses the dark colours under Glass, the theme's otherwise", () => {
    const light = { ...DARK, background: "#f7f7f9", text: "#1c1d22" };
    const solid: Appearance = { theme: "light", colors: light, glass: false, acrylic: false, animations: "minimal", tab_completion: false };
    expect(settingsColors(solid)).toBe(light);
    expect(settingsColors({ ...solid, theme: "glass", glass: true })).toBe(DARK);
    expect(settingsColors({ ...solid, theme: "acrylic", acrylic: true })).toBe(DARK);
  });

  it("under Acrylic a menu may use the full bar's height, as the window grows to fit it", () => {
    expect(menuViewHeight(true, 110)).toBe(420);
    expect(menuViewHeight(false, 110)).toBe(110);
    expect(menuViewHeight(false, 460)).toBe(460);
  });

  it("marks the page when Acrylic's live blur is on", () => {
    const root = { style: { setProperty: () => {}, colorScheme: "" }, dataset: {} } as unknown as HTMLElement;
    applyAppearance({ ...DEFAULT_APPEARANCE, theme: "acrylic", acrylic: true }, DARK, root);
    expect(root.dataset.acrylic).toBe("true");
    applyAppearance(DEFAULT_APPEARANCE, DARK, root);
    expect(root.dataset.acrylic).toBe("false");
  });

  it("bundled icons are repainted in the theme's text colour", () => {
    const raw = '<svg><path stroke="#ececf1"/><path stroke="#ECECF1"/><path stroke="#7F77DD"/></svg>';
    expect(themedSvg(raw, "#1c1d22")).toBe('<svg><path stroke="#1c1d22"/><path stroke="#1c1d22"/><path stroke="#7F77DD"/></svg>');
    expect(svgDataUrl("<svg a='#fff'/>")).toBe("data:image/svg+xml,%3Csvg%20a%3D'%23fff'%2F%3E");
  });

  it("contrast follows WCAG", () => {
    expect(contrast("#000000", "#ffffff")).toBeCloseTo(21, 1);
    expect(contrast("#fff", "#000")).toBeCloseTo(21, 1);
    expect(contrast("#767676", "#ffffff")).toBeCloseTo(4.54, 1);
    expect(contrast("#777", "#777777")).toBeCloseTo(1, 5);
    expect(contrast("red", "#fff")).toBeNull();
  });

  it("text on an accent button is dark on a light accent and white on a dark one", () => {
    expect(onAccent("#88c0d0")).toBe("#10131f");
    expect(onAccent("#f2a65a")).toBe("#10131f");
    expect(onAccent("#3b5bdb")).toBe("#ffffff");
  });

  it("knows a light background from a dark one", () => {
    expect(isLight("#f7f7f9")).toBe(true);
    expect(isLight("#1b1b1f")).toBe(false);
    expect(isLight("#2e3440")).toBe(false);
    expect(isLight("nope")).toBe(false);
  });

  it("warns only about colours that are hard to read", () => {
    expect(contrastWarnings(DARK)).toEqual([]);
    expect(contrastWarnings({ ...DARK, text: "#3a3a40" })).toEqual([
      "Text is hard to read on the background.",
      "Text is hard to read on the selected row.",
    ]);
    expect(contrastWarnings({ ...DARK, muted: "#2c2c33" })).toEqual(["Muted text is hard to read on the background."]);
    // A value still being typed is not a colour yet: no warning about it.
    expect(contrastWarnings({ ...DARK, text: "#ec" })).toEqual([]);
  });

  it("normalises colours for the colour input", () => {
    expect(normalizeColor("#FaB")).toBe("#ffaabb");
    expect(normalizeColor(" #1B1B1F ")).toBe("#1b1b1f");
    expect(normalizeColor("#12345")).toBeNull();
    expect(normalizeColor("blue")).toBeNull();
  });
});

describe("custom colours being edited", () => {
  const saved = { ...DARK };
  it("saves the colours that are done; one still being typed keeps its saved value", () => {
    const draft = { ...DARK, background: "#ABC", text: "#12" };
    expect(colorsToSave(draft, saved)).toEqual({ ...DARK, background: "#aabbcc" });
    // Nothing saved yet to fall back on: nothing to save.
    expect(colorsToSave(draft, undefined)).toBeNull();
    expect(colorsToSave({ ...DARK, background: "#ABC" }, undefined)).toEqual({ ...DARK, background: "#aabbcc" });
  });

  it("a reload keeps a value still being typed and takes the rest from the file", () => {
    const draft = { ...DARK, text: "#12", muted: "#ffffff" };
    const after = draftAfterReload({ ...DARK, background: "#000000" }, draft);
    expect(after).toEqual({ ...DARK, background: "#000000", text: "#12" });
    expect(draftAfterReload(undefined, draft)).toBeNull();
    expect(draftAfterReload(DARK, null)).toEqual(DARK);
  });
});

describe("layoutHeight", () => {
  const style = (o: Partial<Record<string, string>>) =>
    ({ height: "64.4px", boxSizing: "border-box", paddingTop: "6px", paddingBottom: "6px", borderTopWidth: "1px", borderBottomWidth: "1px", ...o }) as unknown as CSSStyleDeclaration;
  it("is the laid-out height, with fractions, whatever a transform does", () => {
    expect(layoutHeight(style({}))).toBe(64.4);
  });
  it("adds padding and border when the height is the content's", () => {
    expect(layoutHeight(style({ boxSizing: "content-box", height: "50px" }))).toBe(64);
  });
  it("is 0 for a height that is not a number", () => {
    expect(layoutHeight(style({ height: "auto" }))).toBe(0);
  });
});
