//! Path browsing: text shaped like a path (`C:\`, `~\Documents\`,
//! `\\server\share\`) lists that folder, filtered by the part after the last
//! separator. Folders are read by the shared [`Loader`] on a background
//! thread; the query path only reads what it holds.
//!
//! The row and action helpers here are shared with `dl` and `recent`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::files::file_icon;
use crate::clipboard::Clipboard;
use crate::dirs::{folder_key, size_text, DirEntry, FileSystem, Load, Loader};
use crate::fuzzy;
use crate::launch::Launcher;
use crate::provider::{Action, HelpLine, Modifier, Outcome, Provider, Query, ResultItem};

const NAME: &str = "path";
pub(crate) const READING: &str = "Reading…";
pub(crate) const FOLDER_NOT_FOUND: &str = "Folder not found";
pub(crate) const CANT_OPEN: &str = "Can't open this folder";
const NOT_FOUND: &str = "file not found";
const CANT_DO: &str = "can't do that here";
/// Ids of rows that do nothing start with this.
const INERT: &str = "inert:";
const MAX_ROWS: usize = 50;
/// Row scores count down from here, so the engine's sort keeps the order.
pub(crate) const TOP_SCORE: u32 = 3000;

/// A typed path split into the folder to list and the text filtering it.
#[derive(Clone, Debug, PartialEq)]
pub struct PathQuery {
    /// Absolute, with a trailing `\`.
    pub folder: PathBuf,
    /// The text up to and including the last separator, as typed.
    pub typed: String,
    pub filter: String,
    /// The last separator typed: `\` or `/`.
    pub sep: char,
}

fn is_sep(c: char) -> bool {
    c == '\\' || c == '/'
}

/// Starts with a drive and separator (`C:\`, `d:/`), `~` and a separator, or
/// `\\` / `//`.
pub fn looks_like_path(text: &str) -> bool {
    let b = text.as_bytes();
    let sep_at = |i: usize| b.get(i).is_some_and(|&c| c == b'\\' || c == b'/');
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && sep_at(2))
        || (b.first() == Some(&b'~') && sep_at(1))
        || text.starts_with(r"\\")
        || text.starts_with("//")
}

/// `None` when `text` is not a path, a UNC path lacks a server and a share
/// followed by a separator, or `~` is used with no home folder.
pub fn parse_path(text: &str, home: Option<&Path>) -> Option<PathQuery> {
    if !looks_like_path(text) {
        return None;
    }
    let cut = text.rfind(is_sep)?;
    let sep = if text[cut..].starts_with('/') { '/' } else { '\\' };
    let typed = &text[..=cut];
    let folder = typed.replace('/', "\\");
    let folder = if let Some(rest) = folder.strip_prefix('~') {
        format!("{}{rest}", home?.to_string_lossy().trim_end_matches(is_sep))
    } else if let Some(unc) = folder.strip_prefix(r"\\") {
        // `server\share\...`: both named, and the share followed by a separator.
        let mut parts = unc.split('\\');
        let (server, share) = (parts.next()?, parts.next()?);
        if server.is_empty() || share.is_empty() || parts.next().is_none() {
            return None;
        }
        folder
    } else {
        folder
    };
    Some(PathQuery { folder: PathBuf::from(folder), typed: typed.to_string(), filter: text[cut + 1..].to_string(), sep })
}

/// A file or folder row: the id is the path (folders end with `\`), the
/// title its name (a drive or share root shows the whole path).
pub(crate) fn entry_row(provider: &'static str, path: &Path, is_dir: bool, subtitle: String, score: u32, complete: String) -> ResultItem {
    let text = path.to_string_lossy().into_owned();
    let title = path.file_name().map_or_else(|| text.clone(), |n| n.to_string_lossy().into_owned());
    let mut id = text;
    if is_dir && !id.ends_with('\\') {
        id.push('\\');
    }
    ResultItem {
        id,
        provider,
        icon: file_icon(&title, is_dir),
        title,
        subtitle,
        score,
        pinnable: true,
        actions: if is_dir { vec![Action::OpenFolder, Action::CopyPath] } else { vec![Action::Reveal, Action::CopyPath] },
        complete,
        ..Default::default()
    }
}

/// A row that only says something: no actions, Enter and Tab do nothing.
pub(crate) fn inert_row(provider: &'static str, title: &str, subtitle: String) -> ResultItem {
    ResultItem {
        id: format!("{INERT}{title}"),
        provider,
        title: title.to_string(),
        subtitle,
        score: TOP_SCORE,
        icon: "folder".into(),
        ..Default::default()
    }
}

