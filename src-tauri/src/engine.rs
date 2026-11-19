//! Engine: fans a query out to providers, merges, ranks and truncates.

use std::cell::Cell;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::fuzzy;
use crate::history::History;
use crate::pins::Pins;
use crate::providers::path::{PathProvider, Unfolded};
use crate::provider::{Action, HelpLine, Modifier, Origin, Outcome, Provider, Query, ResultItem, Route};
use crate::ranking::Ranking;

/// Cap for plain queries.
pub const MAX_RESULTS: usize = 50;
/// Rows the bar shows at once; plain-query shares apply per page of this many.
pub const PAGE: usize = 8;
/// Cap for the truly empty bar (welcome, pins, recents).
pub const EMPTY_BAR_ROWS: usize = 8;
/// Cap for routed, claimed and scoped queries (file lists, shell history, help).
pub const ROUTED_MAX_RESULTS: usize = 50;
/// Provider name of help items, and the word that routes to the help list.
pub const HELP: &str = "help";

const HELP_ROUTE: Route = Route { prefix: HELP, needs_space: true };

/// Numbers the queries in the order they reach the engine (the first is 1).
static QUERY_SEQ: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The number of the query running on this thread; 0 outside one.
    static CURRENT_QUERY: Cell<u64> = const { Cell::new(0) };
}

/// The number of the query this provider call belongs to: a later query has a
/// higher number. Queries run on their own threads and may overlap, so a
/// provider keeping state across calls can tell an older query that arrives
/// late from the newest one. 0 when called outside a query (tests).
pub fn query_seq() -> u64 {
    CURRENT_QUERY.with(Cell::get)
}

/// Gives the thread's query its number while it runs.
struct SeqGuard(u64);

impl SeqGuard {
    fn enter() -> Self {
        let seq = QUERY_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
        SeqGuard(CURRENT_QUERY.with(|c| c.replace(seq)))
    }
}

impl Drop for SeqGuard {
    fn drop(&mut self) {
        CURRENT_QUERY.with(|c| c.set(self.0));
    }
}

/// Tells a search whether a newer one has begun since it started. Only the
/// bar's `query` command makes searches that can be superseded (see
/// [`Engine::query_newest`]); every other caller gets complete results.
#[derive(Clone)]
pub struct QueryGate {
    newest: Arc<AtomicU64>,
    mine: u64,
}

impl QueryGate {
    /// A gate for the next search of `newest`'s owner (tests).
    #[cfg(test)]
    pub fn begin_for_test(newest: &Arc<AtomicU64>, seq: u64) -> QueryGate {
        newest.fetch_max(seq, Ordering::SeqCst);
        QueryGate { newest: newest.clone(), mine: seq }
    }

    /// True once a search with a higher number has begun on the same engine.
    pub fn superseded(&self) -> bool {
        self.newest.load(Ordering::Relaxed) > self.mine
    }
}

thread_local! {
    /// The gate of the cancellable search running on this thread, if any.
    static GATE: std::cell::RefCell<Option<QueryGate>> = const { std::cell::RefCell::new(None) };
}

/// The running search's gate: `None` outside a cancellable search.
pub fn current_gate() -> Option<QueryGate> {
    GATE.with(|g| g.borrow().clone())
}

/// Runs `f` as the cancellable search `gate` stands for.
fn with_gate<R>(gate: QueryGate, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<QueryGate>);
    impl Drop for Restore {
        fn drop(&mut self) {
            GATE.with(|g| *g.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(GATE.with(|g| g.borrow_mut().replace(gate)));
    f()
}

/// Every provider, in order (tests only).
#[cfg(test)]
impl Engine {
    pub fn providers(&self) -> impl Iterator<Item = &dyn Provider> {
        self.providers.iter().map(|p| p.as_ref())
    }
}

/// Runs `f` as if inside query number `seq`.
#[cfg(test)]
pub fn as_query<R>(seq: u64, f: impl FnOnce() -> R) -> R {
    let previous = CURRENT_QUERY.with(|c| c.replace(seq));
    let r = f();
    CURRENT_QUERY.with(|c| c.set(previous));
    r
}

/// The text after `route.prefix`, trimmed, when `text` is routed there: the
/// prefix alone, the prefix and a space, or (symbol prefixes) the prefix
/// directly. Word prefixes (`needs_space`) match in any case: `Help` is `help`.
fn strip_route<'a>(text: &'a str, route: &Route) -> Option<&'a str> {
    let rest = if route.needs_space {
        let n = route.prefix.len();
        let head = text.get(..n)?;
        if !head.eq_ignore_ascii_case(route.prefix) {
            return None;
        }
        &text[n..]
    } else {
        text.strip_prefix(route.prefix)?
    };
    (rest.is_empty() || rest.starts_with(' ') || !route.needs_space).then(|| rest.trim())
}

pub struct Engine {
    providers: Vec<Box<dyn Provider>>,
    ranking: Arc<Ranking>,
    history: Arc<History>,
    pins: Arc<Pins>,
    /// Highest number of any cancellable search begun (see [`Engine::query_newest`]).
    newest: Arc<AtomicU64>,
    /// The provider that unfolds a folder's contents (see [`Engine::unfold`]).
    path: Option<Arc<PathProvider>>,
}

impl Engine {
    pub fn new(
        providers: Vec<Box<dyn Provider>>,
        ranking: Arc<Ranking>,
        history: Arc<History>,
        pins: Arc<Pins>,
    ) -> Self {
        Self { providers, ranking, history, pins, newest: Arc::new(AtomicU64::new(0)), path: None }
    }

    /// The path provider `unfold` asks (it also sits among `providers`).
    pub fn with_path(mut self, path: Arc<PathProvider>) -> Self {
        self.path = Some(path);
        self
    }

    /// The contents of a folder row, for the bar to show under it. Memory
    /// only: nothing is ranked, recorded or pinned, and no search is cancelled.
    /// Without a path provider every folder is `missing`.
    pub fn unfold(&self, folder: &str) -> Unfolded {
        match &self.path {
            Some(path) => path.unfold(folder),
            None => Unfolded { state: "missing", rows: Vec::new(), more: 0, typed: String::new() },
        }
    }

    /// Pins or unpins an item; returns the new pinned state. Unpinning always
    /// works; pinning only an item its provider can rebuild and marks
    /// `pinnable`, so typed text (shortcut arguments) never reaches disk.
    pub fn toggle_pin(&self, provider: &str, id: &str) -> bool {
        if !self.pins.is_pinned(provider, id) && !self.item(provider, id).is_some_and(|i| i.pinnable) {
            return false;
        }
        self.pins.toggle(provider, id)
    }

    /// Past searches, newest first.
    pub fn past_searches(&self) -> Vec<String> {
        self.history.searches()
    }

    /// In-memory only; sorted by score + boost. Plain queries return at most
    /// `MAX_RESULTS`; routed and scoped ones `ROUTED_MAX_RESULTS`. Empty text
    /// returns nothing, except inside a scope, where it lists the scope's choices.
    ///
    /// A scope wins over routes. Otherwise text that matches a provider's route
    /// (or the built-in `help` route) goes to that provider alone, with the
    /// prefix removed; providers that own a route never see plain text. Text a
    /// provider claims by its shape (a typed path) goes, whole, to the first
    /// such provider alone; `help` wins over a claim, a claim over prefixes.
    pub fn query(&self, text: &str, scope: Option<&str>) -> Vec<ResultItem> {
        self.query_among(text, scope, &|_| true)
    }

    /// [`Engine::query`] for the bar, which numbers its searches (`seq`, rising;
    /// 0 = unnumbered). A numbered search registers its number, and an older one
    /// still scanning stops early: a search is superseded only by a higher
    /// number, so the highest one is never cancelled, whatever order the
    /// commands start in. `None` when superseded: the page has asked again and
    /// drops the answer, so it must never be taken for "no results". An
    /// unnumbered search neither cancels nor is cancelled. Other callers use
    /// [`Engine::query`], which is never cut short.
    pub fn query_newest(&self, text: &str, scope: Option<&str>, seq: u64) -> Option<Vec<ResultItem>> {
        if seq == 0 {
            return Some(self.query(text, scope));
        }
        self.newest.fetch_max(seq, Ordering::SeqCst);
        let gate = QueryGate { newest: self.newest.clone(), mine: seq };
        let rows = with_gate(gate.clone(), || self.query(text, scope));
        (!gate.superseded()).then_some(rows)
    }

    /// Like [`Engine::query`], but only the providers named nowhere in `skip`
    /// are asked anything (routes, claims and plain text alike): the others
    /// are never queried, told `skipped`, or asked for empty-bar rows. For
    /// callers that must not start a provider's network or device reads.
    pub fn query_except(&self, text: &str, skip: &[&str]) -> Vec<ResultItem> {
        self.query_among(text, None, &|p| !skip.contains(&p.name()))
    }

    fn query_among(&self, text: &str, scope: Option<&str>, asked: &dyn Fn(&dyn Provider) -> bool) -> Vec<ResultItem> {
        let _seq = SeqGuard::enter();
        let (mut rows, plain) = self.query_rows(text, scope, asked);
        if !plain {
            for p in self.providers.iter().filter(|p| asked(p.as_ref())) {
                p.skipped();
            }
        }
        for row in &mut rows {
            row.pinned = self.pins.is_pinned(row.provider, &row.id);
        }
        rows
    }

    /// The rows, and whether the plain providers were asked (a plain query or
    /// the truly empty bar); when not, the engine tells them with `skipped`.
    /// Only providers `asked` accepts take part.
    fn query_rows(
        &self,
        text: &str,
        scope: Option<&str>,
        asked: &dyn Fn(&dyn Provider) -> bool,
    ) -> (Vec<ResultItem>, bool) {
        let providers = || self.providers.iter().filter(|p| asked(p.as_ref()));
        let untrimmed_empty = text.is_empty();
        let text = text.trim();
        if let Some(scope) = scope {
            let q = Query { text: text.to_string(), scope: Some(scope.to_string()) };
            let owners = providers().filter(|p| p.owns_scope(scope));
            return (self.rank(owners, &q, ROUTED_MAX_RESULTS, false), false);
        }
        if text.is_empty() {
            // Only the unscoped, truly empty bar (not whitespace) shows rows.
            if untrimmed_empty {
                return (self.empty_bar(asked), true);
            }
            return (vec![], false);
        }
        if let Some(rest) = strip_route(text, &HELP_ROUTE) {
            return (self.help_items(rest), false);
        }
        // The first provider that claims the text by its shape.
        if let Some(p) = providers().find(|p| p.claims(text)) {
            let q = Query { text: text.to_string(), scope: None };
            return (self.rank(std::iter::once(p), &q, ROUTED_MAX_RESULTS, false), false);
        }
        for p in providers() {
            if let Some(rest) = p.route().and_then(|r| strip_route(text, &r)) {
                let q = Query { text: rest.to_string(), scope: None };
                return (self.rank(std::iter::once(p), &q, ROUTED_MAX_RESULTS, false), false);
            }
        }
        let q = Query { text: text.to_string(), scope: None };
        (self.rank(providers().filter(|p| p.route().is_none()), &q, MAX_RESULTS, true), true)
    }

