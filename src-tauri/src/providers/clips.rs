//! `v`: Windows' clipboard history, searchable. The history is read on a
//! [`Loader`] thread and held in memory until [`ClipsProvider::forget`];
//! nothing is stored or logged.
#![cfg_attr(not(test), allow(dead_code))] // until the provider is registered (Task 8)

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::cliphistory::{ClipHistory, History, HistoryError, SetError};
use crate::dirs::{ago, one_line, Clock, Load, Loader, NEVER_STALE, SLOW};
use crate::fuzzy::{normalize, score_prenormalized};
use crate::launch::Launcher;
use crate::provider::{Action, HelpLine, Modifier, Outcome, Provider, Query, ResultItem, Route, Then};

pub const CLIPBOARD_SETTINGS: &str = "ms-settings:clipboard";
/// Characters of the text shown as the row title.
pub const MAX_TITLE: usize = 120;
/// Characters of each item that typing matches against.
pub const MATCH_CHARS: usize = 10_000;

const READING: &str = "Reading clipboard history\u{2026}";
const CANT_READ: &str = "Can't read clipboard history";
const NOTHING: &str = "Nothing copied yet";
const OFF_ID: &str = "off";
const OPEN_ID: &str = "open-history";
const GONE: &str = "That item is gone from clipboard history";
const COPY_FAILED: &str = "could not copy to the clipboard";
const ICON: &str = "builtin:clipboard";

/// One history item as read, with what every query needs worked out once.
struct Entry {
    id: String,
    title: String,
    /// `normalize` of the first `MATCH_CHARS` characters.
    matched: String,
    copied: SystemTime,
    pinned: bool,
}

/// The outcome of one read, held until `forget`.
enum Read {
    Off,
    Items(Vec<Entry>),
    Failed(HistoryError),
}

fn prepare(read: Result<History, HistoryError>) -> Read {
    match read {
        Ok(History::Off) => Read::Off,
        Ok(History::Items(clips)) => Read::Items(
            clips
                .into_iter()
                .map(|c| {
                    let head: String = c.text.chars().take(MATCH_CHARS).collect();
                    Entry {
                        title: one_line(&c.text, MAX_TITLE),
                        matched: normalize(&head),
                        id: c.id,
                        copied: c.copied,
                        pinned: c.pinned,
                    }
                })
                .collect(),
        ),
        Err(e) => Read::Failed(e),
    }
}

pub struct ClipsProvider {
    loader: Arc<Loader<Read>>,
    history: Arc<dyn ClipHistory>,
    launcher: Arc<dyn Launcher>,
    now: Clock,
}

impl ClipsProvider {
    pub fn new(
        history: Arc<dyn ClipHistory>,
        launcher: Arc<dyn Launcher>,
        now: Clock,
        on_ready: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self::build(history, launcher, now, on_ready, SLOW)
    }

    #[cfg(test)]
    pub fn with_slow(
        history: Arc<dyn ClipHistory>,
        launcher: Arc<dyn Launcher>,
        now: Clock,
        on_ready: Arc<dyn Fn() + Send + Sync>,
        slow: Duration,
    ) -> Self {
        Self::build(history, launcher, now, on_ready, slow)
    }

    fn build(
        history: Arc<dyn ClipHistory>,
        launcher: Arc<dyn Launcher>,
        now: Clock,
        on_ready: Arc<dyn Fn() + Send + Sync>,
        slow: Duration,
    ) -> Self {
        Self { loader: Loader::new(on_ready, NEVER_STALE, slow), history, launcher, now }
    }

    /// Drops the items read and the history's held items (the bar hid).
    pub fn forget(&self) {
        self.loader.clear();
        self.history.forget();
    }

    /// Puts the item the row was built from on the clipboard.
    fn copy(&self, id: &str) -> Result<(), String> {
        self.history.set_content(id).map_err(|e| match e {
            SetError::Gone => GONE.to_string(),
            SetError::Failed => COPY_FAILED.to_string(),
        })
    }
}

fn inert(title: &str) -> ResultItem {
    ResultItem {
        id: format!("inert:{title}"),
        provider: "clips",
        title: title.to_string(),
        icon: ICON.into(),
        ..Default::default()
    }
}

fn row(id: &str, title: &str, subtitle: &str) -> ResultItem {
    ResultItem {
        id: id.to_string(),
        provider: "clips",
        title: title.to_string(),
        subtitle: subtitle.to_string(),
        icon: ICON.into(),
        ..Default::default()
    }
}