/// The path in an entry id and whether it names a folder: a trailing `\`,
/// dropped unless the path is a drive root.
fn split_id(id: &str) -> (&str, bool) {
    match id.strip_suffix('\\') {
        Some(path) if !path.is_empty() && !path.ends_with(':') => (path, true),
        Some(_) => (id, true),
        None => (id, false),
    }
}

/// The id's path, if it is still on disk as the same kind.
fn checked<'a>(fs: &dyn FileSystem, id: &'a str) -> Result<&'a str, String> {
    let (path, is_dir) = split_id(id);
    if fs.kind(Path::new(path)) != Some(is_dir) {
        return Err(NOT_FOUND.into());
    }
    Ok(path)
}

/// Enter / Ctrl+Shift+Enter open, Ctrl+Enter shows it in Explorer. Inert rows keep the bar.
pub(crate) fn open_entry(fs: &dyn FileSystem, launcher: &dyn Launcher, id: &str, m: Modifier) -> Result<Outcome, String> {
    if id.starts_with(INERT) {
        return Ok(Outcome::Keep);
    }
    let path = checked(fs, id)?;
    match m {
        Modifier::None | Modifier::CtrlShift => launcher.open(id)?,
        Modifier::Ctrl => launcher.reveal(path)?,
    }
    Ok(Outcome::Hide)
}

/// The actions menu of a file or folder row.
pub(crate) fn act_on_entry(fs: &dyn FileSystem, launcher: &dyn Launcher, clipboard: &dyn Clipboard, id: &str, action: Action) -> Result<Outcome, String> {
    if id.starts_with(INERT) || !matches!(action, Action::Reveal | Action::OpenFolder | Action::CopyPath) {
        return Err(CANT_DO.into());
    }
    let path = checked(fs, id)?;
    match action {
        Action::Reveal => launcher.reveal(path)?,
        Action::OpenFolder => launcher.open(id)?,
        _ => clipboard.set_text(path)?,
    }
    Ok(Outcome::Hide)
}

/// The row for `id` without touching the disk: a folder iff it ends with `\`.
pub(crate) fn item_from_id(provider: &'static str, id: &str) -> Option<ResultItem> {
    if !looks_like_path(id) {
        return None;
    }
    let (path, is_dir) = split_id(id);
    let path = Path::new(path);
    let subtitle = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    Some(entry_row(provider, path, is_dir, subtitle, 0, String::new()))
}

/// The value of a ready load, or the one inert row saying why there is none.
pub(crate) fn loaded<T>(provider: &'static str, load: Load<T>, folder: &str) -> Result<Arc<T>, Vec<ResultItem>> {
    let title = match load {
        Load::Ready(value) => return Ok(value),
        Load::Pending => READING,
        Load::NotFound => FOLDER_NOT_FOUND,
        Load::Denied | Load::Slow => CANT_OPEN,
    };
    Err(vec![inert_row(provider, title, folder.to_string())])
}

/// Gives each row a score below the one before, from [`TOP_SCORE`].
pub(crate) fn score_in_order(rows: &mut [ResultItem]) {
    for (i, row) in rows.iter_mut().enumerate() {
        row.score = TOP_SCORE.saturating_sub(i as u32);
    }
}

pub struct PathProvider {
    dirs: Arc<Loader<Vec<DirEntry>>>,
    fs: Arc<dyn FileSystem>,
    home: Option<PathBuf>,
    launcher: Arc<dyn Launcher>,
    clipboard: Arc<dyn Clipboard>,
}

impl PathProvider {
    pub fn new(
        dirs: Arc<Loader<Vec<DirEntry>>>,
        fs: Arc<dyn FileSystem>,
        home: Option<PathBuf>,
        launcher: Arc<dyn Launcher>,
        clipboard: Arc<dyn Clipboard>,
    ) -> Self {
        Self { dirs, fs, home, launcher, clipboard }
    }
}