    /// The truly empty bar: the providers' own rows (welcome) in registration
    /// order, then pins, then recent launches that are not pinned, cut to
    /// `EMPTY_BAR_ROWS`. Pins that do not resolve are skipped, never removed. A
    /// recent that does not resolve is forgotten, but only once its (known)
    /// provider has finished its first scan; before that it may just be missing
    /// from an index that is not built yet.
    fn empty_bar(&self, asked: &dyn Fn(&dyn Provider) -> bool) -> Vec<ResultItem> {
        let mut rows: Vec<ResultItem> =
            self.providers.iter().filter(|p| asked(p.as_ref())).flat_map(|p| p.empty_query()).collect();
        let pins = self.pins.list();
        for (provider, id) in &pins {
            if let Some(item) = self.item(provider, id) {
                rows.push(ResultItem { origin: Origin::Pin, ..item });
            }
        }
        let mut stale = Vec::new();
        for (provider, id) in self.history.recents() {
            if pins.iter().any(|(p, i)| *p == provider && *i == id) {
                continue;
            }
            match self.item(&provider, &id) {
                Some(item) => rows.push(ResultItem { origin: Origin::Recent, ..item }),
                None => {
                    if self.provider(&provider).is_some() && self.ready(&provider) {
                        stale.push((provider, id));
                    }
                }
            }
        }
        // Memory now, disk on another thread: the query path never writes.
        if !stale.is_empty() {
            self.history.forget_launches(&stale);
        }
        rows.truncate(EMPTY_BAR_ROWS);
        rows
    }

    /// Whether `text` typed in `scope` is worth remembering as a past search:
    /// not empty, not inside a scope, not a route (`help`, `>`, ...) and not
    /// claimed (a typed path).
    fn searchable(&self, text: &str, scope: Option<&str>) -> bool {
        let text = text.trim();
        scope.is_none()
            && !text.is_empty()
            && strip_route(text, &HELP_ROUTE).is_none()
            && !self.providers.iter().any(|p| p.claims(text))
            && !self.providers.iter().any(|p| p.route().is_some_and(|r| strip_route(text, &r).is_some()))
    }

