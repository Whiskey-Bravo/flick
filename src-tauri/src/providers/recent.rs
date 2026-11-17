//! `dl` (newest files in Downloads) and `recent` (Windows' own Recent list).
//! Both folders are read on [`Loader`] threads; rows act like file rows.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use super::path::{act_on_entry, entry_row, inert_row, item_from_id, loaded, open_entry, score_in_order, FOLDER_NOT_FOUND};
use crate::clipboard::Clipboard;
use crate::dirs::{ago, folder_key, size_text, Clock, DirEntry, FileSystem, Links, Loader, ReadError};
use crate::fuzzy;
use crate::launch::Launcher;
use crate::provider::{Action, HelpLine, Modifier, Outcome, Provider, Query, ResultItem, Route};

/// Rows each list keeps before filtering.
const NEWEST: usize = 20;
/// `dl`.
pub const DOWNLOADS_ROUTE: Route = Route { prefix: "dl", needs_space: true };
/// `recent`.
pub const RECENT_ROUTE: Route = Route { prefix: "recent", needs_space: true };

/// Keeps the items whose name matches `text` fuzzily, in their order; all when `text` is empty.
fn keep_matching<T>(items: Vec<T>, text: &str, name: impl Fn(&T) -> String) -> Vec<T> {
    let text = text.trim();
    if text.is_empty() {
        return items;
    }
    items.into_iter().filter(|item| fuzzy::score(text, &name(item)).is_some()).collect()
}

/// A folder that is not known shows one inert `Folder not found` row.
fn no_folder(provider: &'static str) -> Vec<ResultItem> {
    vec![inert_row(provider, FOLDER_NOT_FOUND, String::new())]
}

pub struct DownloadsProvider {
    dirs: Arc<Loader<Vec<DirEntry>>>,
    fs: Arc<dyn FileSystem>,
    folder: Option<PathBuf>,
    launcher: Arc<dyn Launcher>,
    clipboard: Arc<dyn Clipboard>,
    now: Clock,
}

impl DownloadsProvider {
    pub fn new(
        dirs: Arc<Loader<Vec<DirEntry>>>,
        fs: Arc<dyn FileSystem>,
        folder: Option<PathBuf>,
        launcher: Arc<dyn Launcher>,
        clipboard: Arc<dyn Clipboard>,
        now: Clock,
    ) -> Self {
        Self { dirs, fs, folder, launcher, clipboard, now }
    }
}

impl Provider for DownloadsProvider {
    fn name(&self) -> &'static str {
        "downloads"
    }

    /// Memory only: the folder is read on a loader thread.
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let Some(folder) = &self.folder else { return no_folder(self.name()) };
        let (fs, dir) = (Arc::clone(&self.fs), folder.clone());
        let load = self.dirs.get(&folder_key(folder), move || fs.read_dir(&dir));
        let entries = match loaded(self.name(), load, &folder.to_string_lossy()) {
            Ok(entries) => entries,
            Err(rows) => return rows,
        };
        let mut files: Vec<&DirEntry> = entries.iter().filter(|e| !e.is_dir).collect();
        files.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.name.cmp(&b.name)));
        files.truncate(NEWEST);
        let now = (self.now)();
        let mut rows: Vec<ResultItem> = keep_matching(files, &q.text, |e| e.name.clone())
            .into_iter()
            .map(|e| {
                let subtitle = match e.modified {
                    Some(at) => format!("{} · {}", ago(now, at), size_text(e.size)),
                    None => size_text(e.size),
                };
                entry_row(self.name(), &folder.join(&e.name), false, subtitle, 0, String::new())
            })
            .collect();
        score_in_order(&mut rows);
        rows
    }

    fn activate(&self, id: &str, modifier: Modifier) -> Result<Outcome, String> {
        open_entry(&*self.fs, &*self.launcher, id, modifier)
    }

    fn act(&self, id: &str, action: Action) -> Result<Outcome, String> {
        act_on_entry(&*self.fs, &*self.launcher, &*self.clipboard, id, action)
    }

    /// Typed lists are never learned.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn route(&self) -> Option<Route> {
        Some(DOWNLOADS_ROUTE)
    }

    fn item(&self, id: &str) -> Option<ResultItem> {
        item_from_id(self.name(), id)
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine { example: "dl".into(), about: "newest files in your Downloads folder".into(), fill: "dl".into() }]
    }
}

