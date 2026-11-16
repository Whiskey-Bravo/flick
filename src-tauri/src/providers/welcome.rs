//! First-start welcome: two rows shown on the very first appearance of the bar.

use std::sync::{Arc, Mutex};

use crate::provider::{Modifier, Outcome, Provider, Query, ResultItem};

/// Reads the number of times the bar has been shown.
pub type Shows = Box<dyn Fn() -> u64 + Send + Sync>;

const HOTKEY_ID: &str = "hotkey";
const HELP_ID: &str = "help";

/// What `arm` recorded: the show it is armed for, and the hotkey (`None` = it
/// could not be registered).
struct Armed {
    show: u64,
    hotkey: Option<String>,
}

pub struct WelcomeProvider {
    shows: Shows,
    armed: Mutex<Option<Armed>>,
}

impl WelcomeProvider {
    pub fn new(shows: Shows) -> Arc<Self> {
        Arc::new(Self { shows, armed: Mutex::new(None) })
    }

    /// Arms the welcome for the next show (the one about to happen). Call
    /// before showing the bar, so the empty query that `bar:shown` triggers
    /// can never arrive first.
    pub fn arm(&self, hotkey: Option<&str>) {
        let armed = Armed { show: (self.shows)() + 1, hotkey: hotkey.map(str::to_string) };
        *self.armed.lock().unwrap_or_else(|e| e.into_inner()) = Some(armed);
    }
}

fn row(id: &str, title: String) -> ResultItem {
    ResultItem {
        id: id.into(),
        provider: "welcome",
        title,
        subtitle: String::new(),
        score: 0,
        confirm: false,
    }
}

impl Provider for WelcomeProvider {
    fn name(&self) -> &'static str {
        "welcome"
    }

    fn query(&self, _q: &Query) -> Vec<ResultItem> {
        Vec::new()
    }

    fn empty_query(&self) -> Vec<ResultItem> {
        let guard = self.armed.lock().unwrap_or_else(|e| e.into_inner());
        let Some(armed) = guard.as_ref().filter(|a| a.show == (self.shows)()) else {
            return Vec::new();
        };
        let first = match &armed.hotkey {
            Some(h) => format!("Press {h} anytime to open Flick"),
            None => "Open Flick from its tray icon until the hotkey works".to_string(),
        };
        vec![row(HOTKEY_ID, first), row(HELP_ID, "Type help to see what it can do".into())]
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        match id {
            HOTKEY_ID => Ok(Outcome::Hide),
            HELP_ID => Ok(Outcome::Fill { text: "help".into() }),
            _ => Err("unknown item".into()),
        }
    }

    /// Welcome rows are never learned or boosted.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn provider() -> (Arc<WelcomeProvider>, Arc<AtomicU64>) {
        let count = Arc::new(AtomicU64::new(0));
        let c = count.clone();
        (WelcomeProvider::new(Box::new(move || c.load(Ordering::SeqCst))), count)
    }

    #[test]
    fn welcome_only_during_first_show() {
        let (w, count) = provider();
        assert!(w.empty_query().is_empty()); // not armed
        w.arm(Some("Alt+Space")); // arms for show 1
        assert!(w.empty_query().is_empty()); // not shown yet
        count.store(1, Ordering::SeqCst);
        let rows = w.empty_query();
        assert_eq!(rows[0].title, "Press Alt+Space anytime to open Flick");
        assert_eq!(rows[1].title, "Type help to see what it can do");
        count.store(2, Ordering::SeqCst);
        assert!(w.empty_query().is_empty());
    }

    #[test]
    fn welcome_without_hotkey_points_to_tray() {
        let (w, count) = provider();
        w.arm(None);
        count.store(1, Ordering::SeqCst);
        assert_eq!(w.empty_query()[0].title, "Open Flick from its tray icon until the hotkey works");
    }

    #[test]
    fn help_row_fills_help() {
        let (w, _) = provider();
        assert_eq!(w.activate("help", Modifier::None), Ok(Outcome::Fill { text: "help".into() }));
        assert_eq!(w.activate("hotkey", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(w.rank_key("help"), None);
        assert!(w.help().is_empty());
    }
}