impl Provider for ClipsProvider {
    fn name(&self) -> &'static str {
        "clips"
    }

    /// Memory only: the history is read on a loader thread, once until `forget`.
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let history = Arc::clone(&self.history);
        let read = match self.loader.get("history", move || Ok(prepare(history.read()))) {
            Load::Pending => return vec![inert(READING)],
            Load::Ready(read) => read,
            Load::NotFound | Load::Denied | Load::Slow => return vec![inert(CANT_READ)],
        };
        let entries = match &*read {
            Read::Off => return vec![row(OFF_ID, "Clipboard history is off", "Turn it on in Settings")],
            Read::Failed(HistoryError::Unsupported) => return vec![row(OPEN_ID, "Open clipboard history", "Win+V")],
            Read::Failed(HistoryError::Failed) => return vec![inert(CANT_READ)],
            Read::Items(entries) if entries.is_empty() => return vec![inert(NOTHING)],
            Read::Items(entries) => entries,
        };
        let needle = normalize(q.text.trim());
        let now = (self.now)();
        entries
            .iter()
            .filter(|e| needle.is_empty() || score_prenormalized(&needle, &e.matched).is_some())
            .enumerate()
            .map(|(i, e)| {
                let mut subtitle = ago(now, e.copied);
                if e.pinned {
                    subtitle.push_str(" \u{00B7} Pinned");
                }
                ResultItem {
                    score: 1000u32.saturating_sub(i as u32),
                    actions: vec![Action::Copy],
                    ..row(&format!("clip:{}", e.id), &e.title, &subtitle)
                }
            })
            .collect()
    }

    fn activate(&self, id: &str, modifier: Modifier) -> Result<Outcome, String> {
        if let Some(clip) = id.strip_prefix("clip:") {
            self.copy(clip)?;
            return Ok(match modifier {
                Modifier::Ctrl => Outcome::Hide,
                Modifier::None | Modifier::CtrlShift => Outcome::HideThen { then: Then::Paste },
            });
        }
        match id {
            OFF_ID => {
                self.launcher.open(CLIPBOARD_SETTINGS)?;
                Ok(Outcome::Hide)
            }
            OPEN_ID => Ok(Outcome::HideThen { then: Then::ClipboardHistory }),
            _ => Ok(Outcome::Keep),
        }
    }

    fn act(&self, id: &str, action: Action) -> Result<Outcome, String> {
        match (id.strip_prefix("clip:"), action) {
            (Some(clip), Action::Copy) => {
                self.copy(clip)?;
                Ok(Outcome::Hide)
            }
            _ => Err("can't do that here".to_string()),
        }
    }

    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn route(&self) -> Option<Route> {
        Some(Route { prefix: "v", needs_space: true })
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: "v / v invoice".into(),
            about: "search your clipboard history; Enter pastes the item".into(),
            fill: "v".into(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cliphistory::testing::FakeHistory;
    use crate::cliphistory::{Clip, History, HistoryError, SetError};
    use crate::launch::testing::RecordingLauncher;
    use crate::provider::{Action, Then};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::Mutex;
    use std::thread::ThreadId;
    use std::time::{Instant, UNIX_EPOCH};

    const NOW: u64 = 1_000_000;

    /// Delegates to a fake and records the thread of every read.
    struct Threads {
        fake: Arc<FakeHistory>,
        read_on: Mutex<Vec<ThreadId>>,
    }

    impl ClipHistory for Threads {
        fn read(&self) -> Result<History, HistoryError> {
            self.read_on.lock().unwrap().push(std::thread::current().id());
            self.fake.read()
        }
        fn set_content(&self, id: &str) -> Result<(), SetError> {
            self.fake.set_content(id)
        }
        fn forget(&self) {
            self.fake.forget()
        }
    }

    struct V {
        p: ClipsProvider,
        rx: Receiver<()>,
        fake: Arc<FakeHistory>,
        launcher: Arc<RecordingLauncher>,
    }

    fn clock() -> Clock {
        Arc::new(|| UNIX_EPOCH + Duration::from_secs(NOW))
    }

    fn signal() -> (Arc<dyn Fn() + Send + Sync>, Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        (
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
            rx,
        )
    }

    fn v(result: Result<History, HistoryError>) -> V {
        let fake = Arc::new(FakeHistory::new());
        fake.set(result);
        let launcher = Arc::new(RecordingLauncher::default());
        let (on_ready, rx) = signal();
        let p = ClipsProvider::new(fake.clone(), launcher.clone(), clock(), on_ready);
        V { p, rx, fake, launcher }
    }

    fn clip(id: &str, text: &str, secs_ago: u64, pinned: bool) -> Clip {
        Clip {
            id: id.to_string(),
            text: text.to_string(),
            copied: UNIX_EPOCH + Duration::from_secs(NOW - secs_ago),
            pinned,
        }
    }

    fn items(clips: Vec<Clip>) -> Result<History, HistoryError> {
        Ok(History::Items(clips))
    }

    /// What follows `v` in the bar (the engine strips the prefix).
    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    /// The rows once read: waits out a first `Reading clipboard history…`.
    fn ready(t: &V, text: &str) -> Vec<ResultItem> {
        let rows = t.p.query(&q(text));
        if rows.first().is_some_and(|r| r.title == "Reading clipboard history\u{2026}") {
            t.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready within 1 s");
            return t.p.query(&q(text));
        }
        rows
    }

    fn titles(rows: &[ResultItem]) -> Vec<String> {
        rows.iter().map(|r| r.title.clone()).collect()
    }

    fn ids(rows: &[ResultItem]) -> Vec<String> {
        rows.iter().map(|r| r.id.clone()).collect()
    }

    #[test]
    fn query_never_waits_for_the_history() {
        let fake = Arc::new(FakeHistory::new());
        fake.set(items(vec![clip("a", "alpha", 10, false)]));
        fake.block();
        let threads = Arc::new(Threads { fake: fake.clone(), read_on: Mutex::new(Vec::new()) });
        let (on_ready, _rx) = signal();
        let p = ClipsProvider::with_slow(
            threads.clone(),
            Arc::new(RecordingLauncher::default()),
            clock(),
            on_ready,
            Duration::from_millis(50),
        );
        let started = Instant::now();
        let rows = p.query(&q(""));
        assert!(started.elapsed() < Duration::from_millis(20), "{:?}", started.elapsed());
        assert_eq!(titles(&rows), ["Reading clipboard history\u{2026}"]);
        let here = std::thread::current().id();
        assert!(!threads.read_on.lock().unwrap().contains(&here), "no read on the calling thread");
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(titles(&p.query(&q(""))), ["Can't read clipboard history"]);
        assert_eq!(fake.reads(), 1);
        assert!(!threads.read_on.lock().unwrap().contains(&here));
        fake.release();
    }

    #[test]
    fn items_are_listed_newest_first_with_age() {
        let long = "x".repeat(200);
        let t = v(items(vec![
            clip("1", "a\r\nb", 5 * 60, false),
            clip("2", "pinned one", 2 * 3600, true),
            clip("3", &long, 3 * 86_400, false),
        ]));
        let rows = ready(&t, "");
        assert_eq!(ids(&rows), ["clip:1", "clip:2", "clip:3"]);
        assert_eq!(rows[0].title, "a\u{23CE}b");
        assert_eq!(rows[0].subtitle, "5 min ago");
        assert_eq!(rows[1].subtitle, "2 h ago \u{00B7} Pinned");
        assert_eq!(rows[2].title, format!("{}\u{2026}", "x".repeat(120)));
        assert!(rows.windows(2).all(|w| w[0].score > w[1].score), "scores strictly decrease");
        for row in &rows {
            assert_eq!(row.provider, "clips");
            assert_eq!(row.icon, "builtin:clipboard");
            assert_eq!(row.actions, [Action::Copy]);
        }
    }

    #[test]
    fn typing_filters_on_the_full_text() {
        let deep = format!("{} needle", "y".repeat(300));
        let t = v(items(vec![
            clip("1", "needle first", 10, false),
            clip("2", "nothing here", 20, false),
            clip("3", &deep, 30, false),
            clip("4", "zzz", 40, false),
        ]));
        ready(&t, "");
        let rows = t.p.query(&q("needle"));
        assert_eq!(ids(&rows), ["clip:1", "clip:3"]);
        assert!(rows[0].score > rows[1].score);
        assert!(!rows[1].title.contains("needle"), "the match is past the title");
    }

    #[test]
    fn a_long_item_filters_without_rereading() {
        let clips = (0..5).map(|i| clip(&i.to_string(), &"ab ".repeat(200_000 / 3), i * 10, false)).collect();
        let t = v(items(clips));
        assert_eq!(ready(&t, "").len(), 5);
        for n in 1..=10 {
            let typed = "x".repeat(n);
            let started = Instant::now();
            let rows = t.p.query(&q(&typed));
            assert!(started.elapsed() < Duration::from_millis(50), "{typed}: {:?}", started.elapsed());
            assert_eq!(titles(&rows), Vec::<String>::new(), "no x anywhere");
        }
        assert_eq!(t.fake.reads(), 1);
    }

    #[test]
    fn history_off_empty_failed_and_unsupported_say_so() {
        let t = v(Ok(History::Off));
        let rows = ready(&t, "");
        assert_eq!(titles(&rows), ["Clipboard history is off"]);
        assert_eq!(rows[0].subtitle, "Turn it on in Settings");
        assert_eq!(rows[0].id, "off");
        assert_eq!(t.p.activate("off", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(t.launcher.calls(), ["open ms-settings:clipboard"]);

        let t = v(items(vec![]));
        let rows = ready(&t, "");
        assert_eq!(titles(&rows), ["Nothing copied yet"]);
        assert_eq!(rows[0].id, "inert:Nothing copied yet");
        assert_eq!(t.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Keep));

        let t = v(Err(HistoryError::Failed));
        let rows = ready(&t, "");
        assert_eq!(titles(&rows), ["Can't read clipboard history"]);
        assert_eq!(rows[0].id, "inert:Can't read clipboard history");
        assert_eq!(t.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Keep));

        let t = v(Err(HistoryError::Unsupported));
        let rows = ready(&t, "");
        assert_eq!(titles(&rows), ["Open clipboard history"]);
        assert_eq!(rows[0].subtitle, "Win+V");
        assert_eq!(rows[0].id, "open-history");
        assert_eq!(
            t.p.activate("open-history", Modifier::None),
            Ok(Outcome::HideThen { then: Then::ClipboardHistory })
        );
        assert!(t.launcher.calls().is_empty());
        assert!(t.fake.set_calls().is_empty());
    }

    #[test]
    fn enter_sets_the_item_read_not_the_one_now_in_its_place() {
        let t = v(items(vec![clip("b", "bee", 10, false), clip("a", "ay", 20, false)]));
        let rows = ready(&t, "");
        t.fake.set(items(vec![clip("c", "sea", 1, false), clip("b", "bee", 10, false), clip("a", "ay", 20, false)]));
        assert_eq!(t.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::HideThen { then: Then::Paste }));
        assert_eq!(t.fake.set_calls(), ["b"]);
        assert_eq!(t.p.activate(&rows[1].id, Modifier::CtrlShift), Ok(Outcome::HideThen { then: Then::Paste }));
        assert_eq!(t.fake.set_calls(), ["b", "a"]);
    }

    #[test]
    fn ctrl_enter_and_copy_only_copy() {
        let t = v(items(vec![clip("b", "bee", 10, false)]));
        let rows = ready(&t, "");
        assert_eq!(t.p.activate(&rows[0].id, Modifier::Ctrl), Ok(Outcome::Hide));
        assert_eq!(t.fake.set_calls(), ["b"]);
        assert_eq!(t.p.act(&rows[0].id, Action::Copy), Ok(Outcome::Hide));
        assert_eq!(t.fake.set_calls(), ["b", "b"]);
    }

    #[test]
    fn a_gone_item_says_so() {
        let t = v(items(vec![clip("b", "bee", 10, false)]));
        let rows = ready(&t, "");
        t.fake.mark_gone("b");
        let gone = Err("That item is gone from clipboard history".to_string());
        assert_eq!(t.p.activate(&rows[0].id, Modifier::None), gone);
        assert_eq!(t.p.activate(&rows[0].id, Modifier::Ctrl), gone);
        assert_eq!(t.p.act(&rows[0].id, Action::Copy), gone);
    }

    #[test]
    fn a_failed_set_says_it_could_not_copy() {
        struct Failing;
        impl ClipHistory for Failing {
            fn read(&self) -> Result<History, HistoryError> {
                Ok(History::Items(vec![]))
            }
            fn set_content(&self, _id: &str) -> Result<(), SetError> {
                Err(SetError::Failed)
            }
            fn forget(&self) {}
        }
        let (on_ready, _rx) = signal();
        let p = ClipsProvider::new(Arc::new(Failing), Arc::new(RecordingLauncher::default()), clock(), on_ready);
        assert_eq!(p.activate("clip:b", Modifier::None), Err("could not copy to the clipboard".to_string()));
    }

    #[test]
    fn forget_drops_everything() {
        let t = v(items(vec![clip("b", "bee", 10, false)]));
        assert_eq!(titles(&ready(&t, "")), ["bee"]);
        assert_eq!(titles(&t.p.query(&q(""))), ["bee"]);
        assert_eq!(t.fake.reads(), 1);
        t.fake.set(items(vec![clip("c", "sea", 1, false)]));
        t.p.forget();
        assert_eq!(t.fake.forgets(), 1);
        assert_eq!(titles(&ready(&t, "")), ["sea"]);
        assert_eq!(t.fake.reads(), 2);
    }

    #[test]
    fn rows_are_never_learned() {
        let t = v(items(vec![clip("b", "bee", 10, false)]));
        let row = &ready(&t, "")[0];
        assert_eq!(t.p.rank_key(&row.id), None);
        assert!(t.p.item(&row.id).is_none());
        assert!(!row.pinnable);
        assert!(t.p.empty_query().is_empty());
        assert_eq!(t.p.name(), "clips");
        assert_eq!(t.p.route(), Some(Route { prefix: "v", needs_space: true }));
        let help = t.p.help();
        assert_eq!(help.len(), 1);
        assert_eq!(help[0].example, "v / v invoice");
        assert_eq!(help[0].about, "search your clipboard history; Enter pastes the item");
        assert_eq!(help[0].fill, "v");
    }
}
