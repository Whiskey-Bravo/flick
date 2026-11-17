//! `snip` and `color`: rows that hide the bar, then snip a region of the
//! screen or open the colour picker (run by the commands after the hide).

use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem, Then};

/// Opens the Windows snipping overlay; the snip lands on the clipboard.
#[allow(dead_code)] // until Task 8 runs the snip after the bar hides
pub const SNIP_URI: &str = "ms-screenclip:";

/// One fixed row for the exact words, or nothing.
#[allow(dead_code)] // until Task 8 registers the providers
fn row(text: &str, words: &[&str], provider: &'static str, title: &str, subtitle: &str, icon: &str) -> Vec<ResultItem> {
    let text = text.trim();
    if !words.iter().any(|w| w.eq_ignore_ascii_case(text)) {
        return Vec::new();
    }
    vec![ResultItem {
        id: provider.to_string(),
        provider,
        title: title.to_string(),
        subtitle: subtitle.to_string(),
        score: 1000,
        icon: icon.to_string(),
        top: true,
        ..Default::default()
    }]
}

/// `snip` / `screenshot`.
#[allow(dead_code)] // until Task 8 registers it
pub struct SnipProvider;

impl Provider for SnipProvider {
    fn name(&self) -> &'static str {
        "snip"
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        row(&q.text, &["snip", "screenshot"], "snip", "Snip a region", "Copies it to the clipboard", "builtin:snip")
    }

    fn activate(&self, _id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        Ok(Outcome::HideThen { then: Then::Snip })
    }

    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: "snip".into(),
            about: "snip a region of the screen to the clipboard".into(),
            fill: "snip".into(),
        }]
    }
}

/// `color` / `colour` / `picker`.
#[allow(dead_code)] // until Task 8 registers it
pub struct ColorProvider;

impl Provider for ColorProvider {
    fn name(&self) -> &'static str {
        "color"
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        row(&q.text, &["color", "colour", "picker"], "color", "Pick a colour", "Copies it as #RRGGBB", "builtin:color")
    }

    fn activate(&self, _id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        Ok(Outcome::HideThen { then: Then::PickColor })
    }

    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: "color".into(),
            about: "pick a colour on screen and copy it as #RRGGBB".into(),
            fill: "color".into(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    fn one(rows: Vec<ResultItem>, title: &str, subtitle: &str, icon: &str) {
        assert_eq!(rows.len(), 1, "{:?}", rows.iter().map(|r| &r.title).collect::<Vec<_>>());
        let r = &rows[0];
        assert_eq!((r.title.as_str(), r.subtitle.as_str(), r.icon.as_str()), (title, subtitle, icon));
        assert!(r.top && r.score == 1000 && r.actions.is_empty() && !r.pinnable && r.panel.is_empty());
    }

    #[test]
    fn words_answer_exactly() {
        for t in ["snip", "Screenshot", " snip "] {
            one(SnipProvider.query(&q(t)), "Snip a region", "Copies it to the clipboard", "builtin:snip");
        }
        for t in ["sni", "snipping", "snip x", "", "color"] {
            assert!(SnipProvider.query(&q(t)).is_empty(), "{t}");
        }
        for t in ["color", "COLOUR", "picker"] {
            one(ColorProvider.query(&q(t)), "Pick a colour", "Copies it as #RRGGBB", "builtin:color");
        }
        for t in ["colors", "col", "snip"] {
            assert!(ColorProvider.query(&q(t)).is_empty(), "{t}");
        }
        assert_eq!((SnipProvider.name(), ColorProvider.name()), ("snip", "color"));
        assert!(SnipProvider.route().is_none() && ColorProvider.route().is_none());
    }

    #[test]
    fn enter_hides_then_snips_or_picks() {
        assert_eq!(SnipProvider.activate("snip", Modifier::None), Ok(Outcome::HideThen { then: Then::Snip }));
        assert_eq!(ColorProvider.activate("color", Modifier::None), Ok(Outcome::HideThen { then: Then::PickColor }));
        assert_eq!(SNIP_URI, "ms-screenclip:");
    }

    #[test]
    fn rows_are_never_learned() {
        assert_eq!(SnipProvider.rank_key("snip"), None);
        assert_eq!(ColorProvider.rank_key("color"), None);
        assert!(SnipProvider.item("snip").is_none() && ColorProvider.item("color").is_none());
        assert!(SnipProvider.empty_query().is_empty() && ColorProvider.empty_query().is_empty());
    }

    #[test]
    fn help_lines() {
        let s = SnipProvider.help();
        assert_eq!((s[0].example.as_str(), s[0].about.as_str(), s[0].fill.as_str()), ("snip", "snip a region of the screen to the clipboard", "snip"));
        let c = ColorProvider.help();
        assert_eq!((c[0].example.as_str(), c[0].about.as_str(), c[0].fill.as_str()), ("color", "pick a colour on screen and copy it as #RRGGBB", "color"));
    }
}
