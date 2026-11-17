//! Chores: empty the Recycle Bin, eject a drive, flush the DNS cache,
//! restart Explorer. The bin and the drives are read on [`Loader`] threads;
//! the query path only looks at memory. Chores are never learned.
#![allow(dead_code)] // until the provider is registered (Task 13)

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::chores::{BinInfo, Chores, Drive, EjectError};
use crate::dirs::{size_text, Load, Loader, ReadError, FRESH, SLOW};
use crate::fuzzy;
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem};
use crate::providers::power::POWER_PENALTY;

const NAME: &str = "chores";

const EMPTY_BIN: &str = "empty-bin";
const FLUSH_DNS: &str = "flush-dns";
const RESTART_EXPLORER: &str = "restart-explorer";

const BIN_TITLE: &str = "Empty Recycle Bin";
const BIN_READING: &str = "Recycle Bin";
const BIN_EMPTY: &str = "Already empty";
const FLUSH_TITLE: &str = "Flush DNS cache";
const FLUSH_SUBTITLE: &str = "Clears Windows' list of looked-up addresses";
const FLUSH_DONE: &str = "DNS cache cleared";
const RESTART_TITLE: &str = "Restart Explorer";
const RESTART_SUBTITLE: &str = "Restarts the taskbar and desktop";

const READING_DRIVES: &str = "Reading drives\u{2026}";
const NO_DRIVES: &str = "No removable drives";
const EJECT_SUBTITLE: &str = "Eject safely";
const UNLABELLED: &str = "USB drive";

const ERR_BIN: &str = "Could not empty the Recycle Bin";
const ERR_DNS: &str = "Could not clear the DNS cache";
const ERR_EXPLORER: &str = "Could not restart Explorer";
const ERR_IN_USE: &str = "Drive is in use";

/// How long the DNS row says it was cleared.
const CLEARED_FOR: Duration = Duration::from_secs(10);

/// (id, words typed to find it)
const KEYWORDS: [(&str, &[&str]); 3] = [
    (EMPTY_BIN, &["empty bin", "empty recycle bin", "recycle bin", "empty trash"]),
    (FLUSH_DNS, &["flush dns", "clear dns", "dns"]),
    (RESTART_EXPLORER, &["restart explorer", "restart taskbar"]),
];

pub struct ChoresProvider {
    chores: Arc<dyn Chores>,
    on_ready: Arc<dyn Fn() + Send + Sync>,
    bin: Arc<Loader<BinInfo>>,
    drives: Arc<Loader<Vec<Drive>>>,
    /// When the DNS cache was last cleared from here.
    cleared_at: Mutex<Option<Instant>>,
    cleared_for: Duration,
}

impl ChoresProvider {
    pub fn new(chores: Arc<dyn Chores>, on_ready: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::build(chores, on_ready, CLEARED_FOR)
    }

    #[cfg(test)]
    pub fn with_cleared_for(chores: Arc<dyn Chores>, on_ready: Arc<dyn Fn() + Send + Sync>, cleared_for: Duration) -> Self {
        Self::build(chores, on_ready, cleared_for)
    }

    fn build(chores: Arc<dyn Chores>, on_ready: Arc<dyn Fn() + Send + Sync>, cleared_for: Duration) -> Self {
        Self {
            bin: Loader::new(Arc::clone(&on_ready), FRESH, SLOW),
            drives: Loader::new(Arc::clone(&on_ready), FRESH, SLOW),
            chores,
            on_ready,
            cleared_at: Mutex::new(None),
            cleared_for,
        }
    }

    fn bin_subtitle(&self) -> String {
        let chores = Arc::clone(&self.chores);
        match self.bin.get("bin", move || chores.bin().map_err(|_| ReadError::Denied)) {
            Load::Ready(info) => bin_text(&info),
            Load::Pending | Load::Slow | Load::NotFound | Load::Denied => BIN_READING.to_string(),
        }
    }

    fn cleared(&self) -> bool {
        let at = *self.cleared_at.lock().unwrap_or_else(|e| e.into_inner());
        at.is_some_and(|at| at.elapsed() < self.cleared_for)
    }

