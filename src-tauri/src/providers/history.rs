//! The `clear history` row: forgets recent searches and launches.

use std::sync::Arc;

use crate::fuzzy;
use crate::history::History;
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem};

const ID: &str = "clear";
const KEYWORD: &str = "clear history";

pub struct HistoryProvider {
    history: Arc<History>,
}

impl HistoryProvider {
    pub fn new(history: Arc<History>) -> Self {
        Self { history }
    }
}

fn row(score: u32) -> ResultItem {
    ResultItem {
        id: ID.into(),
        provider: "history",
        title: "Clear history".into(),
        subtitle: "Forget recent searches and launches".into(),
        score,
        confirm: true,
        icon: "builtin:history".into(),
        ..Default::default()
    }
}

impl Provider for HistoryProvider {
    fn name(&self) -> &'static str {
        "history"
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if text.is_empty() || !self.history.is_enabled() {
            return Vec::new();
        }
        fuzzy::score(text, KEYWORD).map(row).into_iter().collect()
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        if id != ID {
            return Err("unknown item".into());
        }
        self.history.clear();
        Ok(Outcome::Hide)
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: KEYWORD.into(),
            about: "forget recent searches and launches".into(),
            fill: KEYWORD.into(),
        }]
    }

    /// Never learned or recalled as a recent launch.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    #[test]
    fn clear_history_row_needs_confirm_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let h = Arc::new(History::load(dir.path().join("history.json"), true));
        h.record_search("blender");
        h.record_launch("apps", "x");
        let p = HistoryProvider::new(h.clone());
        let rows = p.query(&q("clear history"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Clear history");
        assert_eq!(rows[0].subtitle, "Forget recent searches and launches");
        assert!(rows[0].confirm);
        assert_eq!(rows[0].icon, "builtin:history");
        assert!(p.rank_key(ID).is_none());
        assert!(!p.query(&q("clear hist")).is_empty());
        assert!(p.query(&q("zzzz")).is_empty());
        assert!(p.query(&q("")).is_empty());
        assert_eq!(p.activate(ID, Modifier::None), Ok(Outcome::Hide));
        assert!(h.searches().is_empty() && h.recents().is_empty());
    }

    #[test]
    fn no_row_while_history_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let h = Arc::new(History::load(dir.path().join("history.json"), false));
        assert!(HistoryProvider::new(h).query(&q("clear history")).is_empty());
    }
}
