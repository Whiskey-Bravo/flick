//! Flick's own results: "Flick: edit config" opens the config file, "Flick: settings"
//! opens the Settings window.

use crate::fuzzy;
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem};

/// Opens the config file (`launch::open_config`) or the Settings window; recorders in tests.
pub type Opener = Box<dyn Fn() -> Result<(), String> + Send + Sync>;

const ID: &str = "config";
const TITLE: &str = "Flick: edit config";
/// The texts the result is found by; the best match counts.
const KEYWORDS: [&str; 2] = ["flick config", "edit config"];

const SETTINGS_ID: &str = "settings";
const SETTINGS_TITLE: &str = "Flick: settings";
const SETTINGS_KEYWORDS: [&str; 4] = ["settings", "flick settings", "preferences", "options"];

pub struct FlickProvider {
    open: Opener,
    open_settings: Opener,
}

impl FlickProvider {
    pub fn new(open: Opener, open_settings: Opener) -> Self {
        Self { open, open_settings }
    }
}

fn settings_row(score: u32) -> ResultItem {
    ResultItem {
        id: SETTINGS_ID.into(),
        provider: "flick",
        title: SETTINGS_TITLE.into(),
        subtitle: "change the hotkey, folders, shortcuts and more".into(),
        score,
        icon: "builtin:flick".into(),
        pinnable: true,
        ..Default::default()
    }
}

fn best(text: &str, keywords: &[&str]) -> Option<u32> {
    keywords.iter().filter_map(|k| fuzzy::score(text, k)).max()
}

fn row(score: u32) -> ResultItem {
    ResultItem {
        id: ID.into(),
        provider: "flick",
        title: TITLE.into(),
        subtitle: "open the config file".into(),
        score,
        icon: "builtin:flick".into(),
        pinnable: true,
        ..Default::default()
    }
}

impl Provider for FlickProvider {
    fn name(&self) -> &'static str {
        "flick"
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        let mut rows = Vec::new();
        if let Some(score) = best(text, &KEYWORDS) {
            rows.push(row(score));
        }
        if let Some(score) = best(text, &SETTINGS_KEYWORDS) {
            // Exactly one of its keywords: always first, above an app of the same
            // name (Windows Settings) however often that was picked, so the
            // advertised `settings` opens this window.
            let mut item = settings_row(score);
            item.top = SETTINGS_KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(text));
            rows.push(item);
        }
        rows
    }

    fn item(&self, id: &str) -> Option<ResultItem> {
        match id {
            ID => Some(row(0)),
            SETTINGS_ID => Some(settings_row(0)),
            _ => None,
        }
    }

    /// Opening can block while the shell works; activation runs off the main thread.
    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        match id {
            ID => (self.open)()?,
            SETTINGS_ID => (self.open_settings)()?,
            _ => return Err("unknown item".into()),
        }
        Ok(Outcome::Hide)
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![
            HelpLine {
                example: "flick config".into(),
                about: "open the config file to change the hotkey, file folders and shortcuts".into(),
                fill: "flick config".into(),
            },
            HelpLine {
                example: "settings".into(),
                about: "change the hotkey, folders, shortcuts and more".into(),
                fill: "settings".into(),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn provider(result: Result<(), String>) -> (FlickProvider, Arc<AtomicUsize>) {
        let (p, count, _) = provider_with_settings(result);
        (p, count)
    }

    /// Also returns the counter of the settings opener.
    fn provider_with_settings(result: Result<(), String>) -> (FlickProvider, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let settings = Arc::new(AtomicUsize::new(0));
        let s = settings.clone();
        let p = FlickProvider::new(
            Box::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
                result.clone()
            }),
            Box::new(move || {
                s.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        );
        (p, count, settings)
    }

    #[test]
    fn settings_row_is_found_by_its_keywords() {
        let (p, _) = provider(Ok(()));
        for text in ["settings", "preferences", "options", "flick settings"] {
            let r = p.query(&q(text));
            let row = r.iter().find(|i| i.id == "settings").unwrap_or_else(|| panic!("{text}"));
            assert_eq!((row.title.as_str(), row.provider, row.icon.as_str()), ("Flick: settings", "flick", "builtin:flick"));
            assert_eq!(row.subtitle, "change the hotkey, folders, shortcuts and more");
            assert!(row.pinnable);
        }
        assert_eq!(p.item("settings").unwrap().id, "settings");
    }

    #[test]
    fn only_an_exact_keyword_puts_settings_on_top() {
        let (p, _) = provider(Ok(()));
        let top = |t: &str| p.query(&q(t)).into_iter().find(|i| i.id == "settings").unwrap().top;
        assert!(top("settings") && top("Settings") && top("options"));
        assert!(!top("setting"));
    }

    #[test]
    fn activating_settings_opens_and_hides() {
        let (p, config, settings) = provider_with_settings(Ok(()));
        assert_eq!(p.activate("settings", Modifier::None), Ok(Outcome::Hide));
        assert_eq!((config.load(Ordering::SeqCst), settings.load(Ordering::SeqCst)), (0, 1));
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    #[test]
    fn found_by_either_keyword() {
        let (p, _) = provider(Ok(()));
        let config_row = |text: &str| {
            let r = p.query(&q(text));
            r.into_iter().find(|i| i.id == "config").unwrap_or_else(|| panic!("{text}"))
        };
        for text in ["flick config", "edit config", "flick", "edit", "config", "fc"] {
            let row = config_row(text);
            assert_eq!((row.title.as_str(), row.provider), (TITLE, "flick"), "{text}");
            assert!(!row.confirm);
        }
        let best = |t: &str| KEYWORDS.iter().filter_map(|k| fuzzy::score(t, k)).max().unwrap();
        assert_eq!(config_row("edit config").score, best("edit config"));
        assert_eq!(config_row("flick").score, fuzzy::score("flick", "flick config").unwrap());
    }

    #[test]
    fn unrelated_or_empty_text_gives_nothing() {
        let (p, _) = provider(Ok(()));
        assert!(p.query(&q("")).is_empty());
        assert!(p.query(&q("   ")).is_empty());
        assert!(p.query(&q("bluetooth")).is_empty());
    }

    #[test]
    fn activate_opens_the_config_and_hides() {
        let (p, count) = provider(Ok(()));
        assert_eq!(p.activate("config", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(p.activate("other", Modifier::None), Err("unknown item".to_string()));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn open_error_is_returned() {
        let (p, _) = provider(Err("could not open the config".into()));
        assert_eq!(p.activate("config", Modifier::None), Err("could not open the config".to_string()));
    }

    #[test]
    fn help_line() {
        let (p, _) = provider(Ok(()));
        assert_eq!(
            p.help(),
            vec![
                HelpLine {
                    example: "flick config".into(),
                    about: "open the config file to change the hotkey, file folders and shortcuts".into(),
                    fill: "flick config".into(),
                },
                HelpLine {
                    example: "settings".into(),
                    about: "change the hotkey, folders, shortcuts and more".into(),
                    fill: "settings".into(),
                },
            ]
        );
    }
}