/// One entry of Windows' Recent list, resolved to what it points at.
#[derive(Clone, Debug, PartialEq)]
pub struct RecentEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    /// When the link was last written: when the item was last opened.
    pub opened: SystemTime,
}

/// The Recent folder's links, newest first, resolved: links without a
/// target, missing targets and repeats are skipped; at most [`NEWEST`].
fn read_recent(fs: &dyn FileSystem, links: &dyn Links, folder: &Path) -> Result<Vec<RecentEntry>, ReadError> {
    let mut lnks: Vec<DirEntry> =
        fs.read_dir(folder)?.into_iter().filter(|e| !e.is_dir && e.name.to_lowercase().ends_with(".lnk")).collect();
    lnks.sort_by(|a, b| b.modified.cmp(&a.modified));
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for lnk in lnks {
        if out.len() == NEWEST {
            break;
        }
        let Some(target) = links.target(&folder.join(&lnk.name)) else { continue };
        let Some(is_dir) = fs.kind(&target) else { continue };
        if !seen.insert(folder_key(&target)) {
            continue;
        }
        out.push(RecentEntry { path: target, is_dir, opened: lnk.modified.unwrap_or(SystemTime::UNIX_EPOCH) });
    }
    Ok(out)
}

/// The name of the folder holding `path` (the whole folder path for a drive root).
fn parent_name(path: &Path) -> String {
    let Some(parent) = path.parent() else { return String::new() };
    parent.file_name().unwrap_or(parent.as_os_str()).to_string_lossy().into_owned()
}

pub struct RecentProvider {
    store: Arc<Loader<Vec<RecentEntry>>>,
    fs: Arc<dyn FileSystem>,
    links: Arc<dyn Links>,
    folder: Option<PathBuf>,
    launcher: Arc<dyn Launcher>,
    clipboard: Arc<dyn Clipboard>,
    now: Clock,
}

impl RecentProvider {
    pub fn new(
        store: Arc<Loader<Vec<RecentEntry>>>,
        fs: Arc<dyn FileSystem>,
        links: Arc<dyn Links>,
        folder: Option<PathBuf>,
        launcher: Arc<dyn Launcher>,
        clipboard: Arc<dyn Clipboard>,
        now: Clock,
    ) -> Self {
        Self { store, fs, links, folder, launcher, clipboard, now }
    }
}

