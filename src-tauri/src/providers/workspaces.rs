//! Workspaces: one name in the bar that opens a set of items (apps, files,
//! folders, games, bookmarks…) one after another, each like Enter on its row.
//! The items run on a background thread through an [`ItemRunner`] (the engine);
//! the provider itself only reads its list on the query path.

use crate::config::{ItemRef, WorkspaceDef};
use crate::direct_keys::{wait_ready, STARTUP_WAIT, STILL_STARTING};
use crate::engine::Engine;
use crate::fuzzy;
use crate::provider::{Modifier, Outcome, Provider, Query, ResultItem};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

/// Provider name of workspace rows.
pub const NAME: &str = "workspaces";

/// Time between two items of a workspace, so windows open in order.
pub const PAUSE: Duration = Duration::from_millis(300);

/// Tells the user how a run went: workspace name, message.
pub type Report = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// Resolves and opens items by `(provider, id)`, like Enter on their rows.
pub trait ItemRunner: Send + Sync {
    fn item(&self, provider: &str, id: &str) -> Option<ResultItem>;
    /// False while the provider has not finished its first scan.
    fn ready(&self, provider: &str) -> bool;
    fn open(&self, provider: &str, id: &str) -> Result<Outcome, String>;
}

impl ItemRunner for Engine {
    fn item(&self, provider: &str, id: &str) -> Option<ResultItem> {
        Engine::item(self, provider, id)
    }
    fn ready(&self, provider: &str) -> bool {
        Engine::ready(self, provider)
    }
    fn open(&self, provider: &str, id: &str) -> Result<Outcome, String> {
        self.open_item(provider, id)
    }
}

/// How a run went.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Run {
    /// Items that could not open.
    pub failed: usize,
    /// Of those, items whose provider had not finished its first scan in time.
    pub starting: usize,
}

impl Run {
    /// What the user is told, if anything: `Flick is still starting` when that
    /// is the only reason, else `<k> of <n> could not open`.
    pub fn message(&self, n: usize) -> Option<String> {
        match self.failed {
            0 => None,
            k if k == self.starting => Some(STILL_STARTING.to_string()),
            k => Some(format!("{k} of {n} could not open")),
        }
    }
}

/// Opens `items` in order, `pause` between two opens (never after the last).
/// An item that does not resolve, asks for a confirm, is a workspace (no
/// nesting), needs typing (`Fill`/`Step`: nobody types in a workspace) or fails
/// to open is skipped and counted; nothing is retried. A follow-up (`HideThen`)
/// is not run: the item counts as opened. An item whose provider has not
/// finished its first scan (right after login) is waited for until `wait` after
/// the start of the run, then counted as `starting`.
pub fn run_items(runner: &dyn ItemRunner, items: &[ItemRef], pause: Duration, wait: Duration) -> Run {
    let deadline = Instant::now() + wait;
    let mut run = Run::default();
    let mut first = true;
    for it in items {
        let mut row = runner.item(&it.provider, &it.id);
        if row.is_none() && it.provider != NAME && !runner.ready(&it.provider) {
            if !wait_ready(|| runner.ready(&it.provider), deadline) {
                run.failed += 1;
                run.starting += 1;
                continue;
            }
            row = runner.item(&it.provider, &it.id);
        }
        let openable = it.provider != NAME && row.is_some_and(|row| !row.confirm && row.provider != NAME);
        if !openable {
            run.failed += 1;
            continue;
        }
        if !first {
            std::thread::sleep(pause);
        }
        first = false;
        match runner.open(&it.provider, &it.id) {
            Ok(Outcome::Fill { .. } | Outcome::Step { .. }) | Err(_) => run.failed += 1,
            Ok(_) => {}
        }
    }
    run
}

pub struct WorkspacesProvider {
    defs: RwLock<Vec<WorkspaceDef>>,
    runner: Mutex<Option<Weak<dyn ItemRunner>>>,
    /// True while any workspace is opening.
    running: Arc<AtomicBool>,
    report: Report,
    pause: Duration,
    /// How long a run waits for providers still on their first scan.
    wait: Duration,
}

