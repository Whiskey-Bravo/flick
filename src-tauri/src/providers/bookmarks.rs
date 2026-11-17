//! Firefox bookmarks as search results. The list is read on a background
//! thread from a copy of Firefox's database and swapped in; queries only
//! look at memory.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::bookmarks::{host_of, Bookmark, BookmarkSource};
use crate::clipboard::Clipboard;
use crate::fuzzy::{normalize, score_prenormalized};
use crate::launch::Launcher;
use crate::provider::{Action, Modifier, Outcome, Provider, Query, ResultItem};

/// A new read starts when the last attempt is older than this.
pub const STALE: Duration = Duration::from_secs(300);
const ICON: &str = "builtin:bookmark";
/// Host matches rank below title matches.
const HOST_PENALTY: u32 = 100;

struct Entry {
    bookmark: Bookmark,
    host: String,
    title_n: String,
    host_n: String,
}

#[derive(Default)]
struct State {
    list: Arc<Vec<Entry>>,
    last_attempt: Option<Instant>,
    running: bool,
    /// A forced read was asked for while one was running: read once more after it.
    again: bool,
}

pub struct BookmarksProvider {
    source: Arc<dyn BookmarkSource>,
    launcher: Arc<dyn Launcher>,
    clipboard: Arc<dyn Clipboard>,
    now: Arc<dyn Fn() -> Instant + Send + Sync>,
    state: Mutex<State>,
    ready: AtomicBool,
}

impl BookmarksProvider {
    pub fn new(source: Arc<dyn BookmarkSource>, launcher: Arc<dyn Launcher>, clipboard: Arc<dyn Clipboard>) -> Arc<Self> {
        Self::build(source, launcher, clipboard, Arc::new(Instant::now))
    }

    #[cfg(test)]
    pub fn with_clock(
        source: Arc<dyn BookmarkSource>,
        launcher: Arc<dyn Launcher>,
        clipboard: Arc<dyn Clipboard>,
        now: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Arc<Self> {
        Self::build(source, launcher, clipboard, now)
    }

    fn build(
        source: Arc<dyn BookmarkSource>,
        launcher: Arc<dyn Launcher>,
        clipboard: Arc<dyn Clipboard>,
        now: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self { source, launcher, clipboard, now, state: Mutex::default(), ready: AtomicBool::new(false) })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reads the bookmarks now (blocks). An error keeps the last list.
    pub fn refresh(&self) {
        let read = self.source.read();
        let mut state = self.state();
        state.last_attempt = Some((self.now)());
        if let Ok(bookmarks) = read {
            state.list = Arc::new(
                bookmarks
                    .into_iter()
                    .map(|bookmark| {
                        let host = host_of(&bookmark.url);
                        Entry { title_n: normalize(&bookmark.title), host_n: normalize(&host), host, bookmark }
                    })
                    .collect(),
            );
        }
        drop(state);
        self.ready.store(true, Ordering::Release);
    }

    /// Starts one background read when the last attempt is older than
    /// [`STALE`] and none is running.
    pub fn refresh_if_stale(self: &Arc<Self>) {
        {
            let mut state = self.state();
            let now = (self.now)();
            let fresh = state.last_attempt.is_some_and(|at| now.saturating_duration_since(at) < STALE);
            if state.running || fresh {
                return;
            }
            state.running = true;
            state.last_attempt = Some(now);
        }
        self.spawn_reads();
    }

    /// Starts a background read now, however fresh the list is (the tray's
    /// Reload). With one already running it does not start a second, which
    /// would copy Firefox's database to the same temp file: that one reads
    /// once more when it is done.
    pub fn refresh_now(self: &Arc<Self>) {
        {
            let mut state = self.state();
            if state.running {
                state.again = true;
                return;
            }
            state.running = true;
        }
        self.spawn_reads();
    }

    /// The one reading thread (the caller has set `running`): reads, then
    /// again while a forced read was asked for meanwhile.
    fn spawn_reads(self: &Arc<Self>) {
        let this = Arc::clone(self);
        std::thread::spawn(move || loop {
            this.refresh();
            let mut state = this.state();
            if !std::mem::take(&mut state.again) {
                state.running = false;
                return;
            }
        });
    }
}

fn row(e: &Entry, score: u32) -> ResultItem {
    ResultItem {
        id: e.bookmark.url.clone(),
        provider: "bookmarks",
        title: e.bookmark.title.clone(),
        subtitle: e.host.clone(),
        score,
        icon: ICON.into(),
        pinnable: true,
        actions: vec![Action::Copy],
        ..Default::default()
    }
}

impl Provider for BookmarksProvider {
    fn name(&self) -> &'static str {
        "bookmarks"
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let needle = normalize(q.text.trim());
        if needle.is_empty() {
            return vec![];
        }
        let list = self.state().list.clone();
        list.iter()
            .filter_map(|e| {
                let score = score_prenormalized(&needle, &e.title_n)
                    .or_else(|| score_prenormalized(&needle, &e.host_n).map(|s| s.saturating_sub(HOST_PENALTY)))?;
                Some(row(e, score))
            })
            .collect()
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        self.launcher.open(id)?;
        Ok(Outcome::Hide)
    }