impl Provider for RecentProvider {
    fn name(&self) -> &'static str {
        "recent"
    }

    /// Memory only: the links are read and resolved on a loader thread.
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let Some(folder) = &self.folder else { return no_folder(self.name()) };
        let (fs, links, dir) = (Arc::clone(&self.fs), Arc::clone(&self.links), folder.clone());
        let load = self.store.get(&folder_key(folder), move || read_recent(&*fs, &*links, &dir));
        let entries = match loaded(self.name(), load, &folder.to_string_lossy()) {
            Ok(entries) => entries,
            Err(rows) => return rows,
        };
        let now = (self.now)();
        let named = |e: &&RecentEntry| e.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let mut rows: Vec<ResultItem> = keep_matching(entries.iter().collect(), &q.text, named)
            .into_iter()
            .map(|e| {
                let subtitle = format!("{} · {}", ago(now, e.opened), parent_name(&e.path));
                entry_row(self.name(), &e.path, e.is_dir, subtitle, 0, String::new())
            })
            .collect();
        score_in_order(&mut rows);
        rows
    }

    fn activate(&self, id: &str, modifier: Modifier) -> Result<Outcome, String> {
        open_entry(&*self.fs, &*self.launcher, id, modifier)
    }

    fn act(&self, id: &str, action: Action) -> Result<Outcome, String> {
        act_on_entry(&*self.fs, &*self.launcher, &*self.clipboard, id, action)
    }

    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn route(&self) -> Option<Route> {
        Some(RECENT_ROUTE)
    }

    fn item(&self, id: &str) -> Option<ResultItem> {
        item_from_id(self.name(), id)
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine { example: "recent".into(), about: "files you opened recently".into(), fill: "recent".into() }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::FakeClipboard;
    use crate::dirs::testing::{file, folder, FakeFolders, FakeFs, FakeLinks};
    use crate::dirs::{KnownFolders, FRESH, SLOW};
    use crate::launch::testing::RecordingLauncher;
    use crate::providers::path::READING;
    use std::sync::mpsc::{self, Receiver};
    use std::sync::Mutex;
    use std::time::{Duration, UNIX_EPOCH};

    const DOWNLOADS: &str = r"C:\Users\someone\Downloads";
    const RECENT: &str = r"C:\Users\someone\AppData\Roaming\Microsoft\Windows\Recent";
    const DOCS: &str = r"C:\Users\someone\Documents";

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn mins_ago(m: u64) -> Option<SystemTime> {
        Some(now() - Duration::from_secs(m * 60))
    }

    fn clock() -> Clock {
        Arc::new(now)
    }

    fn loader<T: Send + Sync + 'static>() -> (Arc<Loader<T>>, Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let l = Loader::new(
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
            FRESH,
            SLOW,
        );
        (l, rx)
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    /// The rows once read: waits out a first `Reading…`.
    fn ready(p: &dyn Provider, rx: &Receiver<()>, text: &str) -> Vec<ResultItem> {
        let rows = p.query(&q(text));
        if rows.first().is_some_and(|r| r.title == READING) {
            rx.recv_timeout(Duration::from_secs(1)).expect("on_ready within 1 s");
            return p.query(&q(text));
        }
        rows
    }

    fn titles(rows: &[ResultItem]) -> Vec<&str> {
        rows.iter().map(|r| r.title.as_str()).collect()
    }

    struct Dl {
        p: DownloadsProvider,
        rx: Receiver<()>,
        launcher: Arc<RecordingLauncher>,
        clip: Arc<FakeClipboard>,
    }

    fn downloads(entries: Vec<DirEntry>, folders: &dyn KnownFolders) -> Dl {
        let fs = Arc::new(FakeFs::default());
        fs.add_dir(DOWNLOADS, entries);
        let (dirs, rx) = loader();
        let launcher = Arc::new(RecordingLauncher::default());
        let clip = Arc::new(FakeClipboard::default());
        let p = DownloadsProvider::new(dirs, fs, folders.downloads(), launcher.clone(), clip.clone(), clock());
        Dl { p, rx, launcher, clip }
    }

    fn known() -> FakeFolders {
        FakeFolders { downloads: Some(PathBuf::from(DOWNLOADS)), recent: Some(PathBuf::from(RECENT)) }
    }

    /// 25 files `file-00.txt` (oldest) .. `file-24.txt` (newest), plus two newer folders.
    fn twenty_five() -> Vec<DirEntry> {
        let mut entries: Vec<DirEntry> =
            (0..25).map(|i| DirEntry { modified: mins_ago(100 - i), ..file(&format!("file-{i:02}.txt"), 10) }).collect();
        entries.push(DirEntry { modified: mins_ago(1), ..folder("New folder") });
        entries.push(DirEntry { modified: mins_ago(2), ..folder("Unzipped") });
        entries
    }

    #[test]
    fn dl_lists_the_20_newest_files_newest_first() {
        let d = downloads(twenty_five(), &known());
        let rows = ready(&d.p, &d.rx, "");
        let want: Vec<String> = (5..25).rev().map(|i| format!("file-{i:02}.txt")).collect();
        assert_eq!(titles(&rows), want);
        assert!(rows.windows(2).all(|w| w[0].score > w[1].score));
        assert_eq!(rows[0].id, format!(r"{DOWNLOADS}\file-24.txt"));
    }

    #[test]
    fn dl_subtitle_is_age_and_size() {
        let d = downloads(vec![DirEntry { modified: mins_ago(120), ..file("big.iso", 14 * 1024 * 1024) }], &known());
        let rows = ready(&d.p, &d.rx, "");
        assert_eq!(rows[0].subtitle, "2 h ago · 14 MB");
    }

    #[test]
    fn dl_text_filters_the_newest() {
        let mut entries = twenty_five();
        entries.push(DirEntry { modified: mins_ago(500), ..file("report old.pdf", 1) });
        entries.push(DirEntry { modified: mins_ago(30), ..file("report.pdf", 1) });
        entries.push(DirEntry { modified: mins_ago(3), ..file("report final.pdf", 1) });
        let d = downloads(entries, &known());
        let rows = ready(&d.p, &d.rx, "report");
        // Newest first, not best match first; the old one is past the newest 20.
        assert_eq!(titles(&rows), ["report final.pdf", "report.pdf"]);
        assert!(rows.windows(2).all(|w| w[0].score > w[1].score));
        assert!(ready(&d.p, &d.rx, "zzz").is_empty());
    }

    #[test]
    fn dl_without_a_downloads_folder_says_folder_not_found() {
        let d = downloads(twenty_five(), &FakeFolders::default());
        let rows = d.p.query(&q(""));
        assert_eq!(titles(&rows), [FOLDER_NOT_FOUND]);
        assert!(rows[0].actions.is_empty());
        assert_eq!(d.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Keep));
        assert!(d.rx.recv_timeout(Duration::from_millis(50)).is_err(), "nothing read");
    }

    struct Rc {
        p: RecentProvider,
        rx: Receiver<()>,
    }

    fn lnk(name: &str) -> PathBuf {
        Path::new(RECENT).join(name)
    }

    fn recent_rig(entries: Vec<DirEntry>, setup: impl FnOnce(&FakeFs, &FakeLinks)) -> Rc {
        let fs = Arc::new(FakeFs::default());
        fs.add_dir(RECENT, entries);
        let links = Arc::new(FakeLinks::default());
        setup(&fs, &links);
        let (store, rx) = loader();
        let p = RecentProvider::new(
            store,
            fs,
            links,
            known().recent(),
            Arc::new(RecordingLauncher::default()),
            Arc::new(FakeClipboard::default()),
            clock(),
        );
        Rc { p, rx }
    }

    #[test]
    fn recent_resolves_links_skips_missing_and_duplicates() {
        let entries = vec![
            DirEntry { modified: mins_ago(1), ..file("a.txt.lnk", 1) },
            DirEntry { modified: mins_ago(2), ..file("gone.txt.lnk", 1) },
            DirEntry { modified: mins_ago(3), ..file("broken.lnk", 1) },
            DirEntry { modified: mins_ago(4), ..file("A again.lnk", 1) },
            DirEntry { modified: mins_ago(5), ..file("Documents.LNK", 1) },
            DirEntry { modified: mins_ago(0), ..file("desktop.ini", 1) },
            DirEntry { modified: mins_ago(0), ..folder("AutomaticDestinations") },
        ];
        let r = recent_rig(entries, |fs, links| {
            fs.set_kind(&format!(r"{DOCS}\a.txt"), Some(false));
            fs.set_kind(DOCS, Some(true));
            links.add(&lnk("a.txt.lnk"), &format!(r"{DOCS}\a.txt"));
            links.add(&lnk("gone.txt.lnk"), &format!(r"{DOCS}\gone.txt"));
            links.add(&lnk("A again.lnk"), r"c:\users\someone\documents\A.TXT");
            links.add(&lnk("Documents.LNK"), DOCS);
        });
        let rows = ready(&r.p, &r.rx, "");
        assert_eq!(titles(&rows), ["a.txt", "Documents"]);
        assert_eq!(rows[0].id, format!(r"{DOCS}\a.txt"));
        assert_eq!(rows[0].actions, [Action::Reveal, Action::CopyPath]);
        assert_eq!(rows[1].id, format!(r"{DOCS}\"));
        assert_eq!(rows[1].actions, [Action::OpenFolder, Action::CopyPath]);
        assert_eq!(titles(&ready(&r.p, &r.rx, "doc")), ["Documents"]);
    }

    #[test]
    fn recent_keeps_the_20_newest() {
        let entries: Vec<DirEntry> = (0..30).map(|i| DirEntry { modified: mins_ago(i), ..file(&format!("f{i}.lnk"), 1) }).collect();
        let r = recent_rig(entries, |fs, links| {
            for i in 0..30 {
                let target = format!(r"{DOCS}\f{i}.txt");
                fs.set_kind(&target, Some(false));
                links.add(&lnk(&format!("f{i}.lnk")), &target);
            }
        });
        let rows = ready(&r.p, &r.rx, "");
        assert_eq!(rows.len(), 20);
        assert_eq!(rows[0].title, "f0.txt");
        assert_eq!(rows[19].title, "f19.txt");
    }

    #[test]
    fn recent_subtitle_is_age_and_folder() {
        let r = recent_rig(vec![DirEntry { modified: mins_ago(30 * 60), ..file("a.lnk", 1) }], |fs, links| {
            fs.set_kind(&format!(r"{DOCS}\a.txt"), Some(false));
            links.add(&lnk("a.lnk"), &format!(r"{DOCS}\a.txt"));
        });
        let rows = ready(&r.p, &r.rx, "");
        assert_eq!(rows[0].subtitle, "Yesterday · Documents");
    }

    #[test]
    fn recent_without_a_folder_says_folder_not_found() {
        let p = RecentProvider::new(
            loader().0,
            Arc::new(FakeFs::default()),
            Arc::new(FakeLinks::default()),
            None,
            Arc::new(RecordingLauncher::default()),
            Arc::new(FakeClipboard::default()),
            clock(),
        );
        assert_eq!(titles(&p.query(&q(""))), [FOLDER_NOT_FOUND]);
    }

    #[test]
    fn routes_are_words() {
        let d = downloads(vec![DirEntry { modified: mins_ago(5), ..file("a.zip", 1) }], &known());
        assert_eq!(d.p.route(), Some(Route { prefix: "dl", needs_space: true }));
        assert_eq!(d.p.name(), "downloads");
        let help = d.p.help();
        assert_eq!((help[0].example.as_str(), help[0].about.as_str(), help[0].fill.as_str()), ("dl", "newest files in your Downloads folder", "dl"));
        assert!(d.p.empty_query().is_empty());
        let rows = ready(&d.p, &d.rx, "");
        let id = rows[0].id.clone();
        assert_eq!(d.p.rank_key(&id), None);
        assert_eq!(rows[0].actions, [Action::Reveal, Action::CopyPath]);
        assert!(rows[0].pinnable);
        assert_eq!(rows[0].provider, "downloads");
        assert_eq!(d.p.item(&id).unwrap().title, "a.zip");
        assert_eq!(d.p.act(&id, Action::CopyPath), Ok(Outcome::Hide));
        assert_eq!(*d.clip.0.lock().unwrap(), [id.clone()]);
        assert_eq!(d.p.activate(&id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(d.p.activate(&id, Modifier::Ctrl), Ok(Outcome::Hide));
        assert_eq!(d.launcher.calls(), [format!("open {id}"), format!("reveal {id}")]);

        let r = recent_rig(vec![], |_, _| {});
        assert_eq!(r.p.route(), Some(Route { prefix: "recent", needs_space: true }));
        assert_eq!(r.p.name(), "recent");
        assert_eq!(r.p.rank_key(r"C:\a.txt"), None);
        let help = r.p.help();
        assert_eq!((help[0].example.as_str(), help[0].about.as_str(), help[0].fill.as_str()), ("recent", "files you opened recently", "recent"));
        assert!(r.p.empty_query().is_empty());
        assert_eq!(r.p.act(r"C:\a.txt", Action::CopyPath), Err("file not found".to_string()));
    }
}