/// Clears the running flag when the run ends, even by a panic.
struct Done(Arc<AtomicBool>);

impl Drop for Done {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl WorkspacesProvider {
    pub fn new(report: Report) -> Arc<Self> {
        Self::build(report, PAUSE, STARTUP_WAIT)
    }

    #[cfg(test)]
    pub fn with_pause(report: Report, pause: Duration) -> Arc<Self> {
        Self::build(report, pause, Duration::from_millis(100))
    }

    fn build(report: Report, pause: Duration, wait: Duration) -> Arc<Self> {
        Arc::new(Self {
            defs: RwLock::new(Vec::new()),
            runner: Mutex::new(None),
            running: Arc::new(AtomicBool::new(false)),
            report,
            pause,
            wait,
        })
    }

    /// Puts the config's workspaces in.
    pub fn set(&self, defs: Vec<WorkspaceDef>) {
        *self.defs.write().unwrap_or_else(|e| e.into_inner()) = defs;
    }

    /// The engine, once it exists (it holds this provider, so only weakly).
    pub fn set_runner(&self, runner: Weak<dyn ItemRunner>) {
        *self.runner.lock().unwrap_or_else(|e| e.into_inner()) = Some(runner);
    }

    fn row(def: &WorkspaceDef, score: u32) -> ResultItem {
        let n = def.items.len();
        ResultItem {
            id: def.name.clone(),
            provider: NAME,
            title: format!("Open workspace {}", def.name),
            subtitle: if n == 1 { "1 item".to_string() } else { format!("{n} items") },
            score,
            icon: "builtin:workspace".to_string(),
            pinnable: true,
            ..Default::default()
        }
    }

    fn find(&self, name: &str) -> Option<WorkspaceDef> {
        self.defs.read().unwrap_or_else(|e| e.into_inner()).iter().find(|d| d.name == name).cloned()
    }
}

impl Provider for WorkspacesProvider {
    fn name(&self) -> &'static str {
        NAME
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        self.defs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(|d| {
                let best = std::iter::once(&d.name).chain(&d.aliases).filter_map(|w| fuzzy::score(text, w)).max()?;
                Some(Self::row(d, best))
            })
            .collect()
    }

    fn item(&self, id: &str) -> Option<ResultItem> {
        self.find(id).map(|d| Self::row(&d, 0))
    }