impl Provider for PathProvider {
    fn name(&self) -> &'static str {
        NAME
    }

    /// Memory only: a folder the loader has not read yet starts its read and
    /// shows `Reading…`.
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let Some(pq) = parse_path(&q.text, self.home.as_deref()) else { return Vec::new() };
        let shown = pq.folder.to_string_lossy().into_owned();
        let (fs, folder) = (Arc::clone(&self.fs), pq.folder.clone());
        let load = self.dirs.get(&folder_key(&pq.folder), move || fs.read_dir(&folder));
        let entries = match loaded(NAME, load, &shown) {
            Ok(entries) => entries,
            Err(rows) => return rows,
        };
        // (entry, score, sort name): each name lowercased once, not in every comparison.
        let mut picked: Vec<(&DirEntry, u32, String)> = if pq.filter.is_empty() {
            entries.iter().map(|e| (e, 0, e.name.to_lowercase())).collect()
        } else {
            let needle = fuzzy::normalize(&pq.filter);
            entries
                .iter()
                .filter_map(|e| {
                    let norm = fuzzy::normalize(&e.name);
                    fuzzy::score_prenormalized(&needle, &norm).map(|s| (e, s, norm))
                })
                .collect()
        };
        picked.sort_unstable_by(|a, b| {
            b.0.is_dir
                .cmp(&a.0.is_dir)
                .then(b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(&b.2))
                .then_with(|| a.0.name.cmp(&b.0.name))
        });

        let mut rows = Vec::new();
        if pq.filter.is_empty() {
            let mut own = entry_row(NAME, &pq.folder, true, "This folder".into(), 0, String::new());
            own.title = shown;
            rows.push(own);
        }
        let room = MAX_ROWS - rows.len();
        for (e, _, _) in picked.into_iter().take(room) {
            let complete = if e.is_dir { format!("{}{}{}", pq.typed, e.name, pq.sep) } else { format!("{}{}", pq.typed, e.name) };
            let subtitle = if e.is_dir { "Folder".to_string() } else { size_text(e.size) };
            rows.push(entry_row(NAME, &pq.folder.join(&e.name), e.is_dir, subtitle, 0, complete));
        }
        score_in_order(&mut rows);
        rows
    }

    fn activate(&self, id: &str, modifier: Modifier) -> Result<Outcome, String> {
        open_entry(&*self.fs, &*self.launcher, id, modifier)
    }

    fn act(&self, id: &str, action: Action) -> Result<Outcome, String> {
        act_on_entry(&*self.fs, &*self.launcher, &*self.clipboard, id, action)
    }

    /// Typed paths are never learned.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn item(&self, id: &str) -> Option<ResultItem> {
        item_from_id(NAME, id)
    }

    fn claims(&self, text: &str) -> bool {
        looks_like_path(text)
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: r"C:\ / ~\Documents\".into(),
            about: "browse a folder by typing its path; Tab completes a name".into(),
            fill: r"~\".into(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::FakeClipboard;
    use crate::dirs::testing::{file, folder, FakeFs};
    use crate::dirs::{ReadError, FRESH, SLOW};
    use crate::launch::testing::RecordingLauncher;
    use std::sync::mpsc::{self, Receiver};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    const HOME: &str = r"C:\Users\someone";

    struct Rig {
        p: PathProvider,
        fs: Arc<FakeFs>,
        launcher: Arc<RecordingLauncher>,
        clip: Arc<FakeClipboard>,
        rx: Receiver<()>,
    }

    impl Rig {
        fn q(&self, text: &str) -> Vec<ResultItem> {
            self.p.query(&Query { text: text.into(), scope: None })
        }

        /// The rows once the folder is read: waits out a first `Reading…`.
        fn ready(&self, text: &str) -> Vec<ResultItem> {
            let rows = self.q(text);
            if rows.first().is_some_and(|r| r.title == READING) {
                self.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready within 1 s");
                return self.q(text);
            }
            rows
        }
    }

    fn rig_with(fs: FakeFs, slow: Duration) -> Rig {
        let fs = Arc::new(fs);
        let launcher = Arc::new(RecordingLauncher::default());
        let clip = Arc::new(FakeClipboard::default());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let dirs = Loader::new(
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
            FRESH,
            slow,
        );
        let p = PathProvider::new(dirs, fs.clone(), Some(PathBuf::from(HOME)), launcher.clone(), clip.clone());
        Rig { p, fs, launcher, clip, rx }
    }

    /// `C:\` holds two folders and two files; the home folder `Documents` and `notes.txt`.
    fn rig() -> Rig {
        let fs = FakeFs::default();
        fs.add_dir(r"C:\", vec![file("b.txt", 2048), folder("Users"), file("a.txt", 10), folder("Games")]);
        fs.add_dir(HOME, vec![folder("Documents"), file("notes.txt", 1)]);
        rig_with(fs, SLOW)
    }

    fn titles(rows: &[ResultItem]) -> Vec<&str> {
        rows.iter().map(|r| r.title.as_str()).collect()
    }

    fn assert_scores_fall(rows: &[ResultItem]) {
        assert!(rows.windows(2).all(|w| w[0].score > w[1].score), "scores strictly decreasing");
    }

    #[test]
    fn parse_path_recognises_drives_home_and_shares() {
        let home = Some(Path::new(HOME));
        let p = parse_path(r"C:\", home).unwrap();
        assert_eq!((p.folder, p.typed.as_str(), p.filter.as_str(), p.sep), (PathBuf::from(r"C:\"), r"C:\", "", '\\'));
        let p = parse_path("c:/Win", home).unwrap();
        assert_eq!((p.folder, p.typed.as_str(), p.filter.as_str(), p.sep), (PathBuf::from(r"c:\"), "c:/", "Win", '/'));
        let p = parse_path(r"~\Doc", home).unwrap();
        assert_eq!((p.folder, p.typed.as_str(), p.filter.as_str()), (PathBuf::from(r"C:\Users\someone\"), r"~\", "Doc"));
        let p = parse_path("~/Documents/x", home).unwrap();
        assert_eq!((p.folder, p.typed.as_str(), p.filter.as_str()), (PathBuf::from(r"C:\Users\someone\Documents\"), "~/Documents/", "x"));
        let p = parse_path(r"\\server\share\x", home).unwrap();
        assert_eq!((p.folder, p.filter.as_str()), (PathBuf::from(r"\\server\share\"), "x"));
        let p = parse_path(r"D:\Games\Steam\", home).unwrap();
        assert_eq!((p.folder, p.filter.as_str()), (PathBuf::from(r"D:\Games\Steam\"), ""));
        assert_eq!(parse_path(r"\\server", home), None);
        assert_eq!(parse_path(r"\\server\", home), None);
        assert_eq!(parse_path(r"\\server\share", home), None);
        assert_eq!(parse_path(r"~\", None), None);
        assert_eq!(parse_path("notes", home), None);
    }

    #[test]
    fn claims_only_path_shapes() {
        let r = rig();
        for yes in [r"C:\", "d:/", r"~\", "~/", r"\\srv", "//srv", r"C:\Games\x"] {
            assert!(r.p.claims(yes), "{yes}");
        }
        for no in ["C:", "C:foo", "~", "dl", "dlls", "notes", "", "1:/", r"\x"] {
            assert!(!r.p.claims(no), "{no}");
        }
    }

    #[test]
    fn query_never_touches_the_disk() {
        let r = rig();
        r.fs.block();
        let t = Instant::now();
        let rows = r.q(r"C:\");
        assert!(t.elapsed() < Duration::from_millis(50));
        assert_eq!(titles(&rows), [READING]);
        assert_eq!(r.fs.reads(), 0);
        assert_eq!(titles(&r.q(r"C:\")), [READING]);
        r.fs.release();
        r.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready");
        assert_eq!(r.fs.reads(), 1);
    }

    #[test]
    fn entries_list_folders_first_with_the_folder_itself_on_top() {
        let r = rig();
        let rows = r.ready(r"C:\");
        assert_eq!(titles(&rows), [r"C:\", "Games", "Users", "a.txt", "b.txt"]);
        let subs: Vec<&str> = rows.iter().map(|r| r.subtitle.as_str()).collect();
        assert_eq!(subs, ["This folder", "Folder", "Folder", "10 B", "2.0 KB"]);
        assert_eq!(rows[0].id, r"C:\");
        assert_eq!(rows[0].complete, "");
        assert_eq!(rows[0].actions, [Action::OpenFolder, Action::CopyPath]);
        assert_eq!(rows[1].id, r"C:\Games\");
        assert_eq!(rows[1].icon, "folder");
        assert_eq!(rows[3].id, r"C:\a.txt");
        assert_eq!(rows[3].icon, "ext:.txt");
        assert_eq!(rows[3].actions, [Action::Reveal, Action::CopyPath]);
        assert!(rows.iter().all(|r| r.provider == "path" && r.pinnable));
        assert_scores_fall(&rows);
    }

    #[test]
    fn filter_is_fuzzy_and_folders_stay_first() {
        let fs = FakeFs::default();
        fs.add_dir(r"C:\", vec![file("game.txt", 1), folder("Users"), folder("Games"), file("my game notes.md", 1), folder("Old games")]);
        let r = rig_with(fs, SLOW);
        let rows = r.ready(r"C:\gam");
        assert_eq!(titles(&rows), ["Games", "Old games", "game.txt", "my game notes.md"]);
        assert_scores_fall(&rows);
        assert!(r.ready(r"C:\zzz").is_empty());
    }

    #[test]
    fn reading_then_entries_selects_from_new_rows() {
        let r = rig();
        assert_eq!(titles(&r.q(r"C:\Gam")), [READING]);
        r.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready");
        let rows = r.q(r"C:\Gam");
        assert_eq!(rows[0].title, "Games");
        assert_eq!(rows[0].score, TOP_SCORE);
    }

    #[test]
    fn missing_denied_and_slow_folders_say_so() {
        let r = rig();
        r.fs.fail_dir(r"D:\", ReadError::Denied);
        let rows = r.ready(r"E:\Nope\");
        assert_eq!(titles(&rows), [FOLDER_NOT_FOUND]);
        assert_eq!(rows[0].subtitle, r"E:\Nope\");
        let rows = r.ready(r"D:\");
        assert_eq!(titles(&rows), [CANT_OPEN]);
        assert_eq!(rows[0].subtitle, r"D:\");

        let r = rig_with(FakeFs::default(), Duration::from_millis(50));
        r.fs.block();
        assert_eq!(titles(&r.q(r"F:\")), [READING]);
        std::thread::sleep(Duration::from_millis(80));
        let rows = r.q(r"F:\");
        assert_eq!(titles(&rows), [CANT_OPEN]);
        assert_eq!(rows[0].subtitle, r"F:\");
        r.fs.release();
    }

    #[test]
    fn inert_rows_do_nothing() {
        let r = rig();
        let row = r.ready(r"E:\").remove(0);
        assert_eq!(row.title, FOLDER_NOT_FOUND);
        assert!(row.actions.is_empty());
        assert_eq!(row.complete, "");
        assert!(!row.pinnable);
        assert_eq!(r.p.activate(&row.id, Modifier::None), Ok(Outcome::Keep));
        assert_eq!(r.p.activate(&row.id, Modifier::Ctrl), Ok(Outcome::Keep));
        assert!(r.p.act(&row.id, Action::CopyPath).is_err());
        assert!(r.p.item(&row.id).is_none());
        assert!(r.launcher.calls().is_empty());
        assert!(r.clip.0.lock().unwrap().is_empty());
    }

    #[test]
    fn tab_completion_keeps_what_was_typed() {
        let r = rig();
        let rows = r.ready("~/Doc");
        assert_eq!(rows[0].title, "Documents");
        assert_eq!(rows[0].complete, "~/Documents/");
        let rows = r.ready("~/no");
        assert_eq!(rows[0].title, "notes.txt");
        assert_eq!(rows[0].complete, "~/notes.txt");
        let rows = r.ready(r"C:\Gam");
        assert_eq!(rows[0].complete, r"C:\Games\");
    }

    #[test]
    fn enter_opens_ctrl_enter_reveals_copy_path_copies() {
        let r = rig();
        let rows = r.ready(r"C:\");
        let games = rows.iter().find(|r| r.title == "Games").unwrap();
        let a = rows.iter().find(|r| r.title == "a.txt").unwrap();
        assert_eq!(r.p.activate(&games.id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(r.p.activate(&a.id, Modifier::Ctrl), Ok(Outcome::Hide));
        assert_eq!(r.p.act(&games.id, Action::CopyPath), Ok(Outcome::Hide));
        assert_eq!(r.launcher.calls(), [r"open C:\Games\", r"reveal C:\a.txt"]);
        assert_eq!(*r.clip.0.lock().unwrap(), [r"C:\Games"]);
        assert_eq!(r.p.act(&a.id, Action::Reveal), Ok(Outcome::Hide));
        assert_eq!(r.p.act(&games.id, Action::OpenFolder), Ok(Outcome::Hide));
        assert_eq!(r.p.activate(&a.id, Modifier::CtrlShift), Ok(Outcome::Hide));
        assert_eq!(r.launcher.calls()[2..], [r"reveal C:\a.txt", r"open C:\Games\", r"open C:\a.txt"]);
        assert!(r.p.act(&a.id, Action::Uninstall).is_err());
    }

    #[test]
    fn a_missing_path_is_file_not_found() {
        let r = rig();
        assert_eq!(r.p.activate(r"C:\gone.txt", Modifier::None), Err(NOT_FOUND.to_string()));
        // The kind must match too: a file is not a folder.
        assert_eq!(r.p.activate(r"C:\a.txt\", Modifier::None), Err(NOT_FOUND.to_string()));
        assert_eq!(r.p.act(r"C:\Games", Action::CopyPath), Err(NOT_FOUND.to_string()));
        r.fs.set_kind(r"C:\a.txt", None);
        assert_eq!(r.p.act(r"C:\a.txt", Action::Reveal), Err(NOT_FOUND.to_string()));
        assert!(r.launcher.calls().is_empty());
        assert!(r.clip.0.lock().unwrap().is_empty());
    }

    #[test]
    fn path_rows_are_never_learned_and_rebuild_without_disk() {
        let r = rig();
        assert_eq!(r.p.rank_key(r"C:\Games\"), None);
        assert_eq!(r.p.rank_key(r"C:\a.txt"), None);
        let before = r.fs.reads();
        let row = r.p.item(r"C:\Games\").unwrap();
        assert_eq!((row.id.as_str(), row.title.as_str(), row.provider), (r"C:\Games\", "Games", "path"));
        assert_eq!(row.actions, [Action::OpenFolder, Action::CopyPath]);
        assert!(row.pinnable);
        let row = r.p.item(r"C:\a.txt").unwrap();
        assert_eq!(row.actions, [Action::Reveal, Action::CopyPath]);
        assert_eq!(r.p.item(r"C:\").unwrap().title, r"C:\");
        assert!(r.p.item("notes").is_none());
        assert_eq!(r.fs.reads(), before);
    }

    #[test]
    fn unc_without_a_share_lists_and_reads_nothing() {
        let r = rig();
        for text in [r"\\server", r"\\server\", r"\\server\share", "//server/"] {
            assert!(r.p.claims(text), "{text}");
            assert!(r.q(text).is_empty(), "{text}");
        }
        assert!(r.rx.recv_timeout(Duration::from_millis(100)).is_err(), "no read started");
        assert_eq!(r.fs.reads(), 0);
    }

    #[test]
    fn a_huge_folder_builds_at_most_50_rows() {
        let fs = FakeFs::default();
        let entries: Vec<DirEntry> = (0..20_000)
            .map(|i| if i % 10 == 0 { folder(&format!("album {i}")) } else { file(&format!("track {i} final.mp3"), i) })
            .collect();
        fs.add_dir(r"C:\big", entries);
        let r = rig_with(fs, SLOW);
        assert!(r.ready(r"C:\big\").len() <= MAX_ROWS);
        let t = Instant::now();
        let rows = r.q(r"C:\big\a");
        let took = t.elapsed();
        assert!(!rows.is_empty() && rows.len() <= MAX_ROWS);
        assert!(rows[0].title.starts_with("album"));
        println!("20 000 entries filtered in {took:?}");
        // Debug builds are several times slower; the bound holds in release.
        if !cfg!(debug_assertions) {
            assert!(took < Duration::from_millis(50), "took {took:?}");
        }
    }

    #[test]
    fn case_and_separator_variants_share_one_read() {
        let fs = FakeFs::default();
        fs.add_dir(r"C:\Games", vec![file("x.txt", 1), folder("Xcom")]);
        let r = rig_with(fs, SLOW);
        assert_eq!(r.ready(r"C:\Games\").len(), 3);
        let rows = r.q("c:/games/x");
        assert_eq!(titles(&rows), ["Xcom", "x.txt"]);
        assert!(rows.iter().all(|row| row.complete.starts_with("c:/games/")), "{rows:?}");
        assert_eq!(rows[0].complete, "c:/games/Xcom/");
        assert_eq!(r.fs.reads(), 1);
    }

    #[test]
    fn odd_names_roundtrip() {
        let fs = FakeFs::default();
        fs.add_dir(r"C:\", vec![file("50% #1 ü.txt", 3)]);
        let r = rig_with(fs, SLOW);
        let rows = r.ready(r"C:\50");
        assert_eq!(rows[0].id, r"C:\50% #1 ü.txt");
        assert_eq!(rows[0].complete, r"C:\50% #1 ü.txt");
        assert_eq!(r.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(r.launcher.calls(), [r"open C:\50% #1 ü.txt"]);
    }

    #[test]
    fn help_line_and_no_empty_bar_rows() {
        let r = rig();
        let help = r.p.help();
        assert_eq!(help[0].example, r"C:\ / ~\Documents\");
        assert_eq!(help[0].about, "browse a folder by typing its path; Tab completes a name");
        assert_eq!(help[0].fill, r"~\");
        assert!(r.p.empty_query().is_empty());
        assert_eq!(r.p.route(), None);
        assert_eq!(r.p.name(), "path");
    }
}
