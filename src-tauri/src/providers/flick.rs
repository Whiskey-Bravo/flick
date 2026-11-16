//! Flick's own result: "Flick: edit config" opens the config file.

use crate::fuzzy;
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem};

/// Opens the config file: `launch::open_config` in the app, a recorder in tests.
pub type Opener = Box<dyn Fn() -> Result<(), String> + Send + Sync>;

const ID: &str = "config";
const TITLE: &str = "Flick: edit config";
/// The texts the result is found by; the best match counts.
const KEYWORDS: [&str; 2] = ["flick config", "edit config"];

pub struct FlickProvider {
    open: Opener,
}

impl FlickProvider {
    pub fn new(open: Opener) -> Self {
        Self { open }
    }
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
        let Some(score) = KEYWORDS.iter().filter_map(|k| fuzzy::score(text, k)).max() else {
            return Vec::new();
        };
        vec![row(score)]
    }

    fn item(&self, id: &str) -> Option<ResultItem> {
        (id == ID).then(|| row(0))
    }

    /// Opening can block while the shell works; activation runs off the main thread.
    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        if id != ID {
            return Err("unknown item".into());
        }
        (self.open)()?;
        Ok(Outcome::Hide)
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: "flick config".into(),
            about: "open the config file to change the hotkey, file folders and shortcuts".into(),
            fill: "flick config".into(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn provider(result: Result<(), String>) -> (FlickProvider, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let p = FlickProvider::new(Box::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
            result.clone()
        }));
        (p, count)
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    #[test]
    fn found_by_either_keyword() {
        let (p, _) = provider(Ok(()));
        for text in ["flick config", "edit config", "flick", "edit", "config", "fc"] {
            let r = p.query(&q(text));
            assert_eq!(r.len(), 1, "{text}");
            assert_eq!((r[0].id.as_str(), r[0].title.as_str(), r[0].provider), ("config", TITLE, "flick"));
            assert!(!r[0].confirm);
        }
        let best = |t: &str| KEYWORDS.iter().filter_map(|k| fuzzy::score(t, k)).max().unwrap();
        assert_eq!(p.query(&q("edit config"))[0].score, best("edit config"));
        assert_eq!(p.query(&q("flick"))[0].score, fuzzy::score("flick", "flick config").unwrap());
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
            vec![HelpLine {
                example: "flick config".into(),
                about: "open the config file to change the hotkey, file folders and shortcuts".into(),
                fill: "flick config".into(),
            }]
        );
    }
}