    /// Starts opening the workspace's items on a thread and returns at once.
    fn activate(&self, id: &str, _m: Modifier) -> Result<Outcome, String> {
        let def = self.find(id).ok_or_else(|| "not found".to_string())?;
        let runner = self
            .runner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or_else(|| STILL_STARTING.to_string())?;
        if self.running.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return Err("This workspace is still opening".to_string());
        }
        let done = Done(self.running.clone());
        let (report, pause, wait) = (self.report.clone(), self.pause, self.wait);
        std::thread::Builder::new()
            .name("workspace".into())
            .spawn(move || {
                let _done = done;
                let run = run_items(runner.as_ref(), &def.items, pause, wait);
                drop(runner);
                if let Some(message) = run.message(def.items.len()) {
                    report(&def.name, &message);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Outcome::Hide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::time::Instant;

    /// Resolves the items it was given; records every `open` with its time.
    #[derive(Default)]
    struct FakeRunner {
        items: HashMap<(String, String), ResultItem>,
        /// `open` fails for these ids.
        failing: Vec<String>,
        /// `open` returns `HideThen` for these ids.
        then: Vec<String>,
        /// Providers on their first scan: `ready` calls left until they are ready
        /// (`usize::MAX`: never).
        scanning: Mutex<HashMap<String, usize>>,
        opens: Mutex<Vec<(String, Instant)>>,
    }

    impl FakeRunner {
        fn with(mut self, provider: &'static str, id: &str, confirm: bool) -> Self {
            let row = ResultItem { id: id.into(), provider, confirm, pinnable: true, ..Default::default() };
            self.items.insert((provider.into(), id.into()), row);
            self
        }
        fn opened(&self) -> Vec<String> {
            self.opens.lock().unwrap().iter().map(|(id, _)| id.clone()).collect()
        }
    }

    impl ItemRunner for FakeRunner {
        fn item(&self, provider: &str, id: &str) -> Option<ResultItem> {
            if self.scanning.lock().unwrap().contains_key(provider) {
                return None;
            }
            self.items.get(&(provider.to_string(), id.to_string())).cloned()
        }
        fn ready(&self, provider: &str) -> bool {
            let mut scanning = self.scanning.lock().unwrap();
            match scanning.get_mut(provider) {
                None => true,
                Some(0) => {
                    scanning.remove(provider);
                    true
                }
                Some(left) => {
                    if *left != usize::MAX {
                        *left -= 1;
                    }
                    false
                }
            }
        }
        fn open(&self, _provider: &str, id: &str) -> Result<Outcome, String> {
            self.opens.lock().unwrap().push((id.to_string(), Instant::now()));
            if self.failing.iter().any(|f| f == id) {
                return Err("boom".into());
            }
            if id.starts_with("need:") {
                return Ok(Outcome::Fill { text: "x ".into() });
            }
            if id.starts_with("step:") {
                return Ok(Outcome::Step { prompt: "x".into(), scope: "shortcut:x".into() });
            }
            if self.then.iter().any(|f| f == id) {
                return Ok(Outcome::HideThen { then: crate::provider::Then::Snip });
            }
            Ok(Outcome::Hide)
        }
    }

    fn item(provider: &str, id: &str) -> ItemRef {
        ItemRef { provider: provider.into(), id: id.into(), title: String::new() }
    }

    fn def(name: &str, aliases: &[&str], items: Vec<ItemRef>) -> WorkspaceDef {
        WorkspaceDef { name: name.into(), aliases: aliases.iter().map(|a| a.to_string()).collect(), items }
    }

    fn apps(ids: &[&str]) -> Vec<ItemRef> {
        ids.iter().map(|id| item("apps", id)).collect()
    }

    fn runner_for(ids: &[&str]) -> FakeRunner {
        ids.iter().fold(FakeRunner::default(), |r, id| r.with("apps", id, false))
    }

    type Reports = mpsc::Receiver<(String, String)>;

    /// A provider pausing `pause` between items whose reports land in the receiver.
    fn provider(pause: Duration, defs: Vec<WorkspaceDef>) -> (Arc<WorkspacesProvider>, Reports) {
        let (tx, rx) = mpsc::channel();
        let report: Report = Arc::new(move |name: &str, message: &str| {
            tx.send((name.to_string(), message.to_string())).unwrap();
        });
        let p = WorkspacesProvider::with_pause(report, pause);
        p.set(defs);
        (p, rx)
    }

    fn attach(p: &WorkspacesProvider, runner: &Arc<FakeRunner>) {
        let weak: Weak<FakeRunner> = Arc::downgrade(runner);
        p.set_runner(weak);
    }

    /// Waits until the run started by `activate` has finished (report included).
    fn wait_idle(p: &WorkspacesProvider) {
        let start = Instant::now();
        while p.running.load(Ordering::SeqCst) {
            assert!(start.elapsed() < Duration::from_secs(5), "the run never finished");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn query(p: &WorkspacesProvider, text: &str) -> Vec<ResultItem> {
        p.query(&Query { text: text.into(), scope: None })
    }

    #[test]
    fn typing_the_name_or_an_alias_shows_the_workspace() {
        let (p, _rx) =
            provider(Duration::ZERO, vec![def("work", &["w"], apps(&["a", "b", "c"])), def("solo", &[], apps(&["a"]))]);
        for text in ["work", "w"] {
            let rows = query(&p, text);
            let row = rows.iter().find(|r| r.id == "work").unwrap_or_else(|| panic!("{text}: no row"));
            assert_eq!((row.title.as_str(), row.subtitle.as_str()), ("Open workspace work", "3 items"));
            assert_eq!((row.provider, row.icon.as_str()), ("workspaces", "builtin:workspace"));
            assert!(row.pinnable && !row.confirm && row.score > 0);
        }
        let rows = query(&p, "solo");
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].title.as_str(), rows[0].subtitle.as_str()), ("Open workspace solo", "1 item"));
        assert!(query(&p, "zzz").is_empty());
        assert!(query(&p, "").is_empty());
    }

    #[test]
    fn enter_opens_every_item_in_order_with_a_pause() {
        let pause = Duration::from_millis(20);
        let (p, rx) = provider(pause, vec![def("work", &[], apps(&["a", "b", "c"]))]);
        let runner = Arc::new(runner_for(&["a", "b", "c"]));
        attach(&p, &runner);
        assert_eq!(p.activate("work", Modifier::None), Ok(Outcome::Hide));
        assert!(runner.opened().len() < 3, "activate waited for the run");
        wait_idle(&p);
        assert_eq!(runner.opened(), ["a", "b", "c"]);
        let times: Vec<Instant> = runner.opens.lock().unwrap().iter().map(|(_, t)| *t).collect();
        for w in times.windows(2) {
            assert!(w[1] - w[0] >= pause, "gap {:?}", w[1] - w[0]);
        }
        assert!(rx.try_recv().is_err(), "a clean run reports nothing");
    }

    #[test]
    fn a_failed_item_does_not_stop_the_rest_and_is_reported() {
        let (p, rx) = provider(Duration::ZERO, vec![def("work", &[], apps(&["a", "b", "c", "d", "e"]))]);
        let mut runner = runner_for(&["a", "b", "c", "d", "e"]);
        runner.failing = vec!["b".into(), "d".into()];
        let runner = Arc::new(runner);
        attach(&p, &runner);
        assert_eq!(p.activate("work", Modifier::None), Ok(Outcome::Hide));
        let got = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got, ("work".to_string(), "2 of 5 could not open".to_string()));
        wait_idle(&p);
        assert_eq!(runner.opened(), ["a", "b", "c", "d", "e"]);
        assert!(rx.try_recv().is_err(), "reported once");
    }

    #[test]
    fn confirm_items_nested_workspaces_and_missing_items_are_skipped_and_counted() {
        let mut runner = FakeRunner::default()
            .with("apps", "good", false)
            .with("apps", "then", false)
            .with("power", "off", true)
            .with("workspaces", "other", false);
        runner.then = vec!["then".into()];
        let runner = Arc::new(runner);
        let items = vec![
            item("power", "off"),
            item("apps", "good"),
            item("workspaces", "other"),
            item("apps", "gone"),
            item("apps", "then"),
        ];
        assert_eq!(run_items(runner.as_ref(), &items, Duration::ZERO, Duration::ZERO), Run { failed: 3, starting: 0 });
        assert_eq!(runner.opened(), ["good", "then"], "skipped items never reach open; HideThen counts as opened");

        let (p, rx) = provider(Duration::ZERO, vec![def("work", &[], items)]);
        attach(&p, &runner);
        p.activate("work", Modifier::None).unwrap();
        let got = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got, ("work".to_string(), "3 of 5 could not open".to_string()));
        wait_idle(&p);
        assert_eq!(runner.opened(), ["good", "then", "good", "then"]);
    }