    fn act(&self, id: &str, action: Action) -> Result<Outcome, String> {
        match action {
            Action::Copy => {
                self.clipboard.set_text(id)?;
                Ok(Outcome::Hide)
            }
            _ => Err("can't do that here".to_string()),
        }
    }

    fn plain_share(&self) -> usize {
        3
    }

    fn item(&self, id: &str) -> Option<ResultItem> {
        let list = self.state().list.clone();
        list.iter().find(|e| e.bookmark.url == id).map(|e| row(e, 0))
    }

    fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::FakeClipboard;
    use crate::launch::testing::RecordingLauncher;
    use std::sync::atomic::AtomicUsize;

    struct Fake {
        reads: AtomicUsize,
        results: Mutex<Vec<Result<Vec<Bookmark>, String>>>,
    }

    impl Fake {
        fn new(results: Vec<Result<Vec<Bookmark>, String>>) -> Arc<Self> {
            Arc::new(Self { reads: AtomicUsize::new(0), results: Mutex::new(results) })
        }
    }

    impl BookmarkSource for Fake {
        fn read(&self) -> Result<Vec<Bookmark>, String> {
            let n = self.reads.fetch_add(1, Ordering::SeqCst);
            let results = self.results.lock().unwrap();
            results[n.min(results.len() - 1)].clone()
        }
    }

    fn bm(title: &str, url: &str) -> Bookmark {
        Bookmark { title: title.into(), url: url.into() }
    }

    fn books() -> Vec<Bookmark> {
        vec![bm("The Rust Book", "https://doc.rust-lang.org/book/"), bm("Docs", "https://www.example.com/docs")]
    }

