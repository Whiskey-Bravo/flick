//! Themes and the animation level: the config's `theme`, `custom_theme` and
//! `animations` turned into what the pages apply. Pure: no Windows calls.
//!
//! A theme is eight colours. The pages set them as CSS variables and derive
//! every other shade from them, so a theme never names more than these.

use serde::{Deserialize, Serialize};

/// The built-in themes, in the order Settings shows them. `custom` uses the
/// config's `[custom_theme]`.
pub const THEMES: [&str; 7] = ["dark", "light", "midnight", "frost", "ember", "glass", "custom"];
/// The animation levels, calmest first.
pub const ANIMATIONS: [&str; 3] = ["minimal", "medium", "high"];

/// A theme's colours as `#rrggbb`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Colors {
    /// The panel.
    pub background: String,
    /// The selected row, inputs and buttons.
    pub surface: String,
    /// The panel's edge and dividers.
    pub border: String,
    /// Titles and typed text.
    pub text: String,
    /// Subtitles, hints and placeholders.
    pub muted: String,
    /// The selection marker, links and focus.
    pub accent: String,
    pub error: String,
    /// Warning text and the armed row.
    pub warning: String,
}

/// The config's `[custom_theme]`: any colour left out takes Dark's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CustomTheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub muted: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

impl CustomTheme {
    /// Each colour with its name, in the order Settings shows them.
    pub fn entries(&self) -> [(&'static str, &Option<String>); 8] {
        [
            ("background", &self.background),
            ("surface", &self.surface),
            ("border", &self.border),
            ("text", &self.text),
            ("muted", &self.muted),
            ("accent", &self.accent),
            ("error", &self.error),
            ("warning", &self.warning),
        ]
    }

    pub fn is_empty(&self) -> bool {
        self.entries().iter().all(|(_, v)| v.is_none())
    }
}

/// What the pages apply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Appearance {
    /// The theme's name (one of [`THEMES`]).
    pub theme: String,
    pub colors: Colors,
    /// The bar is see-through glass; `colors` then tint it.
    pub glass: bool,
    /// One of [`ANIMATIONS`].
    pub animations: String,
}

fn colors(c: [&str; 8]) -> Colors {
    let [background, surface, border, text, muted, accent, error, warning] = c.map(String::from);
    Colors { background, surface, border, text, muted, accent, error, warning }
}

/// A built-in theme's colours; `None` for `custom` and unknown names. Glass
/// carries the colours its see-through panel is tinted and written with.
pub fn builtin(name: &str) -> Option<Colors> {
    Some(colors(match name {
        "dark" => ["#1b1b1f", "#2a2a31", "#2e2e35", "#ececf1", "#a4a4b0", "#6d8bff", "#f0717a", "#f2b441"],
        "light" => ["#f7f7f9", "#e6e7ee", "#d9dae3", "#1c1d22", "#5d5f6c", "#3b5bdb", "#c62f3b", "#8a5a00"],
        "midnight" => ["#000000", "#17181d", "#26272e", "#f2f2f5", "#9a9ba6", "#8aa2ff", "#f0717a", "#f2b441"],
        "frost" => ["#2e3440", "#3b4252", "#434c5e", "#eceff4", "#b4bccb", "#88c0d0", "#f08c95", "#ebcb8b"],
        "ember" => ["#24201c", "#332c25", "#3d352d", "#f1e9df", "#b8ab9b", "#f2a65a", "#f0717a", "#f2c14e"],
        "glass" => ["#14161c", "#ffffff", "#ffffff", "#ffffff", "#d0d3dc", "#9db4ff", "#ff9aa2", "#ffd479"],
        _ => return None,
    }))
}

/// The theme a config value names: empty is the default, Dark.
pub fn theme_name(value: &str) -> &str {
    if value.is_empty() {
        "dark"
    } else {
        value
    }
}

/// The animation level a config value names: empty is the default, Minimal.
pub fn animation_name(value: &str) -> &str {
    if value.is_empty() {
        "minimal"
    } else {
        value
    }
}

