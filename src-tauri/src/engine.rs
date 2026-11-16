//! Engine: fans a query out to providers, merges, ranks and truncates.

use std::sync::Arc;

use crate::fuzzy;
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem, Route};
use crate::ranking::Ranking;

pub const MAX_RESULTS: usize = 8;
/// Cap for routed and scoped queries (file lists, shell history, help).
pub const ROUTED_MAX_RESULTS: usize = 50;
/// Provider name of help items, and the word that routes to the help list.
pub const HELP: &str = "help";

const HELP_ROUTE: Route = Route { prefix: HELP, needs_space: true };

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
}

impl Engine {
    pub fn new(providers: Vec<Box<dyn Provider>>, ranking: Arc<Ranking>) -> Self {
        Self { providers, ranking }
    }

    /// In-memory only; sorted by score + boost. Plain queries return at most
    /// `MAX_RESULTS`; routed and scoped ones `ROUTED_MAX_RESULTS`. Empty text
    /// returns nothing, except inside a scope, where it lists the scope's choices.
    ///
    /// A scope wins over routes. Otherwise text that matches a provider's route
    /// (or the built-in `help` route) goes to that provider alone, with the
    /// prefix removed; providers that own a route never see plain text.
    pub fn query(&self, text: &str, scope: Option<&str>) -> Vec<ResultItem> {
        let untrimmed_empty = text.is_empty();
        let text = text.trim();
        if let Some(scope) = scope {
            let q = Query { text: text.to_string(), scope: Some(scope.to_string()) };
            let owners = self.providers.iter().filter(|p| p.owns_scope(scope));
            return self.rank(owners, &q, ROUTED_MAX_RESULTS, false);
        }
        if text.is_empty() {
            // Only the unscoped, truly empty bar (not whitespace) asks the providers
            // for their rows, in registration order.
            if untrimmed_empty {
                return self.providers.iter().flat_map(|p| p.empty_query()).collect();
            }
            return vec![];
        }
        if let Some(rest) = strip_route(text, &HELP_ROUTE) {
            return self.help_items(rest);
        }
        for p in &self.providers {
            if let Some(rest) = p.route().and_then(|r| strip_route(text, &r)) {
                let q = Query { text: rest.to_string(), scope: None };
                return self.rank(std::iter::once(p), &q, ROUTED_MAX_RESULTS, false);
            }
        }
        let q = Query { text: text.to_string(), scope: None };
        self.rank(self.providers.iter().filter(|p| p.route().is_none()), &q, MAX_RESULTS, true)
    }