    fn make(source: Arc<Fake>) -> (Arc<BookmarksProvider>, Arc<RecordingLauncher>, Arc<FakeClipboard>) {
        let launcher = Arc::new(RecordingLauncher::default());
        let clip = Arc::new(FakeClipboard::default());
        (BookmarksProvider::new(source, launcher.clone(), clip.clone()), launcher, clip)
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    #[test]
    fn bookmarks_match_title_and_host() {
        let (p, _, _) = make(Fake::new(vec![Ok(books())]));
        assert!(!p.ready());
        p.refresh();
        assert!(p.ready());
        let rows = p.query(&q("rust"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "The Rust Book");
        assert_eq!(rows[0].subtitle, "doc.rust-lang.org");
        assert_eq!(rows[0].id, "https://doc.rust-lang.org/book/");
        assert_eq!(rows[0].icon, "builtin:bookmark");
        let by_host = p.query(&q("example"));
        assert_eq!(by_host.len(), 1);
        assert_eq!(by_host[0].title, "Docs");
        assert!(p.query(&q("zzzz")).is_empty() && p.query(&q("")).is_empty());
    }

    #[test]
    fn enter_opens_the_url_and_copy_copies_it() {
        let (p, launcher, clip) = make(Fake::new(vec![Ok(books())]));
        p.refresh();
        let url = "https://doc.rust-lang.org/book/";
        assert_eq!(p.activate(url, Modifier::None).unwrap(), Outcome::Hide);
        assert_eq!(launcher.calls(), vec![format!("open {url}")]);
        assert_eq!(p.act(url, Action::Copy).unwrap(), Outcome::Hide);
        assert_eq!(*clip.0.lock().unwrap(), vec![url.to_string()]);
        assert!(p.act(url, Action::Reveal).is_err());
    }

    #[test]
    fn a_failed_copy_keeps_the_last_list_and_leaves_no_temp_files() {
        let (p, _, _) = make(Fake::new(vec![Ok(vec![bm("Alpha", "https://a.example/")]), Err("locked".into())]));
        p.refresh();
        p.refresh();
        assert_eq!(p.query(&q("alpha")).len(), 1);
        // Temp-file cleanup on the real path is covered in `bookmarks::tests`.
    }

    fn wait_idle(p: &BookmarksProvider) {
        let end = Instant::now() + Duration::from_secs(2);
        while p.state().running && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!p.state().running);
    }

    #[test]
    fn refresh_if_stale_reads_at_most_every_5_minutes() {
        let base = Instant::now();
        let offset = Arc::new(Mutex::new(Duration::ZERO));
        let clock_offset = offset.clone();
        let clock: Arc<dyn Fn() -> Instant + Send + Sync> = Arc::new(move || base + *clock_offset.lock().unwrap());
        let source = Fake::new(vec![Ok(books())]);
        let p = BookmarksProvider::with_clock(
            source.clone(),
            Arc::new(RecordingLauncher::default()),
            Arc::new(FakeClipboard::default()),
            clock,
        );
        p.refresh_if_stale();
        wait_idle(&p);
        *offset.lock().unwrap() = Duration::from_secs(60);
        p.refresh_if_stale();
        wait_idle(&p);
        assert_eq!(source.reads.load(Ordering::SeqCst), 1);
        assert!(p.ready());
        *offset.lock().unwrap() = Duration::from_secs(301);
        p.refresh_if_stale();
        wait_idle(&p);
        assert_eq!(source.reads.load(Ordering::SeqCst), 2);
    }

    /// Reads block until released; records how many ran at once.
    #[derive(Default)]
    struct Slow {
        open: Mutex<bool>,
        opened: std::sync::Condvar,
        reads: AtomicUsize,
        active: AtomicUsize,
        most: AtomicUsize,
    }

    impl Slow {
        fn release(&self) {
            *self.open.lock().unwrap() = true;
            self.opened.notify_all();
        }
    }

    impl BookmarkSource for Slow {
        fn read(&self) -> Result<Vec<Bookmark>, String> {
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(now, Ordering::SeqCst);
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.opened.wait(open).unwrap();
            }
            drop(open);
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(books())
        }
    }

    #[test]
    fn a_forced_read_never_runs_beside_another_and_follows_it() {
        let source = Arc::new(Slow::default());
        let p = BookmarksProvider::new(source.clone(), Arc::new(RecordingLauncher::default()), Arc::new(FakeClipboard::default()));
        p.refresh_if_stale();
        let end = Instant::now() + Duration::from_secs(2);
        while source.active.load(Ordering::SeqCst) == 0 && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(2));
        }
        // Reload twice while the first read is still copying: no second copy now.
        p.refresh_now();
        p.refresh_now();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(source.active.load(Ordering::SeqCst), 1);
        source.release();
        wait_idle(&p);
        assert_eq!(source.most.load(Ordering::SeqCst), 1, "never two reads at once");
        assert_eq!(source.reads.load(Ordering::SeqCst), 2, "the asked-for read ran once, after");
        // Idle: a forced read starts at once, however fresh the list is.
        p.refresh_now();
        wait_idle(&p);
        assert_eq!(source.reads.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn bookmarks_take_three_rows_in_plain_search() {
        let (p, _, _) = make(Fake::new(vec![Ok(vec![])]));
        assert_eq!(p.plain_share(), 3);
        assert_eq!(p.name(), "bookmarks");
    }

    #[test]
    fn bookmarks_are_learned_and_pinnable() {
        let (p, _, _) = make(Fake::new(vec![Ok(books())]));
        p.refresh();
        let url = "https://doc.rust-lang.org/book/";
        assert_eq!(p.rank_key(url).as_deref(), Some(url));
        let row = p.item(url).unwrap();
        assert!(row.pinnable);
        assert_eq!(row.actions, vec![Action::Copy]);
        assert_eq!(row.score, 0);
        assert_eq!(row.title, "The Rust Book");
        assert!(p.item("https://nope.example/").is_none());
    }
}