/// `#rgb` or `#rrggbb`.
pub fn valid_color(s: &str) -> bool {
    let Some(hex) = s.strip_prefix('#') else { return false };
    matches!(hex.len(), 3 | 6) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `#rgb` or `#rrggbb` as lower-case `#rrggbb`; `None` when it is neither.
pub fn normalize_color(s: &str) -> Option<String> {
    if !valid_color(s) {
        return None;
    }
    let hex = s[1..].to_ascii_lowercase();
    Some(if hex.len() == 3 { format!("#{}", hex.chars().flat_map(|c| [c, c]).collect::<String>()) } else { format!("#{hex}") })
}

/// Checks the three settings; the message names the setting.
pub fn validate(theme: &str, custom: &CustomTheme, animations: &str) -> Result<(), String> {
    if !THEMES.contains(&theme_name(theme)) {
        return Err(format!("theme must be one of {}", THEMES.join(", ")));
    }
    if !ANIMATIONS.contains(&animation_name(animations)) {
        return Err(format!("animations must be one of {}", ANIMATIONS.join(", ")));
    }
    for (name, value) in custom.entries() {
        if let Some(value) = value {
            if !valid_color(value) {
                return Err(format!("custom_theme: {name} must be a colour like #1b1b1f"));
            }
        }
    }
    Ok(())
}

/// The colours of the custom theme: Dark's, with each given colour on top.
pub fn custom_colors(custom: &CustomTheme) -> Colors {
    let mut c = builtin("dark").expect("dark is built in");
    let pick = |slot: &mut String, value: &Option<String>| {
        if let Some(v) = value.as_deref().and_then(normalize_color) {
            *slot = v;
        }
    };
    pick(&mut c.background, &custom.background);
    pick(&mut c.surface, &custom.surface);
    pick(&mut c.border, &custom.border);
    pick(&mut c.text, &custom.text);
    pick(&mut c.muted, &custom.muted);
    pick(&mut c.accent, &custom.accent);
    pick(&mut c.error, &custom.error);
    pick(&mut c.warning, &custom.warning);
    c
}

/// What the pages apply for these settings. Unknown values (a config that
/// did not validate never gets here) fall back to Dark and Minimal.
pub fn resolve(theme: &str, custom: &CustomTheme, animations: &str) -> Appearance {
    let name = theme_name(theme);
    let (name, colors) = match name {
        "custom" => ("custom", custom_colors(custom)),
        other => match builtin(other) {
            Some(colors) => (other, colors),
            None => ("dark", builtin("dark").expect("dark is built in")),
        },
    };
    let animations = animation_name(animations);
    let animations = if ANIMATIONS.contains(&animations) { animations } else { "minimal" };
    Appearance { theme: name.to_string(), glass: name == "glass", colors, animations: animations.to_string() }
}

/// The channels of a `#rrggbb` colour, 0 to 255.
fn channels(color: &str) -> Option<[u8; 3]> {
    let c = normalize_color(color)?;
    let n = u32::from_str_radix(&c[1..], 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

/// WCAG relative luminance of a colour, 0 (black) to 1 (white).
fn luminance(color: &str) -> Option<f64> {
    let linear = |v: u8| {
        let s = f64::from(v) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    let [r, g, b] = channels(color)?;
    Some(0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b))
}

/// WCAG contrast ratio of two colours, 1 (the same) to 21 (black on white).
pub fn contrast(a: &str, b: &str) -> Option<f64> {
    let (a, b) = (luminance(a)?, luminance(b)?);
    let (light, dark) = if a > b { (a, b) } else { (b, a) };
    Some((light + 0.05) / (dark + 0.05))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_settings_are_dark_and_minimal() {
        let a = resolve("", &CustomTheme::default(), "");
        assert_eq!((a.theme.as_str(), a.glass, a.animations.as_str()), ("dark", false, "minimal"));
        // Today's colours, unchanged.
        assert_eq!(a.colors.background, "#1b1b1f");
        assert_eq!(a.colors.surface, "#2a2a31");
        assert_eq!(a.colors.text, "#ececf1");
    }

    #[test]
    fn every_listed_theme_resolves_under_its_own_name() {
        for name in THEMES {
            let a = resolve(name, &CustomTheme::default(), "high");
            assert_eq!(a.theme, name);
            assert_eq!(a.glass, name == "glass");
            assert_eq!(a.animations, "high");
            for c in [&a.colors.background, &a.colors.surface, &a.colors.border, &a.colors.text, &a.colors.muted, &a.colors.accent, &a.colors.error, &a.colors.warning] {
                assert!(valid_color(c) && c.len() == 7, "{name}: {c}");
            }
        }
        assert!(builtin("custom").is_none() && builtin("nope").is_none());
    }

    #[test]
    fn unknown_values_are_refused_and_fall_back() {
        let none = CustomTheme::default();
        assert!(validate("", &none, "").is_ok());
        assert!(validate("frost", &none, "medium").is_ok());
        assert!(validate("neon", &none, "").unwrap_err().starts_with("theme must be one of dark, light"));
        assert!(validate("dark", &none, "wild").unwrap_err().starts_with("animations must be one of minimal"));
        // Names are exact: no other case.
        assert!(validate("Dark", &none, "").is_err());
        let a = resolve("neon", &none, "wild");
        assert_eq!((a.theme.as_str(), a.animations.as_str()), ("dark", "minimal"));
    }

    #[test]
    fn colours_are_three_or_six_hex_digits() {
        for ok in ["#fff", "#FFF", "#1b1b1f", "#ABCDEF"] {
            assert!(valid_color(ok), "{ok}");
        }
        for bad in ["", "#", "fff", "#ffff", "#12345", "#1234567", "#ggg", "red", "rgb(0,0,0)", " #fff"] {
            assert!(!valid_color(bad), "{bad}");
        }
        assert_eq!(normalize_color("#FaB").as_deref(), Some("#ffaabb"));
        assert_eq!(normalize_color("#1B1B1F").as_deref(), Some("#1b1b1f"));
        assert_eq!(normalize_color("blue"), None);
    }

    #[test]
    fn a_custom_theme_fills_the_gaps_with_dark() {
        let custom = CustomTheme { background: Some("#101418".into()), accent: Some("#4FC".into()), ..Default::default() };
        assert!(validate("custom", &custom, "").is_ok());
        let a = resolve("custom", &custom, "");
        assert_eq!(a.theme, "custom");
        assert_eq!((a.colors.background.as_str(), a.colors.accent.as_str()), ("#101418", "#44ffcc"));
        assert_eq!(a.colors.text, "#ececf1", "not given: Dark's");
        // A wrong colour is refused, and never applied.
        let bad = CustomTheme { text: Some("white".into()), ..Default::default() };
        assert_eq!(validate("custom", &bad, "").unwrap_err(), "custom_theme: text must be a colour like #1b1b1f");
        assert_eq!(custom_colors(&bad).text, "#ececf1");
        // The custom colours are checked whichever theme is active.
        assert!(validate("dark", &bad, "").is_err());
        assert!(CustomTheme::default().is_empty() && !custom.is_empty());
    }

    #[test]
    fn contrast_follows_wcag() {
        let near = |a: f64, b: f64| (a - b).abs() < 0.01;
        assert!(near(contrast("#000000", "#ffffff").unwrap(), 21.0));
        assert!(near(contrast("#ffffff", "#000").unwrap(), 21.0));
        assert!(near(contrast("#777777", "#777777").unwrap(), 1.0));
        assert!(near(contrast("#767676", "#ffffff").unwrap(), 4.54));
        assert_eq!(contrast("nope", "#fff"), None);
    }

    #[test]
    fn every_solid_theme_is_readable() {
        for name in ["dark", "light", "midnight", "frost", "ember"] {
            let c = builtin(name).unwrap();
            for (what, color, least) in [
                ("text", &c.text, 7.0),
                ("muted", &c.muted, 4.5),
                ("accent", &c.accent, 3.0),
                ("error", &c.error, 4.5),
                ("warning", &c.warning, 4.5),
            ] {
                let on_panel = contrast(color, &c.background).unwrap();
                assert!(on_panel >= least, "{name}: {what} on the panel is {on_panel:.2}");
            }
            // Text on the selected row.
            let on_row = contrast(&c.text, &c.surface).unwrap();
            assert!(on_row >= 7.0, "{name}: text on the selected row is {on_row:.2}");
            let muted_on_row = contrast(&c.muted, &c.surface).unwrap();
            assert!(muted_on_row >= 4.0, "{name}: muted on the selected row is {muted_on_row:.2}");
        }
    }
}