    /// The row for one of the three chores, score 0.
    fn chore_row(&self, id: &str) -> Option<ResultItem> {
        let (title, subtitle, confirm, icon) = match id {
            EMPTY_BIN => (BIN_TITLE.to_string(), self.bin_subtitle(), true, "builtin:bin"),
            FLUSH_DNS => {
                let title = if self.cleared() { FLUSH_DONE } else { FLUSH_TITLE };
                (title.to_string(), FLUSH_SUBTITLE.to_string(), false, "builtin:network")
            }
            RESTART_EXPLORER => (RESTART_TITLE.to_string(), RESTART_SUBTITLE.to_string(), true, "builtin:folder"),
            _ => return None,
        };
        Some(ResultItem {
            id: id.to_string(),
            provider: NAME,
            title,
            subtitle,
            confirm,
            icon: icon.into(),
            pinnable: true,
            ..Default::default()
        })
    }

    fn eject_rows(&self, filter: &str) -> Vec<ResultItem> {
        let chores = Arc::clone(&self.chores);
        let drives = match self.drives.get("drives", move || chores.drives().map_err(|_| ReadError::Denied)) {
            Load::Ready(drives) => drives,
            Load::Pending => return vec![inert(READING_DRIVES)],
            // A failed read is not kept: the next `eject` reads again.
            Load::NotFound | Load::Denied => {
                self.drives.clear();
                return vec![inert(NO_DRIVES)];
            }
            Load::Slow => return vec![inert(READING_DRIVES)],
        };
        let rows: Vec<ResultItem> = drives
            .iter()
            .filter_map(|d| drive_score(d, filter).map(|score| drive_row(d, score)))
            .collect();
        if rows.is_empty() {
            vec![inert(NO_DRIVES)]
        } else {
            rows
        }
    }
}

fn bin_text(info: &BinInfo) -> String {
    match info.items {
        0 => BIN_EMPTY.to_string(),
        1 => format!("1 item, {}", size_text(info.bytes)),
        n => format!("{n} items, {}", size_text(info.bytes)),
    }
}

fn inert(title: &str) -> ResultItem {
    ResultItem {
        id: format!("inert:{title}"),
        provider: NAME,
        title: title.to_string(),
        icon: "builtin:drive".into(),
        ..Default::default()
    }
}

fn drive_title(d: &Drive) -> String {
    let label = if d.label.trim().is_empty() { UNLABELLED } else { d.label.trim() };
    format!("{label} ({}:)", d.letter.to_ascii_uppercase())
}

/// How well `filter` (the text after `eject`) picks `d`: everything when
/// empty; a lone letter (`f`, `F:`, `f:\`) only that drive; else its label.
fn drive_score(d: &Drive, filter: &str) -> Option<u32> {
    if filter.is_empty() {
        return Some(0);
    }
    let bare = filter.trim_end_matches(['\\', '/']).trim_end_matches(':');
    let mut chars = bare.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            return c.eq_ignore_ascii_case(&d.letter).then_some(1000);
        }
    }
    let label = if d.label.trim().is_empty() { UNLABELLED } else { d.label.trim() };
    fuzzy::score(filter, label)
}

fn drive_row(d: &Drive, score: u32) -> ResultItem {
    ResultItem {
        id: format!("eject:{}", d.letter.to_ascii_uppercase()),
        provider: NAME,
        title: drive_title(d),
        subtitle: EJECT_SUBTITLE.to_string(),
        score,
        icon: "builtin:drive".into(),
        ..Default::default()
    }
}

/// The text after a leading `eject` word, if `text` starts with one.
fn eject_filter(text: &str) -> Option<&str> {
    let text = text.trim();
    let head = text.get(..5)?;
    if !head.eq_ignore_ascii_case("eject") {
        return None;
    }
    let rest = &text[5..];
    if rest.is_empty() {
        return Some("");
    }
    rest.starts_with(' ').then(|| rest.trim())
}

