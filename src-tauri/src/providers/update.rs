//! The "Update to x.y" row of the empty bar. Reads in-memory state only.

use std::sync::Arc;

use crate::provider::{Modifier, Outcome, Provider, Query, ResultItem};
use crate::updates::{UpdateState, Updates};

const INSTALL_ID: &str = "install";

pub struct UpdateProvider {
    updates: Arc<Updates>,
}

impl UpdateProvider {
    pub fn new(updates: Arc<Updates>) -> Arc<Self> {
        Arc::new(Self { updates })
    }
}

fn row(title: String, subtitle: &str) -> ResultItem {
    ResultItem {
        id: INSTALL_ID.into(),
        provider: "update",
        title,
        subtitle: subtitle.into(),
        score: 0,
        icon: "builtin:update".into(),
        ..Default::default()
    }
}

impl Provider for UpdateProvider {
    fn name(&self) -> &'static str {
        "update"
    }

    /// Text queries never show the row.
    fn query(&self, _q: &Query) -> Vec<ResultItem> {
        Vec::new()
    }

    fn empty_query(&self) -> Vec<ResultItem> {
        match self.updates.visible_state() {
            UpdateState::Idle => Vec::new(),
            UpdateState::Available { version, .. } => {
                vec![row(format!("Update to {version}"), "Enter to install and restart")]
            }
            UpdateState::Downloading { .. } => vec![row("Downloading update…".into(), "")],
            UpdateState::Failed { reason, .. } => {
                vec![row(format!("Update failed: {reason}"), "Enter to try again")]
            }
        }
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        if id != INSTALL_ID {
            return Err("unknown item".into());
        }
        let updates = self.updates.clone();
        std::thread::spawn(move || updates.install());
        Ok(Outcome::Keep)
    }

    /// Never learned, boosted or recorded as a launch (`item` is `None` too, so never pinned).
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updates::tests::{new_available, FakeSource};

    fn provider(source: FakeSource) -> (Arc<UpdateProvider>, Arc<Updates>) {
        let updates = Arc::new(Updates::new(Arc::new(source), Arc::new(|| {})));
        (UpdateProvider::new(updates.clone()), updates)
    }

    fn found() -> FakeSource {
        FakeSource::default().check_returns(Ok(Some(new_available("0.4.0"))))
    }

    #[test]
    fn idle_shows_nothing() {
        let (p, _) = provider(FakeSource::default());
        assert!(p.empty_query().is_empty());
    }

    #[test]
    fn available_row() {
        let (p, u) = provider(found());
        u.check_now().unwrap();
        let rows = p.empty_query();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Update to 0.4.0");
        assert_eq!(rows[0].subtitle, "Enter to install and restart");
        assert_eq!(rows[0].icon, "builtin:update");
        assert_eq!((rows[0].provider, rows[0].id.as_str()), ("update", "install"));
    }

    #[test]
    fn disabled_hides_the_row() {
        let (p, u) = provider(found());
        u.check_now().unwrap();
        u.set_enabled(false);
        assert!(p.empty_query().is_empty());
    }

    #[test]
    fn downloading_row() {
        let (p, u) = provider(found());
        u.check_now().unwrap();
        u.install(); // the fake succeeds; the real source restarts Flick
        let rows = p.empty_query();
        assert_eq!(rows[0].title, "Downloading update…");
        assert_eq!(rows[0].subtitle, "");
    }

    #[test]
    fn failed_row() {
        let (p, u) = provider(found().install_returns(Err("no space".into())));
        u.check_now().unwrap();
        u.install();
        let rows = p.empty_query();
        assert_eq!(rows[0].title, "Update failed: no space");
        assert_eq!(rows[0].subtitle, "Enter to try again");
    }

    #[test]
    fn text_queries_never_show_it_and_nothing_is_pinnable_or_learned() {
        let (p, u) = provider(found());
        u.check_now().unwrap();
        assert!(p.query(&Query { text: "update".into(), scope: None }).is_empty());
        assert!(p.item("install").is_none());
        assert_eq!(p.rank_key("install"), None);
        assert!(p.help().is_empty());
        assert_eq!(p.activate("other", Modifier::None), Err("unknown item".to_string()));
    }

    #[test]
    fn activate_installs_in_the_background_and_keeps_the_bar() {
        let (p, u) = provider(found());
        u.check_now().unwrap();
        assert_eq!(p.activate("install", Modifier::None), Ok(Outcome::Keep));
        for _ in 0..200 {
            if matches!(u.state(), UpdateState::Downloading { .. }) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("install did not start");
    }
}
