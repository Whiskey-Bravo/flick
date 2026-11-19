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
use crate::cloud_roots::{self, CloudRoots};
use crate::file_ops::{self, FileOps};
use crate::dirs::{folder_key, size_text, DirEntry, FileSystem, Load, Loader};
use crate::fuzzy;
use crate::launch::Launcher;
use crate::provider::{Action, HelpLine, Modifier, Outcome, Provider, Query, ResultItem, Then};

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
    entry_row_in(cloud_roots::shared(), provider, path, is_dir, subtitle, score, complete)
}

/// [`entry_row`] with the cloud roots to consult.
pub(crate) fn entry_row_in(
    roots: &CloudRoots,
    provider: &'static str,
    path: &Path,
    is_dir: bool,
    subtitle: String,
    score: u32,
    complete: String,
) -> ResultItem {
    let text = path.to_string_lossy().into_owned();
    let title = path.file_name().map_or_else(|| text.clone(), |n| n.to_string_lossy().into_owned());
    let text_for_menu = text.clone();
    let mut id = text;
    if is_dir && !id.ends_with('\\') {
        id.push('\\');
    }
    ResultItem {
        folder: if is_dir { id.clone() } else { String::new() },
        id,
        provider,
        icon: file_icon(&title, is_dir),
        title,
        subtitle,
        score,
        pinnable: true,
        actions: file_ops::menu(is_dir, roots.contains(&text_for_menu), file_ops::is_drive_root(&text_for_menu)),
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
pub(crate) fn split_id(id: &str) -> (&str, bool) {
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
pub(crate) fn act_on_entry(
    fs: &dyn FileSystem,
    launcher: &dyn Launcher,
    clipboard: &dyn Clipboard,
    ops: &dyn FileOps,
    id: &str,
    action: Action,
) -> Result<Outcome, String> {
    if id.starts_with(INERT) || !handled(id, action) {
        return Err(CANT_DO.into());
    }
    let path = checked(fs, id)?;
    match action {
        Action::Reveal => launcher.reveal(path)?,
        Action::OpenFolder => launcher.open(id)?,
        Action::CopyPath => clipboard.set_text(path)?,
        _ => return run_entry_action(ops, path, action),
    }
    Ok(Outcome::Hide)
}

/// Whether the actions menu of the row `id` handles `action` (Rename is the
/// `rename` command and More options is not built yet; a drive root cannot be
/// changed).
pub(crate) fn handled(id: &str, action: Action) -> bool {
    match action {
        Action::Reveal
        | Action::OpenFolder
        | Action::CopyPath
        | Action::OpenTerminal
        | Action::Properties
        | Action::OpenWith
        | Action::CopyFile => true,
        Action::Compress | Action::KeepOffline | Action::FreeUpSpace | Action::Delete => !file_ops::is_drive_root(id),
        _ => false,
    }
}

/// The actions that go through [`FileOps`], on `path` (already checked to
/// exist). Compress and the cloud pair run once the bar has hidden.
pub(crate) fn run_entry_action(ops: &dyn FileOps, path: &str, action: Action) -> Result<Outcome, String> {
    let p = Path::new(path);
    let later = |then| Ok(Outcome::HideThen { then });
    match action {
        Action::OpenTerminal => ops.open_terminal(p)?,
        Action::Properties => ops.properties(p)?,
        Action::OpenWith => ops.open_with(p)?,
        Action::CopyFile => ops.copy_file(p)?,
        // The bar stays: the rows refresh, and the engine forgets the item.
        Action::Delete => {
            ops.recycle(p)?;
            return Ok(Outcome::Removed);
        }
        Action::Compress => return later(Then::Compress { path: path.to_string() }),
        Action::KeepOffline => return later(Then::CloudPin { path: path.to_string(), keep: true }),
        Action::FreeUpSpace => return later(Then::CloudPin { path: path.to_string(), keep: false }),
        _ => return Err(CANT_DO.into()),
    }
    Ok(Outcome::Hide)
}

/// The text of the refusal to rename onto a name that is taken.
const NAME_TAKEN: &str = "A file with that name already exists";

/// Renames the file or folder row `id` to `new_name` in its folder; the new
/// row id (a folder's ends with `\`). `kind` says what is at a path now
/// (`Some(true)` folder, `Some(false)` file, `None` nothing). The name is
/// checked first; then the row must still be there as the same kind and the
/// name free (a change of letter case alone is fine); nothing else happens
/// on a refusal.
pub(crate) fn rename_entry(
    kind: &dyn Fn(&Path) -> Option<bool>,
    ops: &dyn FileOps,
    id: &str,
    new_name: &str,
) -> Result<String, String> {
    file_ops::valid_new_name(new_name)?;
    let (path, is_dir) = split_id(id);
    if id.starts_with(INERT) || file_ops::is_drive_root(path) || kind(Path::new(path)) != Some(is_dir) {
        return Err(NOT_FOUND.into());
    }
    let old = Path::new(path);
    let name = old.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name == new_name {
        return Ok(id.to_string());
    }
    let target = old.with_file_name(new_name);
    if name.to_lowercase() != new_name.to_lowercase() && kind(&target).is_some() {
        return Err(NAME_TAKEN.into());
    }
    let renamed = ops.rename(old, new_name)?;
    let renamed = renamed.to_string_lossy();
    Ok(if is_dir { format!("{}\\", renamed.trim_end_matches('\\')) } else { renamed.into_owned() })
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

/// A folder's listing after its entry `old` (any letter case) was renamed to
/// `new` (`None`: deleted): the entry keeps its place, kind, size and time.
pub(crate) fn moved_entries(entries: &[DirEntry], old: &str, new: Option<&str>) -> Vec<DirEntry> {
    let old = old.to_lowercase();
    entries
        .iter()
        .filter_map(|e| match (e.name.to_lowercase() == old, new) {
            (false, _) => Some(e.clone()),
            (true, Some(new)) => Some(DirEntry { name: new.to_string(), ..e.clone() }),
            (true, None) => None,
        })
        .collect()
}

/// The most rows an unfolded folder shows.
pub const UNFOLD_ROWS: usize = 15;

/// What a folder's unfolded part has to show right now.
#[derive(Debug, serde::Serialize)]
pub struct Unfolded {
    /// `reading`, `ready`, `missing` or `denied`.
    pub state: &'static str,
    pub rows: Vec<ResultItem>,
    /// Entries left out beyond [`UNFOLD_ROWS`].
    pub more: usize,
    /// The folder as the bar types it (see [`typed_form`]).
    pub typed: String,
}

/// The folder as typed into the bar: `~\rest\` when it is the home folder or
/// under it (compared without regard to case), else the full path; always
/// ends in `\`.
pub fn typed_form(folder: &Path, home: Option<&Path>) -> String {
    let full = folder.to_string_lossy().replace('/', "\\");
    let full = full.trim_end_matches('\\');
    let under_home = home.and_then(|h| {
        let h = h.to_string_lossy().replace('/', "\\");
        let h = h.trim_end_matches('\\').to_string();
        let head = full.get(..h.len())?;
        // A name that merely starts with the home's name is not under it.
        (!h.is_empty() && head.eq_ignore_ascii_case(&h) && (full.len() == h.len() || full[h.len()..].starts_with('\\')))
            .then(|| &full[h.len()..])
    });
    match under_home {
        Some(rest) => format!("~{rest}\\"),
        None => format!("{full}\\"),
    }
}

pub struct PathProvider {
    dirs: Arc<Loader<Vec<DirEntry>>>,
    fs: Arc<dyn FileSystem>,
    home: Option<PathBuf>,
    launcher: Arc<dyn Launcher>,
    clipboard: Arc<dyn Clipboard>,
    ops: Arc<dyn FileOps>,
}

impl PathProvider {
    pub fn new(
        dirs: Arc<Loader<Vec<DirEntry>>>,
        fs: Arc<dyn FileSystem>,
        home: Option<PathBuf>,
        launcher: Arc<dyn Launcher>,
        clipboard: Arc<dyn Clipboard>,
        ops: Arc<dyn FileOps>,
    ) -> Self {
        Self { dirs, fs, home, launcher, clipboard, ops }
    }

    /// `old` was renamed to `new` (`None`: deleted) through Flick. The listing
    /// of the folder holding it is patched in memory, so a typed path, `dl`
    /// and an unfolded folder answer at once (not `Reading…`) with the entry
    /// gone or renamed in place; what was read below `old` is forgotten.
    pub fn entry_moved(&self, old: &Path, new: Option<&Path>) {
        if let (Some(parent), Some(name)) = (old.parent(), old.file_name()) {
            let name = name.to_string_lossy();
            // A new name in the same folder; anything else leaves the folder.
            let renamed = new
                .filter(|n| n.parent().is_some_and(|p| folder_key(p) == folder_key(parent)))
                .and_then(|n| n.file_name())
                .map(|n| n.to_string_lossy().into_owned());
            self.dirs.patch(&folder_key(parent), |entries| moved_entries(entries, &name, renamed.as_deref()));
        }
        self.dirs.forget_under(&folder_key(old));
    }

    /// What the folder under a row has to show: memory only, like `query`. A
    /// folder the loader has not read yet starts its read and answers
    /// `reading`; the page asks again when the read is done. A path that is
    /// not absolute is never handed to the file system.
    pub fn unfold(&self, folder: &str) -> Unfolded {
        let path = Path::new(folder);
        let typed = typed_form(path, self.home.as_deref());
        let answer = |state, rows, more| Unfolded { state, rows, more, typed: typed.clone() };
        if !path.is_absolute() {
            return answer("missing", Vec::new(), 0);
        }
        let (fs, dir) = (Arc::clone(&self.fs), path.to_path_buf());
        let entries = match self.dirs.get(&folder_key(path), move || fs.read_dir(&dir)) {
            Load::Pending => return answer("reading", Vec::new(), 0),
            Load::NotFound => return answer("missing", Vec::new(), 0),
            Load::Denied | Load::Slow => return answer("denied", Vec::new(), 0),
            Load::Ready(entries) => entries,
        };
        // Folders first, then files, each by name without regard to case.
        let mut sorted: Vec<(bool, String, &DirEntry)> = entries.iter().map(|e| (!e.is_dir, e.name.to_lowercase(), e)).collect();
        sorted.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.name.cmp(&b.2.name)));
        let rows = sorted
            .iter()
            .take(UNFOLD_ROWS)
            .map(|(_, _, e)| {
                let subtitle = if e.is_dir { "Folder".to_string() } else { size_text(e.size) };
                entry_row(NAME, &path.join(&e.name), e.is_dir, subtitle, 0, String::new())
            })
            .collect();
        answer("ready", rows, entries.len().saturating_sub(UNFOLD_ROWS))
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
            own.folder = String::new();
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
        act_on_entry(&*self.fs, &*self.launcher, &*self.clipboard, &*self.ops, id, action)
    }

    fn rename(&self, id: &str, new_name: &str) -> Result<String, String> {
        rename_entry(&|p| self.fs.kind(p), &*self.ops, id, new_name)
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
    use crate::file_ops::FakeFileOps;
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
        ops: Arc<FakeFileOps>,
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
        let ops = Arc::new(FakeFileOps::default());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let dirs = Loader::new(
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
            FRESH,
            slow,
        );
        let p = PathProvider::new(dirs, fs.clone(), Some(PathBuf::from(HOME)), launcher.clone(), clip.clone(), ops.clone());
        Rig { p, fs, launcher, clip, ops, rx }
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
    fn path_rows_carry_the_new_menu() {
        use Action::*;
        let roots = CloudRoots::default();
        roots.set(vec![PathBuf::from(r"C:\Cloud")]);
        let row = |p: &str, dir| entry_row_in(&roots, "path", Path::new(p), dir, String::new(), 0, String::new()).actions;
        assert_eq!(row(r"C:\Cloud\a.txt", false), [Reveal, OpenWith, CopyFile, CopyPath, Rename, Delete, KeepOffline, FreeUpSpace, MoreOptions]);
        assert_eq!(row(r"C:\Cloud\d", true), [OpenFolder, OpenTerminal, CopyPath, Compress, Properties, KeepOffline, FreeUpSpace, MoreOptions]);
        assert_eq!(row(r"C:\CloudBackup\d", true), [OpenFolder, OpenTerminal, CopyPath, Compress, Properties, MoreOptions]);
        assert_eq!(row(r"C:\", true), [OpenFolder, OpenTerminal, CopyPath, Properties, MoreOptions]);
        // Rename is the rename command and More options is not handled yet; a drive root cannot be deleted.
        let r = rig();
        for action in [Rename, MoreOptions] {
            assert_eq!(r.p.act(r"C:\a.txt", action), Err("can't do that here".to_string()));
        }
        assert_eq!(r.p.act(r"C:\", Delete), Err("can't do that here".to_string()));
        assert!(r.ops.calls().is_empty());
        assert_eq!(r.p.act(r"C:\a.txt", Delete), Ok(Outcome::Removed));
        assert_eq!(r.ops.calls(), [r"recycle C:\a.txt"]);
    }

    #[test]
    fn act_dispatches_each_entry() {
        use Action::*;
        let r = rig();
        let a = r.ready(r"C:\").into_iter().find(|x| x.id == r"C:\a.txt").unwrap();
        let games = r.ready(r"C:\").into_iter().find(|x| x.id == r"C:\Games\").unwrap();
        assert_eq!(r.p.act(&games.id, OpenTerminal), Ok(Outcome::Hide));
        assert_eq!(r.p.act(&games.id, Properties), Ok(Outcome::Hide));
        assert_eq!(r.p.act(&a.id, Properties), Ok(Outcome::Hide));
        assert_eq!(r.p.act(&a.id, OpenWith), Ok(Outcome::Hide));
        assert_eq!(r.p.act(&a.id, CopyFile), Ok(Outcome::Hide));
        assert_eq!(
            r.ops.calls(),
            [
                r"open_terminal C:\Games",
                r"properties C:\Games",
                r"properties C:\a.txt",
                r"open_with C:\a.txt",
                r"copy_file C:\a.txt"
            ]
        );
        assert!(r.launcher.calls().is_empty() && r.clip.0.lock().unwrap().is_empty());
    }

    #[test]
    fn compress_and_cloud_run_after_hide() {
        use Action::*;
        let r = rig();
        let then = |action, id: &str| match r.p.act(id, action) {
            Ok(Outcome::HideThen { then }) => then,
            other => panic!("{action:?}: {other:?}"),
        };
        assert_eq!(then(Compress, r"C:\Games\"), Then::Compress { path: r"C:\Games".into() });
        assert_eq!(then(KeepOffline, r"C:\a.txt"), Then::CloudPin { path: r"C:\a.txt".into(), keep: true });
        assert_eq!(then(FreeUpSpace, r"C:\a.txt"), Then::CloudPin { path: r"C:\a.txt".into(), keep: false });
        assert!(r.ops.calls().is_empty(), "the work waits for the bar to hide");
        // A drive root cannot be compressed or kept offline.
        for action in [Compress, KeepOffline, FreeUpSpace] {
            assert_eq!(r.p.act(r"C:\", action), Err("can't do that here".to_string()));
        }
    }

    #[test]
    fn act_on_a_missing_path_fails_without_calling_ops() {
        use Action::*;
        let r = rig();
        r.fs.set_kind(r"C:\a.txt", None);
        for action in [OpenTerminal, Properties, OpenWith, CopyFile, Compress, KeepOffline, FreeUpSpace] {
            assert_eq!(r.p.act(r"C:\a.txt", action), Err(NOT_FOUND.to_string()), "{action:?}");
            assert_eq!(r.p.act(r"C:\gone\", action), Err(NOT_FOUND.to_string()), "{action:?}");
        }
        assert!(r.ops.calls().is_empty());
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
        assert_eq!(rows[0].actions, file_ops::menu(true, false, true));
        assert_eq!(rows[1].id, r"C:\Games\");
        assert_eq!(rows[1].icon, "folder");
        assert_eq!(rows[3].id, r"C:\a.txt");
        assert_eq!(rows[3].icon, "ext:.txt");
        assert_eq!(rows[3].actions, file_ops::menu(false, false, false));
        assert!(rows.iter().all(|r| r.provider == "path" && r.pinnable));
        assert_scores_fall(&rows);
    }

    #[test]
    fn folder_rows_carry_their_path_with_a_backslash_and_files_and_the_top_row_do_not() {
        let r = rig();
        let rows = r.ready(r"C:\");
        assert_eq!(rows[0].folder, "", "This folder");
        assert_eq!(rows[1].folder, r"C:\Games\");
        assert_eq!(rows[2].folder, r"C:\Users\");
        assert_eq!(rows[3].folder, "");
        assert_eq!(r.p.item(r"C:\Games\").unwrap().folder, r"C:\Games\");
        assert_eq!(r.p.item(r"C:\a.txt").unwrap().folder, "");
        assert_eq!(r.p.item(r"C:\").unwrap().folder, r"C:\");
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
        assert_eq!(row.actions, file_ops::menu(true, false, false));
        assert!(row.pinnable);
        let row = r.p.item(r"C:\a.txt").unwrap();
        assert_eq!(row.actions, file_ops::menu(false, false, false));
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
    fn typed_form_shortens_home_and_always_ends_with_a_backslash() {
        let home = Some(Path::new(r"C:\Users\me"));
        let form = |p: &str, h: Option<&Path>| typed_form(Path::new(p), h);
        assert_eq!(form(r"C:\Users\me\Documents\Projects", home), r"~\Documents\Projects\");
        assert_eq!(form(r"C:\Users\me", home), r"~\");
        assert_eq!(form(r"C:\Users\me\", home), r"~\");
        assert_eq!(form(r"c:\users\ME\x", home), r"~\x\");
        assert_eq!(form(r"C:\Users\meadow\x", home), r"C:\Users\meadow\x\");
        assert_eq!(form(r"D:\", home), r"D:\");
        assert_eq!(form(r"\\srv\share\x y", home), r"\\srv\share\x y\");
        assert_eq!(form(r"C:\Users\me\Documents", None), r"C:\Users\me\Documents\");
        assert_eq!(form("C:/Users/me/a", home), r"~\a\");
    }

    impl Rig {
        fn unfold_ready(&self, folder: &str) -> Unfolded {
            let u = self.p.unfold(folder);
            if u.state == "reading" {
                self.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready within 1 s");
                return self.p.unfold(folder);
            }
            u
        }
    }

    #[test]
    fn unfold_of_an_unread_folder_reads_once_then_is_ready() {
        let r = rig();
        let u = r.p.unfold(r"C:\Games\");
        assert_eq!((u.state, u.rows.len(), u.more), ("reading", 0, 0));
        r.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready");
        assert_eq!(r.p.unfold(r"C:\").state, "reading", "another folder starts its own read");
        r.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready");
        assert_eq!(r.p.unfold(r"C:\").state, "ready");
        assert_eq!(r.p.unfold(r"C:\").state, "ready");
        assert_eq!(r.fs.reads(), 2, "one read per folder");
    }

    #[test]
    fn unfold_lists_folders_first_then_files_each_by_name_ignoring_case() {
        let fs = FakeFs::default();
        fs.add_dir(
            r"C:\Work",
            vec![file("b.txt", 2048), folder("zeta"), file("A.txt", 10), folder("Alpha"), file("c.txt", 0), folder("beta")],
        );
        let r = rig_with(fs, SLOW);
        let u = r.unfold_ready(r"C:\Work\");
        assert_eq!(u.state, "ready");
        assert_eq!(titles(&u.rows), ["Alpha", "beta", "zeta", "A.txt", "b.txt", "c.txt"]);
        let subs: Vec<&str> = u.rows.iter().map(|r| r.subtitle.as_str()).collect();
        assert_eq!(subs, ["Folder", "Folder", "Folder", "10 B", "2.0 KB", "0 B"]);
        for row in &u.rows {
            let is_dir = row.id.ends_with('\\');
            let want = entry_row("path", Path::new(row.id.trim_end_matches('\\')), is_dir, row.subtitle.clone(), 0, String::new());
            assert_eq!((&row.id, &row.title, row.provider, &row.icon, &row.actions), (&want.id, &want.title, "path", &want.icon, &want.actions));
            assert_eq!(row.complete, "");
            assert_eq!(row.folder, want.folder);
        }
        assert_eq!(u.rows[0].id, r"C:\Work\Alpha\");
        assert_eq!(u.rows[0].folder, r"C:\Work\Alpha\");
        assert_eq!(u.rows[3].id, r"C:\Work\A.txt");
        assert_eq!(u.rows[3].folder, "");
    }

    #[test]
    fn unfold_shows_at_most_15_rows_and_counts_the_rest() {
        let fs = FakeFs::default();
        let many: Vec<DirEntry> = (0..10_000).map(|i| file(&format!("f{i:05}.txt"), 1)).collect();
        fs.add_dir(r"C:\big", many);
        fs.add_dir(r"C:\few", (0..15).map(|i| file(&format!("f{i:02}"), 1)).collect());
        fs.add_dir(r"C:\none", Vec::new());
        let r = rig_with(fs, SLOW);
        let u = r.unfold_ready(r"C:\big\");
        assert_eq!((u.state, u.rows.len(), u.more), ("ready", UNFOLD_ROWS, 9985));
        assert_eq!(u.rows[0].title, "f00000.txt");
        let u = r.unfold_ready(r"C:\few\");
        assert_eq!((u.rows.len(), u.more), (15, 0));
        let u = r.unfold_ready(r"C:\none\");
        assert_eq!((u.state, u.rows.len(), u.more), ("ready", 0, 0));
    }

    #[test]
    fn unfold_says_missing_or_denied_when_the_folder_cannot_be_read() {
        let r = rig();
        r.fs.fail_dir(r"D:\", ReadError::Denied);
        r.fs.fail_dir(r"E:\gone", ReadError::NotFound);
        assert_eq!(r.unfold_ready(r"E:\gone\").state, "missing");
        assert_eq!(r.unfold_ready(r"F:\never\").state, "missing");
        let u = r.unfold_ready(r"D:\");
        assert_eq!((u.state, u.rows.len()), ("denied", 0));

        let r = rig_with(FakeFs::default(), Duration::from_millis(50));
        r.fs.block();
        assert_eq!(r.p.unfold(r"F:\").state, "reading");
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(r.p.unfold(r"F:\").state, "denied");
        r.fs.release();
    }

    #[test]
    fn unfold_of_a_relative_path_is_missing_and_asks_the_file_system_nothing() {
        let r = rig();
        for path in [r"foo\bar", "", "~", r"\x"] {
            let u = r.p.unfold(path);
            assert_eq!((u.state, u.rows.len(), u.more), ("missing", 0, 0), "{path:?}");
        }
        assert!(r.rx.recv_timeout(Duration::from_millis(100)).is_err(), "no read started");
        assert_eq!(r.fs.reads(), 0);
    }

    #[test]
    fn unfold_lists_odd_names_unchanged() {
        let fs = FakeFs::default();
        fs.add_dir(r"C:\odd", vec![file("a b.txt", 1), file("#1.txt", 1), file("50%.txt", 1), file("it's.txt", 1), folder("ü dir")]);
        let r = rig_with(fs, SLOW);
        let u = r.unfold_ready(r"C:\odd\");
        assert_eq!(titles(&u.rows), ["ü dir", "#1.txt", "50%.txt", "a b.txt", "it's.txt"]);
        assert_eq!(u.rows[0].id, "C:\\odd\\ü dir\\");
        assert_eq!(u.rows[4].id, r"C:\odd\it's.txt");
    }

    #[test]
    fn unfold_answers_with_the_typed_form_of_the_folder() {
        let r = rig();
        assert_eq!(r.unfold_ready(r"C:\Users\someone\Documents\").typed, r"~\Documents\");
        assert_eq!(r.p.unfold(r"C:\").typed, r"C:\");
        assert_eq!(r.p.unfold(r"C:\Users\someone\").typed, r"~\");
    }

    #[test]
    fn unfold_never_reads_on_the_calling_thread() {
        let r = rig();
        r.fs.block();
        let t = Instant::now();
        let u = r.p.unfold(r"C:\");
        assert!(t.elapsed() < Duration::from_millis(50));
        assert_eq!((u.state, u.rows.len()), ("reading", 0));
        assert_eq!(r.fs.reads(), 0);
        r.fs.release();
        r.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready");
    }

    #[test]
    fn rows_inside_an_unfolded_folder_open_until_the_entry_is_gone() {
        let r = rig();
        let u = r.unfold_ready(r"C:\");
        let games = u.rows.iter().find(|x| x.title == "Games").unwrap();
        assert_eq!(r.p.activate(&games.id, Modifier::None), Ok(Outcome::Hide));
        r.fs.set_kind(r"C:\Games", None);
        assert_eq!(r.p.activate(&games.id, Modifier::None), Err(NOT_FOUND.to_string()));
        let a = u.rows.iter().find(|x| x.title == "a.txt").unwrap();
        r.fs.set_kind(r"C:\a.txt", None);
        assert_eq!(r.p.activate(&a.id, Modifier::None), Err(NOT_FOUND.to_string()));
    }

    #[test]
    fn a_delete_or_rename_patches_the_typed_listing_and_it_answers_at_once() {
        let r = rig();
        assert_eq!(titles(&r.ready(r"C:\")), [r"C:\", "Games", "Users", "a.txt", "b.txt"]);
        let reads = r.fs.reads();
        r.p.entry_moved(Path::new(r"C:\A.TXT"), None);
        assert_eq!(titles(&r.q(r"C:\")), [r"C:\", "Games", "Users", "b.txt"], "not Reading…");
        r.p.entry_moved(Path::new(r"C:\b.txt"), Some(Path::new(r"C:\c.txt")));
        let rows = r.q(r"C:\");
        assert_eq!(titles(&rows), [r"C:\", "Games", "Users", "c.txt"]);
        assert_eq!(rows[3].id, r"C:\c.txt");
        assert_eq!(rows[3].subtitle, "2.0 KB", "size kept");
        // A folder renamed: the same row, the new name; its own listing is read again.
        r.fs.add_dir(r"C:\Games", vec![file("save.dat", 1)]);
        assert_eq!(r.unfold_ready(r"C:\Games\").state, "ready");
        r.p.entry_moved(Path::new(r"C:\Games"), Some(Path::new(r"C:\Play")));
        assert_eq!(titles(&r.q(r"C:\")), [r"C:\", "Play", "Users", "c.txt"]);
        assert_eq!(r.fs.reads(), reads + 1, "only the unfold read the disk");
        assert_eq!(r.p.unfold(r"C:\Games\").state, "reading", "forgotten below the old name");
        r.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready");
    }

    #[test]
    fn moved_entries_drop_or_rename_in_place() {
        let entries = vec![folder("Sub"), file("a.txt", 5), file("b.txt", 6)];
        let names = |v: Vec<DirEntry>| v.into_iter().map(|e| e.name).collect::<Vec<_>>();
        assert_eq!(names(moved_entries(&entries, "A.TXT", None)), ["Sub", "b.txt"]);
        let renamed = moved_entries(&entries, "sub", Some("New"));
        assert_eq!(renamed[0], DirEntry { name: "New".into(), ..folder("Sub") });
        assert_eq!(names(moved_entries(&entries, "zzz", None)), ["Sub", "a.txt", "b.txt"]);
    }

    #[test]
    fn help_line_and_no_empty_bar_rows() {
        let r = rig();
        let help = r.p.help();
        assert_eq!(help[0].example, r"C:\ / ~\Documents\");
        assert_eq!(help[0].about, "browse a folder by typing its path; Tab completes a name");
        assert_eq!(help[0].fill, r"~\");
        assert_eq!(help.len(), 1);
        assert!(r.p.empty_query().is_empty());
        assert_eq!(r.p.route(), None);
        assert_eq!(r.p.name(), "path");
    }
}