impl Provider for ChoresProvider {
    fn name(&self) -> &'static str {
        NAME
    }

    /// Memory only: the bin and the drives are read on loader threads.
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if let Some(filter) = eject_filter(text) {
            return self.eject_rows(filter);
        }
        if text.is_empty() {
            return Vec::new();
        }
        let mut rows: Vec<ResultItem> = KEYWORDS
            .iter()
            .filter_map(|(id, words)| {
                let best = words.iter().filter_map(|w| fuzzy::score(text, w)).max()?;
                let mut row = self.chore_row(id)?;
                // Like power actions: an app matching the same text ranks first.
                row.score = best.saturating_sub(POWER_PENALTY).max(1);
                Some(row)
            })
            .collect();
        rows.sort_by(|a, b| b.score.cmp(&a.score));
        rows
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        match id {
            EMPTY_BIN => {
                self.chores.empty_bin().map_err(|_| ERR_BIN.to_string())?;
                self.bin.clear();
                Ok(Outcome::Hide)
            }
            FLUSH_DNS => {
                self.chores.flush_dns().map_err(|_| ERR_DNS.to_string())?;
                *self.cleared_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                (self.on_ready)();
                Ok(Outcome::Keep)
            }
            RESTART_EXPLORER => {
                self.chores.restart_explorer().map_err(|_| ERR_EXPLORER.to_string())?;
                Ok(Outcome::Hide)
            }
            _ if id.starts_with("inert:") => Ok(Outcome::Keep),
            _ => {
                let letter = id
                    .strip_prefix("eject:")
                    .and_then(|l| {
                        let mut chars = l.chars();
                        chars.next().filter(|c| c.is_ascii_alphabetic() && chars.next().is_none())
                    })
                    .ok_or_else(|| "unknown chore".to_string())?;
                match self.chores.eject(letter) {
                    Ok(()) => {
                        self.drives.clear();
                        Ok(Outcome::Hide)
                    }
                    Err(EjectError::InUse) => Err(ERR_IN_USE.to_string()),
                    Err(EjectError::Failed) => Err(format!("Could not eject {letter}:")),
                }
            }
        }
    }

    /// Chores are never learned, recorded or saved as searches.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn help(&self) -> Vec<HelpLine> {
        let line = |example: &str, about: &str, fill: &str| HelpLine {
            example: example.into(),
            about: about.into(),
            fill: fill.into(),
        };
        vec![
            line("empty bin", "empty the Recycle Bin (asks first)", "empty bin"),
            line("eject / eject E:", "safely remove a USB drive", "eject"),
            line("flush dns", "clear Windows' DNS cache", "flush dns"),
            line("restart explorer", "restart the taskbar and desktop (asks first)", "restart explorer"),
        ]
    }

    /// The three chores can be pinned or given a hotkey; drive rows cannot.
    fn item(&self, id: &str) -> Option<ResultItem> {
        self.chore_row(id)
    }

    fn claims(&self, text: &str) -> bool {
        eject_filter(text).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chores::testing::FakeChores;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc::{self, Receiver};

    struct T {
        p: ChoresProvider,
        fake: Arc<FakeChores>,
        rx: Receiver<()>,
        readies: Arc<AtomicUsize>,
    }

    fn setup_with(cleared_for: Duration) -> T {
        let fake = Arc::new(FakeChores::default());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let readies = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&readies);
        let p = ChoresProvider::with_cleared_for(
            fake.clone(),
            Arc::new(move || {
                count.fetch_add(1, SeqCst);
                let _ = tx.lock().unwrap().send(());
            }),
            cleared_for,
        );
        T { p, fake, rx, readies }
    }

    fn setup() -> T {
        setup_with(CLEARED_FOR)
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    fn row(t: &T, text: &str, id: &str) -> ResultItem {
        t.p.query(&q(text)).into_iter().find(|r| r.id == id).unwrap_or_else(|| panic!("no {id} row for {text:?}"))
    }

    fn wait(t: &T) {
        t.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready within 1 s");
    }

    /// The eject rows once the drives are read.
    fn eject_rows(t: &T, text: &str) -> Vec<ResultItem> {
        let rows = t.p.query(&q(text));
        if rows.first().is_some_and(|r| r.title == READING_DRIVES) {
            wait(t);
            return t.p.query(&q(text));
        }
        rows
    }

    fn drive(letter: char, label: &str) -> Drive {
        Drive { letter, label: label.into() }
    }

    #[test]
    fn empty_bin_needs_a_second_enter_and_shows_the_count() {
        let t = setup();
        t.fake.set_bin(Ok(BinInfo { items: 12, bytes: 356_515_840 }));
        let r = row(&t, "empty bin", EMPTY_BIN);
        assert_eq!(r.title, "Empty Recycle Bin");
        assert!(r.confirm);
        assert_eq!(r.icon, "builtin:bin");
        assert_eq!(r.subtitle, "Recycle Bin");
        wait(&t);
        assert_eq!(row(&t, "empty bin", EMPTY_BIN).subtitle, "12 items, 340 MB");
        assert_eq!(bin_text(&BinInfo { items: 1, bytes: 12 }), "1 item, 12 B");
        for text in ["empty recycle bin", "recycle bin", "empty trash"] {
            assert_eq!(row(&t, text, EMPTY_BIN).title, "Empty Recycle Bin", "{text}");
        }
    }

    #[test]
    fn an_empty_bin_says_already_empty() {
        let t = setup();
        row(&t, "empty bin", EMPTY_BIN);
        wait(&t);
        assert_eq!(row(&t, "empty bin", EMPTY_BIN).subtitle, "Already empty");
    }

    #[test]
    fn enter_on_empty_bin_empties_without_a_dialog() {
        let t = setup();
        assert_eq!(t.p.activate(EMPTY_BIN, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(t.fake.calls(), vec!["empty_bin".to_string()]);
        t.fake.set_empty(Err("nope".into()));
        assert_eq!(t.p.activate(EMPTY_BIN, Modifier::None), Err("Could not empty the Recycle Bin".to_string()));
    }

    #[test]
    fn restart_explorer_needs_a_second_enter() {
        let t = setup();
        for text in ["restart explorer", "restart taskbar"] {
            let r = row(&t, text, RESTART_EXPLORER);
            assert_eq!(r.title, "Restart Explorer");
            assert_eq!(r.subtitle, "Restarts the taskbar and desktop");
            assert_eq!(r.icon, "builtin:folder");
            assert!(r.confirm);
        }
        assert_eq!(t.p.activate(RESTART_EXPLORER, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(t.fake.calls(), vec!["restart_explorer".to_string()]);
        t.fake.set_restart(Err("nope".into()));
        assert_eq!(t.p.activate(RESTART_EXPLORER, Modifier::None), Err("Could not restart Explorer".to_string()));
    }

    #[test]
    fn flush_dns_runs_on_one_enter_then_says_cleared() {
        let t = setup_with(Duration::from_millis(50));
        for text in ["flush dns", "clear dns", "dns"] {
            let r = row(&t, text, FLUSH_DNS);
            assert_eq!(r.title, "Flush DNS cache");
            assert_eq!(r.subtitle, "Clears Windows' list of looked-up addresses");
            assert_eq!(r.icon, "builtin:network");
            assert!(!r.confirm);
        }
        assert_eq!(t.readies.load(SeqCst), 0);
        assert_eq!(t.p.activate(FLUSH_DNS, Modifier::None), Ok(Outcome::Keep));
        assert_eq!(t.readies.load(SeqCst), 1, "on_ready once");
        assert_eq!(t.fake.calls(), vec!["flush_dns".to_string()]);
        assert_eq!(row(&t, "flush dns", FLUSH_DNS).title, "DNS cache cleared");
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(row(&t, "flush dns", FLUSH_DNS).title, "Flush DNS cache");
        t.fake.set_flush(Err("nope".into()));
        assert_eq!(t.p.activate(FLUSH_DNS, Modifier::None), Err("Could not clear the DNS cache".to_string()));
    }

    #[test]
    fn eject_lists_removable_drives_with_label_and_letter() {
        let t = setup();
        t.fake.set_drives(Ok(vec![drive('E', "STICK"), drive('F', "")]));
        assert_eq!(t.p.query(&q("eject"))[0].title, "Reading drives\u{2026}");
        wait(&t);
        let rows = t.p.query(&q("eject"));
        let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["STICK (E:)", "USB drive (F:)"]);
        assert_eq!(rows[0].id, "eject:E");
        assert_eq!(rows[0].subtitle, "Eject safely");
        assert_eq!(rows[0].icon, "builtin:drive");
        assert!(!rows[0].confirm);
        let only_f = eject_rows(&t, "eject f");
        assert_eq!(only_f.len(), 1);
        assert_eq!(only_f[0].id, "eject:F");
        assert_eq!(eject_rows(&t, "eject F:")[0].id, "eject:F");
        let by_label = eject_rows(&t, "eject stick");
        assert_eq!(by_label.len(), 1);
        assert_eq!(by_label[0].id, "eject:E");
        assert_eq!(t.p.activate("eject:E", Modifier::None), Ok(Outcome::Hide));
        assert!(t.fake.calls().contains(&"eject:E".to_string()));
        t.fake.set_eject(Err(EjectError::Failed));
        assert_eq!(t.p.activate("eject:E", Modifier::None), Err("Could not eject E:".to_string()));
    }

    #[test]
    fn a_drive_in_use_says_so() {
        let t = setup();
        t.fake.set_eject(Err(EjectError::InUse));
        assert_eq!(t.p.activate("eject:E", Modifier::None), Err("Drive is in use".to_string()));
    }

    #[test]
    fn no_removable_drives_says_so() {
        let t = setup();
        let rows = eject_rows(&t, "eject");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "No removable drives");
        assert_eq!(t.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Keep));
        let t2 = setup();
        t2.fake.set_drives(Ok(vec![drive('E', "STICK")]));
        assert_eq!(eject_rows(&t2, "eject g")[0].title, "No removable drives");
    }

    #[test]
    fn query_never_waits_for_the_recycle_bin_or_drives() {
        let t = setup();
        t.fake.block();
        let start = Instant::now();
        let bin = row(&t, "empty bin", EMPTY_BIN);
        let drives = t.p.query(&q("eject"));
        let item = t.p.item(EMPTY_BIN);
        let took = start.elapsed();
        t.fake.release();
        assert!(took < Duration::from_millis(20), "{took:?}");
        assert_eq!(bin.subtitle, "Recycle Bin");
        assert_eq!(drives[0].title, "Reading drives\u{2026}");
        assert!(item.is_some());
    }

    #[test]
    fn chores_are_never_learned_but_can_get_a_hotkey() {
        let t = setup();
        for id in [EMPTY_BIN, FLUSH_DNS, RESTART_EXPLORER, "eject:E"] {
            assert_eq!(t.p.rank_key(id), None, "{id}");
        }
        let bin = t.p.item(EMPTY_BIN).expect("empty-bin item");
        assert!(bin.confirm && bin.pinnable);
        assert_eq!(bin.score, 0);
        assert_eq!(bin.title, "Empty Recycle Bin");
        let dns = t.p.item(FLUSH_DNS).expect("flush-dns item");
        assert!(!dns.confirm && dns.pinnable);
        let explorer = t.p.item(RESTART_EXPLORER).expect("restart-explorer item");
        assert!(explorer.confirm && explorer.pinnable);
        assert!(t.p.item("eject:E").is_none());
    }

    #[test]
    fn only_eject_is_claimed() {
        let t = setup();
        assert!(t.p.claims("eject"));
        assert!(t.p.claims("Eject e"));
        assert!(t.p.claims("EJECT"));
        assert!(!t.p.claims("ejector"));
        assert!(!t.p.claims("empty bin"));
        assert!(!t.p.claims("flush dns"));
    }

    #[test]
    fn chores_score_below_equal_app_match() {
        let t = setup();
        assert_eq!(row(&t, "flush dns", FLUSH_DNS).score, 1000 - POWER_PENALTY);
        assert_eq!(row(&t, "restart explorer", RESTART_EXPLORER).score, 1000 - POWER_PENALTY);
        for text in ["d", "e", "r", "c", "t"] {
            for r in t.p.query(&q(text)) {
                assert!(r.score <= 900 - POWER_PENALTY, "{text}: {} scored {}", r.id, r.score);
            }
        }
    }

    #[test]
    fn name_and_help() {
        let t = setup();
        assert_eq!(t.p.name(), "chores");
        let help = t.p.help();
        let examples: Vec<&str> = help.iter().map(|h| h.example.as_str()).collect();
        assert_eq!(examples, ["empty bin", "eject / eject E:", "flush dns", "restart explorer"]);
        assert_eq!(help[0].about, "empty the Recycle Bin (asks first)");
        assert_eq!(help[1].fill, "eject");
        assert_eq!(help[3].about, "restart the taskbar and desktop (asks first)");
    }
}