    #[test]
    fn items_that_need_typing_count_as_could_not_open() {
        let runner = FakeRunner::default()
            .with("shortcuts", "need:x", false)
            .with("shortcuts", "step:x", false)
            .with("apps", "a", false);
        let items = vec![item("shortcuts", "need:x"), item("apps", "a"), item("shortcuts", "step:x")];
        let run = run_items(&runner, &items, Duration::ZERO, Duration::ZERO);
        assert_eq!(run, Run { failed: 2, starting: 0 });
        assert_eq!(run.message(3).as_deref(), Some("2 of 3 could not open"));
    }

    #[test]
    fn a_provider_still_scanning_is_waited_for() {
        let runner = runner_for(&["a"]).with("games", "g", false);
        runner.scanning.lock().unwrap().insert("games".into(), 3);
        let items = vec![item("games", "g"), item("apps", "a")];
        let run = run_items(&runner, &items, Duration::ZERO, Duration::from_secs(5));
        assert_eq!(run, Run::default());
        assert_eq!(run.message(2), None);
        assert_eq!(runner.opened(), ["g", "a"]);
    }

    #[test]
    fn a_provider_that_never_gets_ready_says_still_starting() {
        let (p, rx) = provider(Duration::ZERO, vec![def("work", &[], vec![item("games", "g"), item("apps", "a")])]);
        let runner = Arc::new(runner_for(&["a"]).with("games", "g", false));
        runner.scanning.lock().unwrap().insert("games".into(), usize::MAX);
        attach(&p, &runner);
        let start = Instant::now();
        p.activate("work", Modifier::None).unwrap();
        let got = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(100), "waited for the scan");
        assert_eq!(got, ("work".to_string(), "Flick is still starting".to_string()));
        wait_idle(&p);
        assert_eq!(runner.opened(), ["a"]);
        // With another failure besides, the usual count.
        assert_eq!(Run { failed: 2, starting: 1 }.message(3).as_deref(), Some("2 of 3 could not open"));
    }

    #[test]
    fn a_second_enter_while_opening_is_refused() {
        let (p, _rx) = provider(
            Duration::from_millis(50),
            vec![def("work", &[], apps(&["a", "b", "c"])), def("play", &[], apps(&["a"]))],
        );
        let runner = Arc::new(runner_for(&["a", "b", "c"]));
        attach(&p, &runner);
        assert_eq!(p.activate("work", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(p.activate("work", Modifier::None), Err("This workspace is still opening".to_string()));
        assert_eq!(p.activate("play", Modifier::None), Err("This workspace is still opening".to_string()));
        wait_idle(&p);
        assert_eq!(runner.opened(), ["a", "b", "c"]);
        // Once the run is over, a workspace opens again.
        assert_eq!(p.activate("play", Modifier::None), Ok(Outcome::Hide));
        wait_idle(&p);
        assert_eq!(runner.opened(), ["a", "b", "c", "a"]);
    }

    #[test]
    fn without_a_runner_nothing_opens() {
        let (p, rx) = provider(Duration::ZERO, vec![def("work", &[], apps(&["a"]))]);
        assert_eq!(p.activate("work", Modifier::None), Err("Flick is still starting".to_string()));
        let runner = Arc::new(runner_for(&["a"]));
        attach(&p, &runner);
        assert_eq!(p.activate("nope", Modifier::None), Err("not found".to_string()));
        drop(runner);
        assert_eq!(p.activate("work", Modifier::None), Err("Flick is still starting".to_string()));
        assert!(!p.running.load(Ordering::SeqCst));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn workspaces_are_learned_and_pinnable() {
        let (p, _rx) = provider(Duration::ZERO, vec![def("work", &["w"], apps(&["a", "b"]))]);
        assert_eq!(p.name(), "workspaces");
        assert_eq!(p.rank_key("work"), Some("work".to_string()));
        let row = p.item("work").unwrap();
        assert_eq!((row.id.as_str(), row.title.as_str(), row.subtitle.as_str()), ("work", "Open workspace work", "2 items"));
        assert!(row.pinnable && row.score == 0);
        assert!(p.item("w").is_none(), "rebuilt by name only");
        assert!(p.item("nope").is_none());
        p.set(vec![]);
        assert!(p.item("work").is_none());
    }
}