    /// Sorted by score + boost, cut to `max`. With `shares`, each provider
    /// keeps at most its [`Provider::plain_share`] rows among the others'
    /// results; its further items fill the rows left over, after them.
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
                p.query(q).into_iter().map(move |item| {
                    let boost = p.rank_key(&item.id).map_or(0, |key| self.ranking.boost(p.name(), &key));
                    (item.score.saturating_add(boost), share, item)
                })
            })
            .collect();
        ranked.sort_by(|(sa, _, a), (sb, _, b)| {
            sb.cmp(sa)
                .then_with(|| a.title.chars().count().cmp(&b.title.chars().count()))
                .then_with(|| a.title.cmp(&b.title))
        });
        if !shares {
            ranked.truncate(max);
            return ranked.into_iter().map(|(_, _, item)| item).collect();
        }
        let mut taken: std::collections::HashMap<&'static str, usize> = Default::default();
        let (mut kept, mut spill) = (Vec::new(), Vec::new());
        for (_, share, item) in ranked {
            let n = taken.entry(item.provider).or_default();
            if *n < share {
                *n += 1;
                kept.push(item);
            } else {
                spill.push(item);
            }
            if kept.len() == max {
                break;
            }
        }
        let room = max - kept.len();
        kept.extend(spill.into_iter().take(room));
        kept
    }

    /// Every provider's help lines in registration order, then the engine's own,
    /// filtered by `filter` and kept in that order (not score order).
    fn help_items(&self, filter: &str) -> Vec<ResultItem> {
        let own = [
            HelpLine { example: "help".into(), about: "show this list".into(), fill: String::new() },
            HelpLine { example: "Esc".into(), about: "close the bar".into(), fill: String::new() },
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

    /// The row `provider` would return for `id`, if it can be rebuilt.
    #[allow(dead_code)] // called by the pins and menu tasks
    pub fn item(&self, provider: &str, id: &str) -> Option<ResultItem> {
        self.provider(provider)?.item(id)
    }

    /// False while the provider has not finished its first scan. An unknown
    /// provider has nothing to wait for.
    #[allow(dead_code)] // called by the pins and menu tasks
    pub fn ready(&self, provider: &str) -> bool {
        self.provider(provider).is_none_or(|p| p.ready())
    }

    #[allow(dead_code)] // called by the pins and menu tasks
    pub fn reveal(&self, provider: &str, id: &str) -> Result<(), String> {
        self.provider(provider).ok_or_else(|| format!("unknown provider: {provider}"))?.reveal(id)
    }

    pub fn activate(&self, provider: &str, id: &str, m: Modifier) -> Result<Outcome, String> {
        // Help lines are not a provider: picking one only fills the bar, and is never learned.
        if provider == HELP {
            return Ok(Outcome::Fill { text: id.to_string() });
        }
        let p = self
            .providers
            .iter()
            .find(|p| p.name() == provider)
            .ok_or_else(|| format!("unknown provider: {provider}"))?;
        let outcome = p.activate(id, m)?;
        if let Some(key) = p.rank_key(id) {
            self.ranking.record(provider, &key);
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
        /// `Query.text` of every query this provider received.
        seen: std::sync::Mutex<Vec<String>>,
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
                seen: Default::default(),
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
        fn item(&self, id: &str) -> Option<ResultItem> {
            self.items.iter().find(|(i, _, _)| *i == id).map(|(id, title, _)| ResultItem {
                id: id.to_string(),
                provider: self.name,
                title: title.to_string(),
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
                    ..Default::default()
                })
                .collect()
        }
        fn rank_key(&self, id: &str) -> Option<String> {
            self.learn.then(|| id.to_string())
        }
        fn activate(&self, _id: &str, _m: Modifier) -> Result<Outcome, String> {
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
        let dir = tempfile::tempdir().unwrap();
        let ranking = Arc::new(Ranking::load(dir.path().join("picks.json")));
        (Engine::new(providers, ranking.clone()), ranking, dir)
    }

    #[test]
    fn engine_item_and_reveal_route_to_the_provider() {
        let (e, _, _dir) = engine_boxed(vec![Box::new(FakeProvider::new("a", vec![("x", "Ex", 500)]))]);
        let row = e.item("a", "x").unwrap();
        assert_eq!((row.id.as_str(), row.title.as_str(), row.provider, row.score), ("x", "Ex", "a", 0));
        assert!(e.item("a", "nope").is_none());
        assert!(e.item("nope", "x").is_none());
        assert!(e.ready("a"));
        assert!(e.reveal("nope", "x").is_err());
        assert!(e.reveal("a", "x").is_err());
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

    #[test]
    fn truncates_to_max_results() {
        let items: Vec<Item> = vec![
            ("0", "t", 1), ("1", "t", 1), ("2", "t", 1), ("3", "t", 1), ("4", "t", 1),
            ("5", "t", 1), ("6", "t", 1), ("7", "t", 1), ("8", "t", 1), ("9", "t", 1),
            ("10", "t", 1), ("11", "t", 1), ("12", "t", 1), ("13", "t", 1), ("14", "t", 1),
            ("15", "t", 1), ("16", "t", 1), ("17", "t", 1), ("18", "t", 1), ("19", "t", 1),
        ];
        let (e, _, _dir) = engine(vec![FakeProvider::new("a", items)]);
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
        assert!(e.activate("nope", "1", Modifier::None).is_err());
    }

    #[test]
    fn activate_records_pick() {
        let (e, ranking, _dir) = engine(vec![FakeProvider::new("a", vec![("1", "t", 1)])]);
        assert_eq!(ranking.boost("a", "1"), 0);
        assert_eq!(e.activate("a", "1", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(ranking.boost("a", "1"), 15);
        assert_eq!(ranking.boost("a", "2"), 0);
    }

    #[test]
    fn picked_item_outranks_equal_score_unpicked() {
        // Same score, "aa" would win the alphabetical tie-break; a pick on "zz" must beat it.
        let (e, _, _dir) = engine(vec![FakeProvider::new("a", vec![("1", "aa", 500), ("2", "zz", 500)])]);
        assert_eq!(e.query("x", None)[0].id, "1");
        e.activate("a", "2", Modifier::None).unwrap();
        assert_eq!(e.query("x", None)[0].id, "2");
    }

    #[test]
    fn step_outcome_passes_through() {
        let mut p = FakeProvider::new("a", vec![]);
        p.step = true;
        let (e, _, _dir) = engine(vec![p]);
        assert_eq!(
            e.activate("a", "1", Modifier::None),
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
        assert_eq!(e.activate("power", "sleep", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(ranking.boost("power", "sleep"), 0);
        assert!(!dir.path().join("picks.json").exists());
    }

    #[test]
    fn rank_key_is_forwarded_through_arc() {
        let mut p = FakeProvider::new("power", vec![("sleep", "Sleep", 700)]);
        p.learn = false;
        let (e, ranking, dir) = engine_boxed(vec![Box::new(Arc::new(p))]);
        e.activate("power", "sleep", Modifier::None).unwrap();
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
            assert_eq!(eng.activate("a", "1", Modifier::CtrlShift), Err(e.to_string()));
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
        assert_eq!(all.len(), 5);
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
        let mut owner = FakeProvider::new("owner", items.clone());
        owner.scope = Some("proj");
        let (e2, _, _d2) = engine(vec![owner]);
        assert_eq!(e2.query("x", Some("proj")).len(), ROUTED_MAX_RESULTS);
        let (e3, _, _d3) = engine(vec![FakeProvider::new("plain", items)]);
        assert_eq!(e3.query("x", None).len(), MAX_RESULTS);
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
        assert_eq!(titles, ["blender", "bluetooth", "> ls", "help", "Esc"]);
        assert!(r.iter().all(|i| i.provider == HELP && !i.confirm));
        let scores: Vec<_> = r.iter().map(|i| i.score).collect();
        assert_eq!(scores, [1000, 999, 998, 997, 996]);
        assert_eq!(r[2].id, "> ");
        assert_eq!(r[2].subtitle, "run a command");
        assert_eq!(r[3].subtitle, "show this list");
        assert_eq!(r[4].subtitle, "close the bar");
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
        assert_eq!(e.activate(HELP, "> ", Modifier::None), Ok(Outcome::Fill { text: "> ".into() }));
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
    fn a_capped_provider_leaves_rows_for_others_then_fills_the_rest() {
        let (e, _, _dir) = engine(vec![capped_files(), FakeProvider::new("apps", vec![("a", "app", 500)])]);
        let r = e.query("x", None);
        let rows: Vec<_> = r.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(rows, ["f0", "f1", "f2", "f3", "f4", "a", "f5", "f6"]);
    }

    #[test]
    fn a_capped_provider_alone_fills_every_row() {
        let (e, _, _dir) = engine(vec![capped_files(), FakeProvider::new("apps", vec![])]);
        assert_eq!(e.query("x", None).len(), MAX_RESULTS);
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
}