    /// Sorted by score + boost, cut to `max`. With `shares` (plain queries),
    /// `top` rows come first, and the list is built a page of [`PAGE`] rows at
    /// a time: within a page each provider keeps at most its
    /// [`Provider::plain_share`] rows among the others' results; its further
    /// items fill the page's rows left over, after them, and what the page
    /// had no room for leads the next page, still in rank order.
    fn rank<'a>(
        &self,
        providers: impl Iterator<Item = &'a Box<dyn Provider>>,
        q: &Query,
        max: usize,
        shares: bool,
    ) -> Vec<ResultItem> {
        let mut ranked: Vec<(u32, usize, ResultItem)> = providers
            .flat_map(|p| {
                let share = p.plain_share();
                let found = {
                    let _files = if crate::timing::enabled() && p.name() == "files" { crate::timing::span("query.files") } else { crate::timing::span_off() };
                    p.query(q)
                };
                found.into_iter().map(move |item| {
                    let boost = p.rank_key(&item.id).map_or(0, |key| self.ranking.boost(p.name(), &key));
                    (item.score.saturating_add(boost), share, item)
                })
            })
            .collect();
        // Plain queries put computed answers (`top`) above everything else.
        ranked.sort_by(|(sa, _, a), (sb, _, b)| {
            (shares && b.top)
                .cmp(&(shares && a.top))
                .then_with(|| sb.cmp(sa))
                .then_with(|| a.title.chars().count().cmp(&b.title.chars().count()))
                .then_with(|| a.title.cmp(&b.title))
        });
        if !shares {
            ranked.truncate(max);
            return ranked.into_iter().map(|(_, _, item)| item).collect();
        }
        let mut pending: std::collections::VecDeque<(usize, ResultItem)> =
            ranked.into_iter().map(|(_, share, item)| (share, item)).collect();
        let mut out = Vec::new();
        while out.len() < max && !pending.is_empty() {
            let mut taken: std::collections::HashMap<&'static str, usize> = Default::default();
            let (mut kept, mut spill) = (Vec::new(), Vec::new());
            while kept.len() < PAGE {
                let Some((share, item)) = pending.pop_front() else { break };
                let n = taken.entry(item.provider).or_default();
                if *n < share {
                    *n += 1;
                    kept.push(item);
                } else {
                    spill.push((share, item));
                }
            }
            let room = PAGE - kept.len();
            let mut spill = spill.into_iter();
            kept.extend(spill.by_ref().take(room).map(|(_, item)| item));
            // Spilled rows were ranked above everything not yet looked at.
            for row in spill.rev() {
                pending.push_front(row);
            }
            out.extend(kept);
        }
        out.truncate(max);
        out
    }

    /// Every provider's help lines in registration order, then the engine's own,
    /// filtered by `filter` and kept in that order (not score order).
    fn help_items(&self, filter: &str) -> Vec<ResultItem> {
        let own = [
            HelpLine { example: "help".into(), about: "show this list".into(), fill: String::new() },
            HelpLine { example: "Esc".into(), about: "close the bar".into(), fill: String::new() },
            HelpLine { example: "Ctrl+P".into(), about: "pin or unpin the selected row".into(), fill: String::new() },
            HelpLine { example: "Up".into(), about: "recall past searches (empty bar)".into(), fill: String::new() },
            HelpLine { example: "Tab".into(), about: "show the actions for the selected row".into(), fill: String::new() },
            HelpLine { example: "Right / Left".into(), about: "unfold or fold the selected folder".into(), fill: String::new() },
        ];
        self.providers
            .iter()
            .flat_map(|p| p.help())
            .chain(own)
            .filter(|l| filter.is_empty() || fuzzy::score(filter, &format!("{} {}", l.example, l.about)).is_some())
            .take(ROUTED_MAX_RESULTS)
            .enumerate()
            .map(|(i, l)| ResultItem {
                id: l.fill,
                provider: HELP,
                title: l.example,
                subtitle: l.about,
                score: 1000 - i as u32,
                icon: "builtin:help".into(),
                ..Default::default()
            })
            .collect()
    }

    fn provider(&self, name: &str) -> Option<&dyn Provider> {
        self.providers.iter().find(|p| p.name() == name).map(|p| p.as_ref())
    }

    /// `(provider, id)` of every pin, in pin order.
    pub fn pin_list(&self) -> Vec<(String, String)> {
        self.pins.list()
    }

    /// Moves a pin one place up or down.
    pub fn move_pin(&self, provider: &str, id: &str, up: bool) {
        self.pins.move_pin(provider, id, up);
    }

    /// The row `provider` would return for `id`, if it can be rebuilt.
    pub fn item(&self, provider: &str, id: &str) -> Option<ResultItem> {
        self.provider(provider)?.item(id)
    }

    /// False while the provider has not finished its first scan. An unknown
    /// provider has nothing to wait for.
    pub fn ready(&self, provider: &str) -> bool {
        self.provider(provider).is_none_or(|p| p.ready())
    }

    /// Runs a row's menu action. Nothing is ranked or recorded: an action is
    /// not a launch.
    pub fn act(&self, provider: &str, id: &str, action: Action) -> Result<Outcome, String> {
        let outcome = match self.provider(provider).ok_or_else(|| format!("unknown provider: {provider}"))?.act(id, action) {
            // No to Windows' "delete for good?": the item and its row stay, with no error.
            Err(e) if action == Action::Delete && e == crate::file_ops::DELETE_DECLINED => return Ok(Outcome::Keep),
            other => other?,
        };
        // Gone for good (to the Recycle Bin): what Flick knows about it goes too.
        if action == Action::Delete {
            self.pins.forget(id);
            self.ranking.forget(id);
            self.history.forget(id);
            self.moved(id, None);
            self.listing_changed(id, None);
        }
        Ok(outcome)
    }

    /// Windows' own menu for a file or folder row (`show` gets the row's path).
    /// Only a row whose menu offers More options… is shown one, and only while
    /// its path is still on disk as the same kind; otherwise a short error and
    /// `show` is not called. `show`'s answer is passed on: whether a command ran.
    pub fn more_options(&self, provider: &str, id: &str, show: impl FnOnce(&Path) -> Result<bool, String>) -> Result<bool, String> {
        let p = self.provider(provider).ok_or_else(|| format!("unknown provider: {provider}"))?;
        if !p.item(id).is_some_and(|item| item.actions.contains(&Action::MoreOptions)) {
            return Err("can't do that here".into());
        }
        let (path, is_dir) = crate::providers::path::split_id(id);
        let path = Path::new(path);
        // Checking the disk is fine here: this is not the query path.
        if std::fs::metadata(path).ok().map(|m| m.is_dir()) != Some(is_dir) {
            return Err("file not found".into());
        }
        show(path)
    }

    /// The item `id` was renamed to `new` (`None`: deleted): the cached folder
    /// listings (typed paths, `dl`, unfolded folders) are patched, not
    /// forgotten, so the page's next ask answers at once with the row gone or
    /// renamed in place, and the selection can stay where it belongs.
    fn listing_changed(&self, id: &str, new: Option<&str>) {
        let Some(path) = &self.path else { return };
        let path_of = |id: &str| std::path::PathBuf::from(crate::providers::path::split_id(id).0);
        path.entry_moved(&path_of(id), new.map(path_of).as_deref());
    }

    /// The item `old` (a row id) now is `new`, or gone: every provider that
    /// keeps paths follows.
    fn moved(&self, old: &str, new: Option<&str>) {
        let path = |id: &str| std::path::PathBuf::from(crate::providers::path::split_id(id).0);
        let new = new.map(path);
        for p in &self.providers {
            p.moved(&path(old), new.as_deref());
        }
    }

    /// Renames a file or folder row; the new row id. Pins, picks and recent
    /// launches follow it (and, for a folder, those of what is inside).
    pub fn rename(&self, provider: &str, id: &str, new_name: &str) -> Result<String, String> {
        let new_id = self.provider(provider).ok_or_else(|| format!("unknown provider: {provider}"))?.rename(id, new_name)?;
        if new_id != id {
            self.pins.rename(id, &new_id);
            self.ranking.rename(id, &new_id);
            self.history.rename(id, &new_id);
            self.moved(id, Some(&new_id));
            self.listing_changed(id, Some(&new_id));
        }
        Ok(new_id)
    }

    /// Runs an item like Enter on its row (no modifier) for a workspace or a
    /// direct hotkey. Nothing is ranked or recorded. An item that asks for a
    /// confirm is refused: there is no second Enter to give.
    pub fn open_item(&self, provider: &str, id: &str) -> Result<Outcome, String> {
        let item = self.item(provider, id).ok_or_else(|| "not found".to_string())?;
        if item.confirm {
            return Err("needs a second Enter".to_string());
        }
        self.provider(provider).ok_or_else(|| "not found".to_string())?.activate(id, Modifier::None)
    }

    /// `text` and `scope` are what the bar held when the row was picked. A launch
    /// that is learned (`rank_key`) is ranked; when its provider can also rebuild
    /// the row (`item`) it is recorded as a recent launch (unless it asks for a
    /// confirm), and the text as a past search when it is searchable. A failed
    /// launch records nothing; when it no longer resolves, it is also dropped
    /// from the recents.
    pub fn activate(
        &self,
        provider: &str,
        id: &str,
        m: Modifier,
        text: &str,
        scope: Option<&str>,
    ) -> Result<Outcome, String> {
        // Help lines are not a provider: picking one only fills the bar, and is never learned.
        if provider == HELP {
            return Ok(Outcome::Fill { text: id.to_string() });
        }
        let p = self
            .providers
            .iter()
            .find(|p| p.name() == provider)
            .ok_or_else(|| format!("unknown provider: {provider}"))?;
        let outcome = match p.activate(id, m) {
            Ok(o) => o,
            Err(e) => {
                if p.ready() && p.item(id).is_none() {
                    self.history.forget_launch(provider, id);
                }
                return Err(e);
            }
        };
        if let Some(key) = p.rank_key(id) {
            self.ranking.record(provider, &key);
            // Rows built from typed text (shortcut arguments) cannot be rebuilt: never saved.
            if let Some(item) = p.item(id) {
                // A confirm row in the empty bar would arm on a habitual Enter.
                if !item.confirm {
                    self.history.record_launch(provider, id);
                }
                if self.searchable(text, scope) {
                    self.history.record_search(text);
                }
            }
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Item = (&'static str, &'static str, u32); // id, title, score

    struct FakeProvider {
        name: &'static str,
        items: Vec<Item>,
        scope: Option<&'static str>,
        step: bool,
        /// false: opts out of learning (`rank_key` returns `None`), like power.
        learn: bool,
        /// `activate` fails with this text.
        fail: Option<&'static str>,
        route: Option<Route>,
        help: Vec<HelpLine>,
        /// Rows this provider may take in a plain query while others have results.
        share: usize,
        /// `ready()` result.
        ready: bool,
        /// Every item's `confirm` and `pinnable`.
        confirm: bool,
        pinnable: bool,
        /// Every query row's `top`.
        top: bool,
        /// `Query.text` of every query this provider received.
        seen: std::sync::Mutex<Vec<String>>,
        /// Claims text that starts with this.
        claims: Option<&'static str>,
        /// Every `act` call.
        acted: std::sync::Mutex<Vec<(String, Action)>>,
        /// Every `activate` call: id and modifier.
        activated: std::sync::Mutex<Vec<(String, Modifier)>>,
    }

    impl FakeProvider {
        fn new(name: &'static str, items: Vec<Item>) -> Self {
            Self {
                name,
                items,
                scope: None,
                step: false,
                learn: true,
                fail: None,
                route: None,
                help: vec![],
                share: usize::MAX,
                ready: true,
                confirm: false,
                pinnable: true,
                top: false,
                seen: Default::default(),
                claims: None,
                acted: Default::default(),
                activated: Default::default(),
            }
        }
    }

    impl Provider for FakeProvider {
        fn name(&self) -> &'static str {
            self.name
        }
        fn owns_scope(&self, scope: &str) -> bool {
            self.scope == Some(scope)
        }
        fn route(&self) -> Option<Route> {
            self.route
        }
        fn help(&self) -> Vec<HelpLine> {
            self.help.clone()
        }
        fn plain_share(&self) -> usize {
            self.share
        }
        fn claims(&self, text: &str) -> bool {
            self.claims.is_some_and(|c| text.starts_with(c))
        }
        fn act(&self, id: &str, action: Action) -> Result<Outcome, String> {
            self.acted.lock().unwrap().push((id.to_string(), action));
            Ok(Outcome::Hide)
        }
        fn ready(&self) -> bool {
            self.ready
        }
        fn item(&self, id: &str) -> Option<ResultItem> {
            self.items.iter().find(|(i, _, _)| *i == id).map(|(id, title, _)| ResultItem {
                id: id.to_string(),
                provider: self.name,
                title: title.to_string(),
                confirm: self.confirm,
                pinnable: self.pinnable,
                ..Default::default()
            })
        }
        fn query(&self, q: &Query) -> Vec<ResultItem> {
            self.seen.lock().unwrap().push(q.text.clone());
            self.items
                .iter()
                .map(|(id, title, score)| ResultItem {
                    id: id.to_string(),
                    provider: self.name,
                    title: title.to_string(),
                    subtitle: String::new(),
                    score: *score,
                    top: self.top,
                    ..Default::default()
                })
                .collect()
        }
        fn rank_key(&self, id: &str) -> Option<String> {
            self.learn.then(|| id.to_string())
        }
        fn activate(&self, id: &str, m: Modifier) -> Result<Outcome, String> {
            self.activated.lock().unwrap().push((id.to_string(), m));
            if let Some(e) = self.fail {
                return Err(e.to_string());
            }
            if self.step {
                Ok(Outcome::Step { prompt: "p".into(), scope: "proj".into() })
            } else {
                Ok(Outcome::Hide)
            }
        }
    }

    /// The returned `TempDir` holds `picks.json`; keep it alive for the whole test
    /// so the folder is removed afterwards instead of being recreated in `%TEMP%`.
    fn engine(providers: Vec<FakeProvider>) -> (Engine, Arc<Ranking>, tempfile::TempDir) {
        let boxed = providers
            .into_iter()
            .map(|p| Box::new(p) as Box<dyn Provider>)
            .collect();
        engine_boxed(boxed)
    }

    fn engine_boxed(providers: Vec<Box<dyn Provider>>) -> (Engine, Arc<Ranking>, tempfile::TempDir) {
        let (e, ranking, _, _, dir) = engine_stores(providers);
        (e, ranking, dir)
    }

    type Stores = (Engine, Arc<Ranking>, Arc<History>, Arc<Pins>, tempfile::TempDir);

    fn engine_stores(providers: Vec<Box<dyn Provider>>) -> Stores {
        let dir = tempfile::tempdir().unwrap();
        let ranking = Arc::new(Ranking::load(dir.path().join("picks.json")));
        let history = Arc::new(History::load(dir.path().join("history.json"), true));
        let pins = Arc::new(Pins::load(dir.path().join("pins.json")));
        let e = Engine::new(providers, ranking.clone(), history.clone(), pins.clone());
        (e, ranking, history, pins, dir)
    }

    #[test]
    fn unfold_reaches_the_path_provider_and_records_nothing() {
        use crate::clipboard::FakeClipboard;
        use crate::dirs::testing::{file, FakeFs};
        use crate::dirs::{Loader, FRESH, SLOW};
        use crate::launch::testing::RecordingLauncher;

        let fs = Arc::new(FakeFs::default());
        fs.add_dir(r"C:\Work", vec![file("a.txt", 10)]);
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let dirs = Loader::new(Arc::new(move || {
            let _ = tx.lock().unwrap().send(());
        }), FRESH, SLOW);
        let path = Arc::new(PathProvider::new(dirs, fs, None, Arc::new(RecordingLauncher::default()), Arc::new(FakeClipboard::default()), Arc::new(crate::file_ops::FakeFileOps::default())));
        let (e, ranking, history, pins, _dir) = engine_stores(Vec::new());
        let e = e.with_path(path);
        assert_eq!(e.unfold(r"C:\Work\").state, "reading");
        rx.recv_timeout(std::time::Duration::from_secs(1)).expect("on_ready");
        let u = e.unfold(r"C:\Work\");
        assert_eq!((u.state, u.rows.len(), u.rows[0].id.as_str(), u.typed.as_str()), ("ready", 1, r"C:\Work\a.txt", r"C:\Work\"));
        assert_eq!(ranking.boost("path", r"C:\Work\a.txt"), 0);
        assert!(history.searches().is_empty() && pins.list().is_empty());
        assert_eq!(Engine::new(Vec::new(), ranking, history, pins).unfold(r"C:\Work\").state, "missing");
    }

    #[test]
    fn delete_and_rename_patch_the_listings_so_the_next_ask_answers_at_once() {
        use crate::clipboard::FakeClipboard;
        use crate::dirs::testing::{file, FakeFs};
        use crate::dirs::{Loader, FRESH, SLOW};
        use crate::launch::testing::RecordingLauncher;
        use crate::providers::recent::DownloadsProvider;

        let fs = Arc::new(FakeFs::default());
        fs.add_dir(r"C:\Work", vec![file("a.txt", 10), file("b.txt", 10), file("c.txt", 10)]);
        fs.add_dir(r"C:\Dl", vec![file("x.zip", 10), file("y.zip", 10)]);
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let dirs = Loader::new(Arc::new(move || {
            let _ = tx.lock().unwrap().send(());
        }), FRESH, SLOW);
        let ops: Arc<crate::file_ops::FakeFileOps> = Arc::new(Default::default());
        let launcher = Arc::new(RecordingLauncher::default());
        let clip = Arc::new(FakeClipboard::default());
        let path = Arc::new(PathProvider::new(dirs.clone(), fs.clone(), None, launcher.clone(), clip.clone(), ops.clone()));
        let now: crate::dirs::Clock = Arc::new(std::time::SystemTime::now);
        let dl = Arc::new(DownloadsProvider::new(dirs, fs.clone(), Some(r"C:\Dl".into()), launcher, clip, ops, now));
        let (e, _, _, _, _dir) = engine_stores(vec![Box::new(path.clone()), Box::new(dl.clone())]);
        let e = e.with_path(path);
        let wait = || rx.recv_timeout(std::time::Duration::from_secs(1)).expect("on_ready");
        let typed = |e: &Engine| e.query(r"C:\Work\", None).into_iter().map(|r| r.title).collect::<Vec<_>>();
        let dl_rows = || {
            let mut t: Vec<String> = dl.query(&Query { text: String::new(), scope: None }).into_iter().map(|r| r.title).collect();
            t.sort();
            t
        };
        assert_eq!(e.unfold(r"C:\Work\").state, "reading");
        wait();
        assert_eq!(typed(&e), [r"C:\Work\", "a.txt", "b.txt", "c.txt"]);
        assert_eq!(dl_rows(), [crate::providers::path::READING]);
        wait();
        assert_eq!(dl_rows(), ["x.zip", "y.zip"]);

        // Delete: the very next ask has the row gone, no `Reading…` in between.
        assert_eq!(e.act("path", r"C:\Work\a.txt", Action::Delete), Ok(Outcome::Keep));
        assert_eq!(typed(&e), [r"C:\Work\", "b.txt", "c.txt"]);
        let u = e.unfold(r"C:\Work\");
        assert_eq!((u.state, u.rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>()), ("ready", vec!["b.txt", "c.txt"]));
        assert_eq!(e.act("downloads", r"C:\Dl\x.zip", Action::Delete), Ok(Outcome::Keep));
        assert_eq!(dl_rows(), ["y.zip"]);

        // Rename: the row is there at once under its new name.
        assert_eq!(e.rename("path", r"C:\Work\b.txt", "z.txt"), Ok(r"C:\Work\z.txt".to_string()));
        assert_eq!(typed(&e), [r"C:\Work\", "c.txt", "z.txt"]);
        assert_eq!(e.rename("downloads", r"C:\Dl\y.zip", "w.zip"), Ok(r"C:\Dl\w.zip".to_string()));
        assert_eq!(dl_rows(), ["w.zip"]);
        assert!(rx.recv_timeout(std::time::Duration::from_millis(100)).is_err(), "nothing was read again");
    }

    /// A welcome-like provider `w` (one empty-bar row), then `a`.
    fn empty_bar_engine(a: FakeProvider) -> Stores {
        struct Welcome;
        impl Provider for Welcome {
            fn name(&self) -> &'static str {
                "w"
            }
            fn query(&self, _q: &Query) -> Vec<ResultItem> {
                vec![]
            }
            fn activate(&self, _id: &str, _m: Modifier) -> Result<Outcome, String> {
                Ok(Outcome::Hide)
            }
            fn empty_query(&self) -> Vec<ResultItem> {
                vec![ResultItem { id: "w1".into(), provider: "w", title: "welcome".into(), ..Default::default() }]
            }
        }
        engine_stores(vec![Box::new(Welcome), Box::new(a)])
    }

    fn bar(e: &Engine) -> Vec<(String, Origin, bool)> {
        e.query("", None).into_iter().map(|r| (r.title, r.origin, r.pinned)).collect()
    }

    fn letters() -> FakeProvider {
        FakeProvider::new("a", vec![("b", "b", 1), ("c", "c", 1), ("d", "d", 1), ("x", "x", 1)])
    }

    fn pair(p: &str, i: &str) -> (String, String) {
        (p.to_string(), i.to_string())
    }

    #[test]
    fn empty_bar_lists_welcome_then_pins_then_recents() {
        let (e, _, h, p, _dir) = empty_bar_engine(letters());
        p.toggle("a", "b");
        h.record_launch("a", "c");
        h.record_launch("a", "d");
        assert_eq!(
            bar(&e),
            [
                ("welcome".to_string(), Origin::Search, false),
                ("b".to_string(), Origin::Pin, true),
                ("d".to_string(), Origin::Recent, false),
                ("c".to_string(), Origin::Recent, false),
            ]
        );
    }

    #[test]
    fn empty_bar_never_shows_more_than_8_rows() {
        let items: Vec<Item> = vec![
            ("0", "t0", 1), ("1", "t1", 1), ("2", "t2", 1), ("3", "t3", 1), ("4", "t4", 1),
            ("5", "t5", 1), ("6", "t6", 1), ("7", "t7", 1), ("8", "t8", 1), ("9", "t9", 1),
        ];
        let (e, _, h, p, _dir) = empty_bar_engine(FakeProvider::new("a", items));
        for i in 0..7 {
            p.toggle("a", &i.to_string());
        }
        for i in 7..10 {
            h.record_launch("a", &i.to_string());
        }
        let rows = e.query("", None);
        assert_eq!(rows.len(), EMPTY_BAR_ROWS);
        assert_eq!(rows.iter().filter(|r| r.origin == Origin::Recent).count(), 0);
    }

    #[test]
    fn a_pinned_recent_is_listed_once() {
        let (e, _, h, p, _dir) = empty_bar_engine(letters());
        h.record_launch("a", "c");
        p.toggle("a", "c");
        let rows = bar(&e);
        assert_eq!(rows.iter().filter(|r| r.0 == "c").count(), 1);
        assert_eq!(rows[1], ("c".to_string(), Origin::Pin, true));
    }

    #[test]
    fn unresolved_pins_are_skipped_but_kept() {
        let (e, _, _, p, _dir) = empty_bar_engine(letters());
        p.toggle("a", "gone");
        p.toggle("nowhere", "x");
        assert_eq!(bar(&e).len(), 1);
        assert_eq!(p.list().len(), 2);
    }

    #[test]
    fn unresolved_recents_are_kept_while_the_provider_is_not_ready() {
        let mut a = letters();
        a.ready = false;
        let (e, _, h, _, _dir) = empty_bar_engine(a);
        h.record_launch("a", "gone");
        h.record_launch("nowhere", "x");
        assert_eq!(bar(&e).len(), 1);
        assert_eq!(h.recents().len(), 2);

        let (e, _, h, _, _dir) = empty_bar_engine(letters());
        h.record_launch("a", "gone");
        h.record_launch("nowhere", "x");
        assert_eq!(bar(&e).len(), 1);
        // The ready provider's missing item is forgotten; the unknown provider's is not.
        assert_eq!(h.recents(), [pair("nowhere", "x")]);
    }

    #[test]
    fn launch_records_search_and_recent() {
        let (e, _, h, _, _dir) = engine_stores(vec![Box::new(FakeProvider::new("a", vec![("x", "X", 1)]))]);
        e.activate("a", "x", Modifier::None, "blen", None).unwrap();
        assert_eq!(h.searches(), ["blen"]);
        assert_eq!(h.recents(), [pair("a", "x")]);
    }

    #[test]
    fn typed_text_launch_records_no_search() {
        let (e, _, h, _, _dir) = engine_stores(vec![Box::new(FakeProvider::new("a", vec![("x", "X", 1)]))]);
        // "arg:a:typed" is not among the provider's items, so `item` cannot rebuild it.
        e.activate("a", "arg:a:typed", Modifier::None, "a typed", None).unwrap();
        assert!(h.searches().is_empty());
        e.activate("a", "x", Modifier::None, "x", None).unwrap();
        assert_eq!(h.searches(), ["x"]);
    }

    #[test]
    fn typed_text_launch_records_no_recent() {
        let (e, ranking, h, _, _dir) = engine_stores(vec![Box::new(FakeProvider::new("a", vec![("x", "X", 1)]))]);
        // Has a rank_key but no `item`: learned for ranking, never kept as a recent or a search.
        e.activate("a", "arg:a:typed", Modifier::None, "a typed", None).unwrap();
        assert!(h.recents().is_empty());
        assert!(h.searches().is_empty());
        assert!(ranking.boost("a", "arg:a:typed") > 0);
    }

    #[test]
    fn shortcut_arguments_never_reach_history_file() {
        use crate::config::ShortcutDef;
        use crate::launch::testing::RecordingLauncher;
        use crate::providers::shortcuts::ShortcutsProvider;
        let def = |name: &str, run: &str| ShortcutDef {
            name: name.into(),
            aliases: vec![],
            about: String::new(),
            run: Some(run.into()),
            wsl: None,
            open: None,
            terminal: None,
            admin: false,
            pick: None,
            choices: vec![],
            icon: None,
        };
        let sc = ShortcutsProvider::new(Arc::new(RecordingLauncher::default()), "");
        sc.set(vec![def("search", "x {arg}"), def("hello", "notepad")], "", "");
        let (e, _, h, _, dir) = engine_stores(vec![Box::new(sc)]);
        e.activate("shortcuts", "arg:search:secret words", Modifier::None, "search secret words", None).unwrap();
        e.activate("shortcuts", "run:hello", Modifier::None, "hello", None).unwrap();
        assert_eq!(h.recents(), [pair("shortcuts", "run:hello")]);
        let saved = std::fs::read_to_string(dir.path().join("history.json")).unwrap();
        assert!(!saved.contains("secret"), "{saved}");
    }

    #[test]
    fn confirm_launch_records_no_recent() {
        let mut a = FakeProvider::new("a", vec![("off", "Shut down", 1)]);
        a.confirm = true;
        let (e, _, h, _, _dir) = engine_stores(vec![Box::new(a)]);
        e.activate("a", "off", Modifier::None, "shut", None).unwrap();
        assert!(h.recents().is_empty());
    }

    #[test]
    fn toggle_pin_refuses_unbuildable_or_unpinnable_items() {
        let (e, _, _, pins, _dir) = engine_stores(vec![Box::new(FakeProvider::new("a", vec![("x", "X", 1)]))]);
        assert!(!e.toggle_pin("a", "arg:a:typed"));
        assert!(!e.toggle_pin("nowhere", "x"));
        assert!(pins.list().is_empty());
        assert!(e.toggle_pin("a", "x"));
        assert!(!e.toggle_pin("a", "x"));
        assert!(pins.list().is_empty());

        let mut a = FakeProvider::new("a", vec![("x", "X", 1)]);
        a.pinnable = false;
        let (e, _, _, pins, _dir) = engine_stores(vec![Box::new(a)]);
        assert!(!e.toggle_pin("a", "x"));
        assert!(pins.list().is_empty());
        // A pin made earlier (or by hand) can still be removed.
        pins.toggle("a", "gone");
        assert!(!e.toggle_pin("a", "gone"));
        assert!(pins.list().is_empty());
    }

    #[test]
    fn routed_scoped_and_unlearned_launches_record_no_search() {
        let mut routed = FakeProvider::new("sh", vec![("x", "X", 1)]);
        routed.route = Some(Route { prefix: ">", needs_space: false });
        let mut unlearned = FakeProvider::new("u", vec![("y", "Y", 1)]);
        unlearned.learn = false;
        let (e, _, h, _, _dir) = engine_stores(vec![
            Box::new(routed),
            Box::new(unlearned),
            Box::new(FakeProvider::new("a", vec![("x", "X", 1)])),
        ]);
        e.activate("sh", "x", Modifier::None, "> htop", None).unwrap();
        e.activate("a", "x", Modifier::None, "help", None).unwrap();
        e.activate("a", "x", Modifier::None, "Help me", None).unwrap();
        e.activate("a", "x", Modifier::None, "  ", None).unwrap();
        e.activate("a", "x", Modifier::None, "pick", Some("proj")).unwrap();
        assert!(h.searches().is_empty());
        h.clear();
        e.activate("u", "y", Modifier::None, "typed", None).unwrap();
        assert!(h.searches().is_empty());
        assert!(h.recents().is_empty());
    }

    #[test]
    fn failed_launch_records_nothing() {
        let mut a = FakeProvider::new("a", vec![("x", "X", 1)]);
        a.fail = Some("boom");
        let (e, _, h, _, _dir) = engine_stores(vec![Box::new(a)]);
        assert!(e.activate("a", "x", Modifier::None, "blen", None).is_err());
        assert!(h.searches().is_empty() && h.recents().is_empty());
    }

    #[test]
    fn failed_launch_of_a_vanished_item_forgets_it() {
        let mut a = FakeProvider::new("a", vec![("x", "X", 1)]);
        a.fail = Some("boom");
        let (e, _, h, _, _dir) = engine_stores(vec![Box::new(a)]);
        h.record_launch("a", "gone");
        h.record_launch("a", "x");
        assert!(e.activate("a", "gone", Modifier::None, "", None).is_err());
        assert!(e.activate("a", "x", Modifier::None, "", None).is_err());
        assert_eq!(h.recents(), [pair("a", "x")]);
    }

    #[test]
    fn history_off_shows_no_recents() {
        let (e, _, h, p, _dir) = empty_bar_engine(letters());
        p.toggle("a", "b");
        h.record_launch("a", "c");
        h.set_enabled(false);
        e.activate("a", "d", Modifier::None, "dd", None).unwrap();
        assert_eq!(bar(&e).iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), ["welcome", "b"]);
        assert!(h.searches().is_empty());
    }

    #[test]
    fn query_rows_carry_the_pinned_flag() {
        let (e, _, _, p, _dir) = engine_stores(vec![Box::new(letters())]);
        p.toggle("a", "b");
        let rows = e.query("x", None);
        assert!(rows.iter().find(|r| r.id == "b").unwrap().pinned);
        assert!(!rows.iter().find(|r| r.id == "c").unwrap().pinned);
    }

    /// Reports whether it ran inside a cancellable search; with `register`, a
    /// search with that sequence number is registered while it runs.
    struct GateWatcher {
        gated: std::sync::Mutex<Vec<bool>>,
        register: Option<u64>,
    }

    impl GateWatcher {
        fn engine(register: Option<u64>) -> (Engine, Arc<GateWatcher>, tempfile::TempDir) {
            let w = Arc::new(GateWatcher { gated: Default::default(), register });
            let (e, _, dir) = engine_boxed(vec![Box::new(w.clone())]);
            (e, w, dir)
        }
    }

    impl Provider for GateWatcher {
        fn name(&self) -> &'static str {
            "g"
        }
        fn query(&self, _q: &Query) -> Vec<ResultItem> {
            let gate = current_gate();
            self.gated.lock().unwrap().push(gate.is_some());
            if let (Some(n), Some(g)) = (self.register, gate) {
                g.newest.fetch_max(n, Ordering::SeqCst);
            }
            vec![ResultItem { id: "1".into(), provider: "g", title: "row".into(), ..Default::default() }]
        }
        fn activate(&self, _id: &str, _m: Modifier) -> Result<Outcome, String> {
            Ok(Outcome::Hide)
        }
    }

    #[test]
    fn only_the_bars_numbered_query_is_cancellable() {
        let (e, w, _dir) = GateWatcher::engine(None);
        assert_eq!(e.query("x", None).len(), 1);
        assert_eq!(e.query_newest("x", None, 3).map(|r| r.len()), Some(1));
        assert_eq!(e.query_newest("x", None, 0).map(|r| r.len()), Some(1));
        assert_eq!(*w.gated.lock().unwrap(), [false, true, false]);
        assert!(current_gate().is_none());
    }

    #[test]
    fn the_highest_number_wins_whatever_order_the_searches_start_in() {
        // (a) 5 starts first, 4 arrives while it runs: 5 completes; 4 started later is superseded.
        let (e, _, _d) = GateWatcher::engine(Some(4));
        assert!(e.query_newest("x", None, 5).is_some());
        assert!(e.query_newest("x", None, 4).is_none());
        // (b) 4 starts first and 5 is registered while it runs: 4 is superseded, 5 completes.
        let (e, _, _d) = GateWatcher::engine(Some(5));
        assert!(e.query_newest("x", None, 4).is_none());
        assert!(e.query_newest("x", None, 5).is_some());
        assert!(current_gate().is_none());
    }

    #[test]
    fn a_search_without_a_number_neither_cancels_nor_is_cancelled() {
        // (c) zero is never superseded, and registers nothing.
        let (e, _, _d) = GateWatcher::engine(Some(9));
        assert!(e.query_newest("x", None, 0).is_some());
        assert_eq!(e.query("x", None).len(), 1);
        assert!(e.newest.load(Ordering::SeqCst) == 0);
        assert!(e.query_newest("x", None, 3).is_none()); // its own provider registers 9
    }

    #[test]
    fn looking_up_an_item_records_nothing() {
        let a = Arc::new(FakeProvider::new("a", vec![("x", "Ex", 500)]));
        let (e, ranking, h, _, dir) = engine_stores(vec![Box::new(a)]);
        assert!(e.item("a", "x").is_some());
        assert_eq!(ranking.boost("a", "x"), 0);
        assert!(h.recents().is_empty() && h.searches().is_empty());
        assert!(!dir.path().join("picks.json").exists());
    }

    #[test]
    fn engine_item_routes_to_the_provider() {
        let (e, _, _dir) = engine_boxed(vec![Box::new(FakeProvider::new("a", vec![("x", "Ex", 500)]))]);
        let row = e.item("a", "x").unwrap();
        assert_eq!((row.id.as_str(), row.title.as_str(), row.provider, row.score), ("x", "Ex", "a", 0));
        assert!(e.item("a", "nope").is_none());
        assert!(e.item("nope", "x").is_none());
        assert!(e.ready("a"));
    }

    #[test]
    fn act_routes_to_the_provider_and_records_nothing() {
        let a = Arc::new(FakeProvider::new("a", vec![("x", "Ex", 500)]));
        let (e, ranking, h, _, dir) = engine_stores(vec![Box::new(a.clone())]);
        assert_eq!(e.act("a", "x", Action::CopyPath), Ok(Outcome::Hide));
        assert_eq!(*a.acted.lock().unwrap(), [("x".to_string(), Action::CopyPath)]);
        assert_eq!(ranking.boost("a", "x"), 0);
        assert!(h.recents().is_empty() && h.searches().is_empty());
        assert!(!dir.path().join("picks.json").exists());
        assert!(e.act("nope", "x", Action::CopyPath).is_err());
    }

    #[test]
    fn act_and_claims_default_to_nothing() {
        struct Bare;
        impl Provider for Bare {
            fn name(&self) -> &'static str {
                "bare"
            }
            fn query(&self, _q: &Query) -> Vec<ResultItem> {
                vec![]
            }
            fn activate(&self, _id: &str, _m: Modifier) -> Result<Outcome, String> {
                Ok(Outcome::Hide)
            }
        }
        let (e, _, _dir) = engine_boxed(vec![Box::new(Bare)]);
        assert_eq!(e.act("bare", "x", Action::Reveal), Err("can't do that here".to_string()));
        assert!(!Bare.claims("C:\\x"));
    }

    #[test]
    fn empty_and_whitespace_return_nothing() {
        let (e, _, _dir) = engine(vec![FakeProvider::new("a", vec![("1", "x", 500)])]);
        assert!(e.query("", None).is_empty());
        assert!(e.query("   ", None).is_empty());
    }

    #[test]
    fn empty_text_asks_empty_query_providers() {
        struct Empty;
        impl Provider for Empty {
            fn name(&self) -> &'static str {
                "empty"
            }
            fn query(&self, _q: &Query) -> Vec<ResultItem> {
                vec![]
            }
            fn activate(&self, _id: &str, _m: Modifier) -> Result<Outcome, String> {
                Ok(Outcome::Hide)
            }
            fn empty_query(&self) -> Vec<ResultItem> {
                vec![ResultItem {
                    id: "w".into(),
                    provider: "empty",
                    title: "hello".into(),
                    subtitle: String::new(),
                    score: 0,
                    ..Default::default()
                }]
            }
        }
        let (e, _, _dir) = engine_boxed(vec![
            Box::new(FakeProvider::new("a", vec![("1", "x", 500)])),
            Box::new(Empty),
        ]);
        let rows = e.query("", None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "hello");
        assert!(e.query("   ", None).is_empty());
    }

    #[test]
    fn merges_and_sorts_across_providers() {
        let (e, _, _dir) = engine(vec![
            FakeProvider::new("a", vec![("1", "low", 500)]),
            FakeProvider::new("b", vec![("2", "high", 900)]),
        ]);
        let r = e.query("x", None);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].provider, r[0].id.as_str()), ("b", "2"));
        assert_eq!((r[1].provider, r[1].id.as_str()), ("a", "1"));
    }

    #[test]
    fn ties_break_by_shorter_title_then_alphabetical() {
        let (e, _, _dir) = engine(vec![FakeProvider::new(
            "a",
            vec![("1", "bbb", 500), ("2", "aaa", 500), ("3", "zz", 500)],
        )]);
        let titles: Vec<_> = e.query("x", None).into_iter().map(|i| i.title).collect();
        assert_eq!(titles, ["zz", "aaa", "bbb"]);
    }

    /// `n` items `{prefix}0..` at `score`, ids leaked to `'static`.
    fn numbered(prefix: &str, n: usize, score: u32) -> Vec<Item> {
        (0..n)
            .map(|i| {
                let id: &'static str = Box::leak(format!("{prefix}{i}").into_boxed_str());
                (id, id, score)
            })
            .collect()
    }

    #[test]
    fn plain_queries_return_up_to_50() {
        let (e, _, _dir) = engine(vec![FakeProvider::new("a", numbered("", 60, 1))]);
        assert_eq!(MAX_RESULTS, 50);
        assert_eq!(e.query("x", None).len(), MAX_RESULTS);
    }

    #[test]
    fn scope_limits_to_owning_provider() {
        let mut owner = FakeProvider::new("owner", vec![("1", "mine", 100)]);
        owner.scope = Some("proj");
        let other = FakeProvider::new("other", vec![("2", "theirs", 900)]);
        let (e, _, _dir) = engine(vec![owner, other]);
        let r = e.query("x", Some("proj"));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].provider, "owner");
        assert_eq!(e.query("x", None).len(), 2);
    }

    #[test]
    fn activate_unknown_provider_is_err() {
        let (e, _, _dir) = engine(vec![]);
        assert!(e.activate("nope", "1", Modifier::None, "", None).is_err());
    }

    #[test]
    fn activate_records_pick() {
        let (e, ranking, _dir) = engine(vec![FakeProvider::new("a", vec![("1", "t", 1)])]);
        assert_eq!(ranking.boost("a", "1"), 0);
        assert_eq!(e.activate("a", "1", Modifier::None, "", None), Ok(Outcome::Hide));
        assert_eq!(ranking.boost("a", "1"), 15);
        assert_eq!(ranking.boost("a", "2"), 0);
    }

    #[test]
    fn picked_item_outranks_equal_score_unpicked() {
        // Same score, "aa" would win the alphabetical tie-break; a pick on "zz" must beat it.
        let (e, _, _dir) = engine(vec![FakeProvider::new("a", vec![("1", "aa", 500), ("2", "zz", 500)])]);
        assert_eq!(e.query("x", None)[0].id, "1");
        e.activate("a", "2", Modifier::None, "", None).unwrap();
        assert_eq!(e.query("x", None)[0].id, "2");
    }

    #[test]
    fn step_outcome_passes_through() {
        let mut p = FakeProvider::new("a", vec![]);
        p.step = true;
        let (e, _, _dir) = engine(vec![p]);
        assert_eq!(
            e.activate("a", "1", Modifier::None, "", None),
            Ok(Outcome::Step { prompt: "p".into(), scope: "proj".into() })
        );
    }

    #[test]
    fn scoped_empty_query_lists_the_scope() {
        let mut owner = FakeProvider::new("owner", vec![("1", "choice", 100)]);
        owner.scope = Some("proj");
        let other = FakeProvider::new("other", vec![("2", "theirs", 900)]);
        let (e, _, _dir) = engine(vec![owner, other]);
        let r = e.query("", Some("proj"));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].provider, "owner");
        assert!(e.query("", None).is_empty());
    }

    #[test]
    fn provider_without_rank_key_records_nothing() {
        let mut p = FakeProvider::new("power", vec![("sleep", "Sleep", 700)]);
        p.learn = false;
        let (e, ranking, dir) = engine(vec![p]);
        assert_eq!(e.activate("power", "sleep", Modifier::None, "", None), Ok(Outcome::Hide));
        assert_eq!(ranking.boost("power", "sleep"), 0);
        assert!(!dir.path().join("picks.json").exists());
    }

    #[test]
    fn rank_key_is_forwarded_through_arc() {
        let mut p = FakeProvider::new("power", vec![("sleep", "Sleep", 700)]);
        p.learn = false;
        let (e, ranking, dir) = engine_boxed(vec![Box::new(Arc::new(p))]);
        e.activate("power", "sleep", Modifier::None, "", None).unwrap();
        assert_eq!(ranking.boost("power", "sleep"), 0);
        assert!(!dir.path().join("picks.json").exists());
    }

    #[test]
    fn stored_picks_never_boost_a_provider_without_rank_key() {
        // e.g. picks recorded for power before it opted out of learning
        let mut power = FakeProvider::new("power", vec![("sleep", "Sleep", 700)]);
        power.learn = false;
        let app = FakeProvider::new("apps", vec![("spotify", "Spotify", 750)]);
        let (e, ranking, _dir) = engine(vec![power, app]);
        for _ in 0..20 {
            ranking.record("power", "sleep");
        }
        assert_eq!(e.query("s", None)[0].provider, "apps");
    }

    #[test]
    fn failed_or_cancelled_activation_records_no_pick() {
        for e in [crate::launch::CANCELLED, "file not found"] {
            let mut p = FakeProvider::new("a", vec![("1", "t", 1)]);
            p.fail = Some(e);
            let (eng, ranking, dir) = engine(vec![p]);
            assert_eq!(eng.activate("a", "1", Modifier::CtrlShift, "", None), Err(e.to_string()));
            assert_eq!(ranking.boost("a", "1"), 0);
            assert!(!dir.path().join("picks.json").exists());
        }
    }

    fn line(example: &str, about: &str, fill: &str) -> HelpLine {
        HelpLine { example: example.into(), about: about.into(), fill: fill.into() }
    }

    fn routed_with(prefix: &'static str, needs_space: bool, items: Vec<Item>) -> FakeProvider {
        let mut p = FakeProvider::new("sh", items);
        p.route = Some(Route { prefix, needs_space });
        p
    }

    /// Engine over a shared routed provider, so the test can read what it was asked.
    fn engine_with_routed(
        routed: FakeProvider,
        plain: FakeProvider,
    ) -> (Engine, Arc<FakeProvider>, tempfile::TempDir) {
        let routed = Arc::new(routed);
        let (e, _, dir) = engine_boxed(vec![Box::new(routed.clone()), Box::new(plain)]);
        (e, routed, dir)
    }

    fn seen(p: &FakeProvider) -> Vec<String> {
        p.seen.lock().unwrap().clone()
    }

    #[test]
    fn routed_text_goes_only_to_the_routing_provider() {
        let (e, sh, _dir) = engine_with_routed(
            routed_with(">", false, vec![("r", "routed", 100)]),
            FakeProvider::new("plain", vec![("p", "plain", 900)]),
        );
        let r = e.query("> ls", None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].provider, "sh");
        assert_eq!(seen(&sh), ["ls"]);
    }

    #[test]
    fn symbol_prefix_needs_no_space() {
        let (e, sh, _dir) = engine_with_routed(
            routed_with(">", false, vec![("r", "routed", 100)]),
            FakeProvider::new("plain", vec![]),
        );
        assert_eq!(e.query(">ls", None).len(), 1);
        assert_eq!(e.query(">", None).len(), 1);
        assert_eq!(seen(&sh), ["ls", ""]);
    }

    #[test]
    fn word_prefix_needs_space_or_end() {
        let (e, sh, _dir) = engine_with_routed(
            routed_with("go", true, vec![("r", "routed", 100)]),
            FakeProvider::new("plain", vec![]),
        );
        assert_eq!(e.query("go", None).len(), 1);
        assert_eq!(e.query("go home", None).len(), 1);
        assert_eq!(seen(&sh), ["", "home"]);
        assert!(e.query("gone", None).is_empty());
        assert_eq!(seen(&sh).len(), 2);
    }

    #[test]
    fn help_word_needs_space_or_end() {
        let (e, _, _dir) = engine(vec![FakeProvider::new("a", vec![])]);
        assert!(!e.query("help", None).is_empty());
        assert!(!e.query("help esc", None).is_empty());
        assert!(e.query("helpndoc", None).is_empty());
    }

    #[test]
    fn word_prefixes_ignore_case() {
        let (e, sh, _dir) = engine_with_routed(
            routed_with("go", true, vec![("r", "routed", 100)]),
            FakeProvider::new("plain", vec![]),
        );
        assert_eq!(e.query("GO", None).len(), 1);
        assert_eq!(e.query("Go Home", None).len(), 1);
        assert_eq!(seen(&sh), ["", "Home"]);
        assert!(e.query("Gone", None).is_empty());
    }

    #[test]
    fn help_word_ignores_case() {
        let (e, _dir) = help_engine();
        let all = e.query("Help", None);
        assert_eq!(all.len(), 9);
        assert!(all.iter().all(|i| i.provider == HELP));
        let r = e.query("HELP blue", None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "bluetooth");
        assert!(e.query("Helpndoc", None).is_empty());
    }

    #[test]
    fn text_containing_prefix_later_is_not_routed() {
        let (e, sh, _dir) = engine_with_routed(
            routed_with(">", false, vec![("r", "routed", 100)]),
            FakeProvider::new("plain", vec![("p", "plain", 900)]),
        );
        let r = e.query("a > b", None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].provider, "plain");
        assert!(seen(&sh).is_empty());
    }

    #[test]
    fn routing_provider_is_skipped_for_plain_text() {
        let (e, sh, _dir) = engine_with_routed(
            routed_with(">", false, vec![("r", "routed", 100)]),
            FakeProvider::new("plain", vec![("p", "plain", 50)]),
        );
        let r = e.query("ls", None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].provider, "plain");
        assert!(seen(&sh).is_empty());
    }

    /// A plain provider, then a claimer for text starting with `prefix`; both shared.
    fn engine_with_claimer(prefix: &'static str) -> (Engine, Arc<FakeProvider>, Arc<FakeProvider>, tempfile::TempDir) {
        let mut claimer = FakeProvider::new("files", vec![("c", "claimed", 100)]);
        claimer.claims = Some(prefix);
        let claimer = Arc::new(claimer);
        let plain = Arc::new(FakeProvider::new("plain", vec![("p", "plain", 900)]));
        let (e, _, dir) = engine_boxed(vec![Box::new(plain.clone()), Box::new(claimer.clone())]);
        (e, claimer, plain, dir)
    }

    #[test]
    fn claimed_text_goes_only_to_the_claiming_provider() {
        let (e, claimer, plain, _dir) = engine_with_claimer("C:\\");
        let r = e.query(" C:\\x ", None);
        assert_eq!(r.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["c"]);
        assert_eq!(seen(&claimer), ["C:\\x"]);
        assert!(seen(&plain).is_empty());
        // Other text is a plain query: the claim route is not taken.
        let r = e.query("cx", None);
        assert_eq!(r[0].id, "p");
        assert_eq!(seen(&plain), ["cx"]);
    }

    #[test]
    fn claimed_queries_allow_50_results_and_ignore_shares() {
        let mut claimer = FakeProvider::new("files", numbered("f", 60, 1));
        claimer.claims = Some("C:\\");
        claimer.share = 5;
        let (e, _, _dir) = engine(vec![claimer, FakeProvider::new("apps", vec![("a", "app", 500)])]);
        let r = e.query("C:\\x", None);
        assert_eq!(r.len(), ROUTED_MAX_RESULTS);
        assert!(r.iter().all(|i| i.provider == "files"));
    }

    #[test]
    fn scope_and_help_win_over_a_claim() {
        // This claimer claims every text.
        let (e, claimer, _, _dir) = engine_with_claimer("");
        let mut owner = FakeProvider::new("owner", vec![("o", "mine", 100)]);
        owner.scope = Some("proj");
        let claimer2 = claimer.clone();
        let (e2, _, _d2) = engine_boxed(vec![Box::new(claimer2), Box::new(owner)]);
        let r = e2.query("C:\\x", Some("proj"));
        assert_eq!(r.iter().map(|i| i.provider).collect::<Vec<_>>(), ["owner"]);
        let r = e.query("help", None);
        assert!(!r.is_empty() && r.iter().all(|i| i.provider == HELP));
        assert!(seen(&claimer).is_empty());
    }

    #[test]
    fn claimed_text_is_not_a_past_search() {
        let mut claimer = FakeProvider::new("files", vec![("x", "X", 1)]);
        claimer.claims = Some("C:\\");
        let (e, _, h, _, _dir) = engine_stores(vec![Box::new(claimer)]);
        e.activate("files", "x", Modifier::None, "C:\\x", None).unwrap();
        assert!(h.searches().is_empty());
        assert_eq!(h.recents(), [pair("files", "x")]);
        e.activate("files", "x", Modifier::None, "x", None).unwrap();
        assert_eq!(h.searches(), ["x"]);
    }

    #[test]
    fn claims_and_act_are_forwarded_through_arc() {
        let mut claimer = FakeProvider::new("files", vec![]);
        claimer.claims = Some("C:\\");
        let shared = Arc::new(claimer);
        assert!(Provider::claims(&shared, "C:\\x"));
        assert!(!Provider::claims(&shared, "x"));
        assert_eq!(Provider::act(&shared, "i", Action::Reveal), Ok(Outcome::Hide));
        assert_eq!(*shared.acted.lock().unwrap(), [("i".to_string(), Action::Reveal)]);
    }

    #[test]
    fn scope_wins_over_route() {
        let mut owner = FakeProvider::new("owner", vec![("1", "mine", 100)]);
        owner.scope = Some("proj");
        let (e, sh, _dir) = engine_with_routed(routed_with(">", false, vec![("r", "routed", 100)]), owner);
        let r = e.query("> x", Some("proj"));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].provider, "owner");
        assert!(seen(&sh).is_empty());
    }

    #[test]
    fn routed_and_scoped_queries_allow_50_results() {
        let ids: Vec<&'static str> = (0..60).map(|i| &*Box::leak(i.to_string().into_boxed_str())).collect();
        let items: Vec<Item> = ids.iter().map(|id| (*id, "t", 1u32)).collect();
        let (e, _, _dir) = engine_with_routed(
            routed_with(">", false, items.clone()),
            FakeProvider::new("plain", vec![]),
        );
        assert_eq!(e.query("> x", None).len(), ROUTED_MAX_RESULTS);
        let mut owner = FakeProvider::new("owner", items);
        owner.scope = Some("proj");
        let (e2, _, _d2) = engine(vec![owner]);
        assert_eq!(e2.query("x", Some("proj")).len(), ROUTED_MAX_RESULTS);
    }

    fn help_engine() -> (Engine, tempfile::TempDir) {
        let mut a = FakeProvider::new("apps", vec![]);
        a.help = vec![line("blender", "open an app by name", "")];
        let mut s = FakeProvider::new("settings", vec![]);
        s.help = vec![line("bluetooth", "open a Windows Settings page", ""), line("> ls", "run a command", "> ")];
        let (e, _, dir) = engine(vec![a, s]);
        (e, dir)
    }

    #[test]
    fn help_lists_every_providers_lines_in_order() {
        let (e, _dir) = help_engine();
        let r = e.query("help", None);
        let titles: Vec<_> = r.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["blender", "bluetooth", "> ls", "help", "Esc", "Ctrl+P", "Up", "Tab", "Right / Left"]);
        assert!(r.iter().all(|i| i.provider == HELP && !i.confirm));
        let scores: Vec<_> = r.iter().map(|i| i.score).collect();
        assert_eq!(scores, [1000, 999, 998, 997, 996, 995, 994, 993, 992]);
        assert_eq!(r[2].id, "> ");
        assert_eq!(r[2].subtitle, "run a command");
        assert_eq!(r[3].subtitle, "show this list");
        assert_eq!(r[4].subtitle, "close the bar");
        assert_eq!(r[5].subtitle, "pin or unpin the selected row");
        assert_eq!(r[6].subtitle, "recall past searches (empty bar)");
        assert_eq!((r[7].subtitle.as_str(), r[7].id.as_str()), ("show the actions for the selected row", ""));
    }

    #[test]
    fn help_filters_by_remainder() {
        let (e, _dir) = help_engine();
        let r = e.query("help blue", None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "bluetooth");
    }

    #[test]
    fn activating_a_help_line_fills_and_records_nothing() {
        let (e, ranking, dir) = engine(vec![FakeProvider::new("a", vec![])]);
        assert_eq!(e.activate(HELP, "> ", Modifier::None, "", None), Ok(Outcome::Fill { text: "> ".into() }));
        assert_eq!(ranking.boost(HELP, "> "), 0);
        assert!(!dir.path().join("picks.json").exists());
    }

    #[test]
    fn fill_outcome_serialises_with_kind_and_text() {
        let v = serde_json::to_value(Outcome::Fill { text: "> ".into() }).unwrap();
        assert_eq!(v, serde_json::json!({"kind": "fill", "text": "> "}));
    }

    /// Eight file-like items at 600 from a provider that may take five rows.
    fn capped_files() -> FakeProvider {
        let ids: Vec<&'static str> = (0..8).map(|i| &*Box::leak(format!("f{i}").into_boxed_str())).collect();
        let mut files = FakeProvider::new("files", ids.iter().map(|id| (*id, *id, 600u32)).collect());
        files.share = 5;
        files
    }

    #[test]
    fn shares_apply_per_page_of_eight() {
        // The first page is what a single 8-row list gave; the spill follows.
        let (e, _, _dir) = engine(vec![capped_files(), FakeProvider::new("apps", vec![("a", "app", 500)])]);
        let r = e.query("x", None);
        let rows: Vec<_> = r.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(rows, ["f0", "f1", "f2", "f3", "f4", "a", "f5", "f6", "f7"]);

        let mut files = FakeProvider::new("files", numbered("f", 20, 600));
        files.share = 5;
        let (e, _, _dir) = engine(vec![files, FakeProvider::new("apps", numbered("a", 10, 500))]);
        let r = e.query("x", None);
        assert_eq!(r.len(), 30);
        let mut apps_left = 10;
        for page in r.chunks(PAGE) {
            let files = page.iter().filter(|i| i.provider == "files").count();
            if apps_left > 0 {
                assert!(files <= 5, "{:?}", page.iter().map(|i| &i.id).collect::<Vec<_>>());
            }
            apps_left -= page.len() - files;
        }
        // Each provider's rows keep their rank order.
        let ids = |p: &str| r.iter().filter(|i| i.provider == p).map(|i| i.id.clone()).collect::<Vec<_>>();
        let expect = |prefix: &str, n: usize| numbered(prefix, n, 0).iter().map(|i| i.0.to_string()).collect::<Vec<_>>();
        assert_eq!(ids("apps"), expect("a", 10));
        assert_eq!(ids("files"), expect("f", 20));
    }

    #[test]
    fn a_capped_provider_alone_fills_every_row() {
        let (e, _, _dir) = engine(vec![capped_files(), FakeProvider::new("apps", vec![])]);
        assert_eq!(e.query("x", None).len(), 8);
    }

    #[test]
    fn routed_and_scoped_queries_ignore_shares() {
        let mut routed = capped_files();
        routed.route = Some(Route { prefix: ">", needs_space: false });
        let (e, _, _dir) = engine(vec![routed, FakeProvider::new("apps", vec![("a", "app", 500)])]);
        assert_eq!(e.query("> x", None).len(), 8);
        let mut scoped = capped_files();
        scoped.scope = Some("proj");
        let (e, _, _dir) = engine(vec![scoped]);
        assert_eq!(e.query("x", Some("proj")).len(), 8);
    }

    #[test]
    fn plain_share_is_forwarded_through_arc() {
        let shared = Arc::new(capped_files());
        assert_eq!(Provider::plain_share(&shared), 5);
        assert_eq!(Provider::plain_share(&Arc::new(FakeProvider::new("a", vec![]))), usize::MAX);
    }

    #[test]
    fn top_rows_come_first_in_plain_queries_only() {
        let mut answer = FakeProvider::new("answers", vec![("a", "= 84", 10)]);
        answer.top = true;
        answer.learn = false;
        let (e, ranking, _dir) = engine(vec![answer, FakeProvider::new("apps", vec![("x", "app", 1000)])]);
        for _ in 0..50 {
            ranking.record("apps", "x");
        }
        let r = e.query("12*7", None);
        assert_eq!((r[0].provider, r[1].provider), ("answers", "apps"));

        let mut scoped = FakeProvider::new("answers", vec![("a", "= 84", 10), ("b", "high", 900)]);
        scoped.top = true;
        scoped.scope = Some("proj");
        let (e, _, _dir) = engine(vec![scoped]);
        assert_eq!(e.query("x", Some("proj"))[0].id, "b", "scoped: score order");
    }

    #[test]
    fn open_item_records_nothing() {
        let a = Arc::new(FakeProvider::new("a", vec![("x", "Ex", 500)]));
        let (e, ranking, h, _, dir) = engine_stores(vec![Box::new(a.clone())]);
        assert_eq!(e.open_item("a", "x"), Ok(Outcome::Hide));
        assert_eq!(*a.activated.lock().unwrap(), [("x".to_string(), Modifier::None)]);
        assert_eq!(ranking.boost("a", "x"), 0);
        assert!(h.recents().is_empty() && h.searches().is_empty());
        assert!(!dir.path().join("picks.json").exists());
    }

    #[test]
    fn open_item_refuses_confirm_and_unknown() {
        let mut c = FakeProvider::new("c", vec![("off", "Shut down", 1)]);
        c.confirm = true;
        let c = Arc::new(c);
        let a = Arc::new(FakeProvider::new("a", vec![("x", "Ex", 500)]));
        let (e, _, _dir) = engine_boxed(vec![Box::new(c.clone()), Box::new(a.clone())]);
        assert_eq!(e.open_item("c", "off"), Err("needs a second Enter".to_string()));
        assert_eq!(e.open_item("a", "gone"), Err("not found".to_string()));
        assert_eq!(e.open_item("nowhere", "x"), Err("not found".to_string()));
        assert_eq!(e.open_item(HELP, "timer"), Err("not found".to_string()));
        assert!(c.activated.lock().unwrap().is_empty() && a.activated.lock().unwrap().is_empty());
    }

    /// Files on disk (`a.txt`, `b.txt`, `sub\`, `sub\inner.txt`) behind a real
    /// files provider with recording ops, and an engine over it.
    struct Disk {
        e: Engine,
        ops: Arc<crate::file_ops::FakeFileOps>,
        ranking: Arc<Ranking>,
        history: Arc<History>,
        pins: Arc<Pins>,
        dir: tempfile::TempDir,
        _stores: tempfile::TempDir,
    }

    impl Disk {
        fn id(&self, rel: &str) -> String {
            self.dir.path().join(rel).to_string_lossy().into_owned()
        }

        /// Pinned, picked and recently launched.
        fn remember(&self, id: &str) {
            self.pins.toggle("files", id);
            self.ranking.record("files", id);
            self.history.record_launch("files", id);
        }

        fn remembered(&self, id: &str) -> (bool, u32, bool) {
            (
                self.pins.list().iter().any(|(p, i)| p == "files" && i == id),
                self.ranking.boost("files", id),
                self.history.recents().iter().any(|(_, i)| i == id),
            )
        }

        fn found(&self, text: &str) -> Vec<String> {
            self.e.query(text, None).into_iter().filter(|r| r.provider == "files").map(|r| r.id).collect()
        }
    }

    fn disk() -> Disk {
        use crate::launch::testing::RecordingLauncher;
        use crate::providers::files::{FileEntry, FilesProvider};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        for f in ["a.txt", "b.txt", r"sub\inner.txt"] {
            std::fs::write(root.join(f), b"").unwrap();
        }
        let entries = vec![
            FileEntry::new(root.join("a.txt"), false),
            FileEntry::new(root.join("b.txt"), false),
            FileEntry::new(root.join("sub"), true),
            FileEntry::new(root.join(r"sub\inner.txt"), false).at_depth(2),
        ];
        let ops = Arc::new(crate::file_ops::FakeFileOps::default());
        let files = FilesProvider::with_ops(Arc::new(RecordingLauncher::default()), ops.clone(), entries);
        let (e, ranking, history, pins, stores) = engine_stores(vec![Box::new(files)]);
        Disk { e, ops, ranking, history, pins, dir, _stores: stores }
    }

    #[test]
    fn delete_recycles_and_forgets_records() {
        let d = disk();
        let (a, b) = (d.id("a.txt"), d.id("b.txt"));
        d.remember(&a);
        d.remember(&b);
        // Another provider's entry for the same file (and in another letter case) goes too.
        d.pins.toggle("path", &a.to_uppercase());
        d.ranking.record("path", &a.to_lowercase());
        assert_eq!(d.e.act("files", &a, Action::Delete), Ok(Outcome::Keep));
        assert_eq!(d.ops.calls(), [format!("recycle {a}")]);
        assert_eq!(d.remembered(&a), (false, 0, false));
        assert_eq!(d.ranking.boost("path", &a.to_lowercase()), 0);
        assert_eq!(d.pins.list(), [("files".to_string(), b.clone())]);
        assert_eq!(d.remembered(&b), (true, 15, true), "other items keep theirs");
        assert!(d.found("a.txt").is_empty(), "the index dropped it: {:?}", d.found("a.txt"));
        assert_eq!(d.found("b.txt"), [b]);
    }

    #[test]
    fn delete_missing_file_fails_and_keeps_records() {
        let d = disk();
        let a = d.id("a.txt");
        d.remember(&a);
        std::fs::remove_file(&a).unwrap();
        assert_eq!(d.e.act("files", &a, Action::Delete), Err("file not found".to_string()));
        assert!(d.ops.calls().is_empty());
        assert_eq!(d.remembered(&a), (true, 15, true));
        assert_eq!(d.found("a.txt"), [a.clone()], "the index is untouched too");
        // A failing recycle keeps them as well.
        let b = d.id("b.txt");
        d.remember(&b);
        d.ops.fail("recycle", "in use");
        assert_eq!(d.e.act("files", &b, Action::Delete), Err("in use".to_string()));
        assert_eq!(d.remembered(&b), (true, 15, true));
        assert_eq!(d.found("b.txt"), [b]);
    }

    #[test]
    fn delete_declined_in_windows_prompt_keeps_the_row_with_no_error() {
        let d = disk();
        let a = d.id("a.txt");
        d.remember(&a);
        d.ops.fail("recycle", crate::file_ops::DELETE_DECLINED);
        assert_eq!(d.e.act("files", &a, Action::Delete), Ok(Outcome::Keep));
        assert_eq!(d.ops.calls(), [format!("recycle {a}")]);
        assert_eq!(d.remembered(&a), (true, 15, true), "nothing forgotten");
        assert_eq!(d.found("a.txt"), [a]);
    }

    #[test]
    fn more_options_on_a_missing_path_errors_without_a_menu() {
        use std::path::PathBuf;
        let d = disk();
        let a = d.id("a.txt");
        let folder = format!("{}\\", d.id("sub"));
        let shown = std::cell::RefCell::new(Vec::new());
        let show = |p: &Path| {
            shown.borrow_mut().push(p.to_path_buf());
            Ok(true)
        };
        // A file and a folder still on disk: the menu is shown for the path alone.
        assert_eq!(d.e.more_options("files", &a, show), Ok(true));
        assert_eq!(d.e.more_options("files", &folder, show), Ok(true));
        assert_eq!(*shown.borrow(), [PathBuf::from(&a), PathBuf::from(d.id("sub"))]);
        shown.borrow_mut().clear();
        // Gone since the search: a short error, and no menu.
        std::fs::remove_file(&a).unwrap();
        assert_eq!(d.e.more_options("files", &a, show), Err("file not found".to_string()));
        // A folder row whose path is now a file is not that row either.
        std::fs::remove_dir_all(d.id("sub")).unwrap();
        std::fs::write(d.id("sub"), b"").unwrap();
        assert_eq!(d.e.more_options("files", &folder, show), Err("file not found".to_string()));
        // Not indexed, or another provider's id: refused before the disk is asked.
        assert_eq!(d.e.more_options("files", &d.id("zzz.txt"), show), Err("can't do that here".to_string()));
        assert_eq!(d.e.more_options("nowhere", &d.id("b.txt"), show), Err("unknown provider: nowhere".to_string()));
        assert!(shown.borrow().is_empty());
    }

    #[test]
    fn rename_moves_pins_ranking_and_history() {
        let d = disk();
        let (a, b) = (d.id("a.txt"), d.id("b.txt"));
        d.remember(&a);
        d.remember(&b);
        d.ranking.record("files", &a);
        // Case differs in the stored id: still the same item.
        d.pins.toggle("path", &a.to_uppercase());
        let new = d.e.rename("files", &a, "Ä b.txt").unwrap();
        assert_eq!(new, d.id("Ä b.txt"));
        assert_eq!(d.ops.calls(), [format!("rename {a} Ä b.txt")]);
        assert_eq!(d.remembered(&a), (false, 0, false));
        assert_eq!(d.remembered(&new), (true, 30, true));
        // The pins keep their places; the other provider's pin followed too.
        assert_eq!(
            d.pins.list(),
            [("files".to_string(), new.clone()), ("files".to_string(), b.clone()), ("path".to_string(), new.clone())]
        );
        // Recent launches keep their places as well (newest first).
        assert_eq!(d.history.recents(), [("files".to_string(), b.clone()), ("files".to_string(), new.clone())]);
        // Saved: a restart sees the same.
        assert_eq!(Pins::load(d._stores.path().join("pins.json")).list().len(), 3);
        assert_eq!(History::load(d._stores.path().join("history.json"), true).recents().len(), 2);
    }

    #[test]
    fn rename_refuses_an_existing_name() {
        let d = disk();
        let (a, b) = (d.id("a.txt"), d.id("b.txt"));
        d.remember(&a);
        assert_eq!(d.e.rename("files", &a, "b.txt"), Err("A file with that name already exists".to_string()));
        assert_eq!(d.e.rename("files", &a, "B.TXT"), Err("A file with that name already exists".to_string()));
        assert!(d.ops.calls().is_empty());
        assert_eq!(d.remembered(&a), (true, 15, true));
        assert_eq!(d.found("a.txt"), [a]);
        assert_eq!(d.found("b.txt"), [b]);
    }

    #[test]
    fn rename_allows_a_case_change() {
        let d = disk();
        let a = d.id("a.txt");
        d.remember(&a);
        let new = d.e.rename("files", &a, "A.TXT").unwrap();
        assert_eq!(new, d.id("A.TXT"));
        assert_eq!(d.ops.calls(), [format!("rename {a} A.TXT")]);
        assert_eq!(d.remembered(&new), (true, 15, true));
        assert_eq!(d.found("a.txt"), [new]);
    }

    #[test]
    fn rename_patches_the_files_index() {
        let d = disk();
        let a = d.id("a.txt");
        assert_eq!(d.found("a.txt"), [a.clone()]);
        let new = d.e.rename("files", &a, "Ä b.txt").unwrap();
        assert_eq!(d.found("Ä b"), [new]);
        assert!(d.found("a.txt").is_empty(), "{:?}", d.found("a.txt"));
        // The row is rebuilt for the new id, and the old id is gone.
        assert!(d.e.item("files", &d.id("Ä b.txt")).is_some());
        assert!(d.e.item("files", &a).is_none());
    }

    #[test]
    fn rename_of_a_folder_keeps_the_trailing_backslash() {
        let d = disk();
        let (folder, inner) = (format!("{}\\", d.id("sub")), d.id(r"sub\inner.txt"));
        d.remember(&folder);
        d.remember(&inner);
        let new = d.e.rename("files", &folder, "Sub 2").unwrap();
        assert_eq!(new, format!("{}\\", d.id("Sub 2")));
        assert_eq!(d.ops.calls(), [format!("rename {} Sub 2", d.id("sub"))]);
        assert_eq!(d.remembered(&folder), (false, 0, false));
        assert_eq!(d.remembered(&new), (true, 15, true));
        // What is inside moves with it, in the records and in the index.
        let moved_inner = d.id(r"Sub 2\inner.txt");
        assert_eq!(d.remembered(&inner), (false, 0, false));
        assert_eq!(d.remembered(&moved_inner), (true, 15, true));
        assert_eq!(d.found("inner"), [moved_inner]);
        // The files index names a folder row without the trailing backslash.
        assert_eq!(d.found("sub 2").first().map(String::as_str), Some(new.trim_end_matches('\\')));
    }

    #[test]
    fn rename_refuses_an_invalid_name_without_calling_ops() {
        let d = disk();
        let a = d.id("a.txt");
        d.remember(&a);
        for (name, why) in [
            ("", "Type a name"),
            ("a/b.txt", r#"A name can't contain \ / : * ? " < > |"#),
            ("a.", "A name can't end with a space or a dot"),
            ("con.txt", "That name is reserved by Windows"),
        ] {
            assert_eq!(d.e.rename("files", &a, name), Err(why.to_string()), "{name:?}");
        }
        assert!(d.ops.calls().is_empty());
        assert_eq!(d.remembered(&a), (true, 15, true));
        assert_eq!(d.found("a.txt"), [a.clone()]);
        // Nothing indexed under that id, or no such provider: a short error, and no call either.
        assert_eq!(d.e.rename("files", &d.id("zzz.txt"), "ok.txt"), Err("file not found".to_string()));
        assert_eq!(d.e.rename("nowhere", &a, "ok.txt"), Err("unknown provider: nowhere".to_string()));
        assert!(d.ops.calls().is_empty());
    }
}
