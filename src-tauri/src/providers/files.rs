//! Files provider: file and folder names under a few root folders, searched
//! in memory. The index is built off the query path (a background scan, kept
//! fresh by a folder watcher, and by the periodic refresh when the watcher
//! lost track of something) and swapped in whole.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

#[cfg(test)]
use super::files_scan::skip_by_attributes;
use super::files_scan::{char_bit, is_junk_dir, scan_index, Index, Memory, Sizes, Slot};
use crate::clipboard::Clipboard;
use crate::fuzzy;
use crate::launch::Launcher;
use crate::timing;
use crate::provider::{Action, HelpLine, Modifier, Outcome, Provider, Query, ResultItem};

pub const MAX_ENTRIES: usize = 1_000_000;
pub const MAX_DEPTH: usize = 9;

/// The provider caps its own output.
const MAX_RESULTS: usize = 20;
/// Rows of a plain query's list files may take while other results exist.
const PLAIN_SHARE: usize = 5;
/// Shorter text matches far too much to be useful.
const MIN_QUERY_CHARS: usize = 2;
/// A scan asks whether a newer search has begun once per this many slots.
const CANCEL_CHECK_SLOTS: usize = 4096;
/// One directory listing adds at most this many entries.
pub const MAX_PER_FOLDER: usize = 20_000;
/// A watcher-triggered rescan starts this long after the last relevant change...
const QUIET: Duration = Duration::from_secs(2);
/// ...but no later than this long after the first change of a burst.
const MAX_WAIT: Duration = Duration::from_secs(10);
/// Watcher-triggered rescans start at least this long apart...
const MIN_RESCAN_GAP: Duration = Duration::from_secs(30);
/// ...and at least this many times the duration of the last full scan.
const RESCAN_GAP_FACTOR: u32 = 3;
/// A clean, watched index is still rescanned by the periodic tick once the
/// last completed full scan is this old (a watcher that died silently).
const MAX_CLEAN_AGE: Duration = Duration::from_secs(2 * 60 * 60);
const NOT_FOUND: &str = "file not found";

/// One indexed file or folder in list form, for tests (the provider keeps the
/// compact `Index`).
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub name_norm: String,
    pub is_dir: bool,
    /// Levels below its root: 1 for an entry directly in a root.
    pub depth: u8,
}

#[cfg(test)]
impl FileEntry {
    /// An entry directly in a root (depth 1).
    pub fn new(path: PathBuf, is_dir: bool) -> Self {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        let name_norm = fuzzy::normalize(&name);
        Self { path, name, name_norm, is_dir, depth: 1 }
    }

    pub fn at_depth(self, depth: u8) -> Self {
        Self { depth, ..self }
    }
}

/// The whole user folder (`%USERPROFILE%`); empty without it.
pub fn default_roots() -> Vec<PathBuf> {
    roots_for_profile(std::env::var_os("USERPROFILE").as_deref())
}

fn roots_for_profile(profile: Option<&OsStr>) -> Vec<PathBuf> {
    profile.map(|p| vec![PathBuf::from(p)]).unwrap_or_default()
}

/// Every file and folder under `roots`, breadth-first: skips hidden and
/// system entries, dot-names and build/tool folders, does not follow links,
/// stops at `MAX_DEPTH` levels, at an equal share of `MAX_ENTRIES` per root
/// and at `MAX_PER_FOLDER` entries per folder. Never panics on missing or
/// unreadable folders.
/// The provider keeps the compact index; this list form is for tests.
#[cfg(test)]
pub fn scan(roots: &[PathBuf]) -> Vec<FileEntry> {
    scan_with_limits(roots, MAX_ENTRIES, MAX_DEPTH, MAX_PER_FOLDER)
}

#[cfg(test)]
fn scan_with_limits(roots: &[PathBuf], max_entries: usize, max_depth: usize, max_per_folder: usize) -> Vec<FileEntry> {
    scan_index(roots, None, max_entries, max_depth, max_per_folder, Sizes::default()).entries()
}

/// `dir` with a leading `home` shown as `~`, always with `\` separators
/// (roots in the config may be written with `/`).
fn shorten_home(dir: &Path, home: Option<&Path>) -> String {
    let shown = match home.and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!(r"~\{}", rest.display()),
        None => dir.display().to_string(),
    };
    shown.replace('/', r"\")
}

/// Folder scores per tier; a file scores 10 less. All stay below 650, the
/// lowest name match of an app, shortcut or Settings page.
const EXACT: u32 = 640;
const PREFIX: u32 = 600;
const SQUASHED_PREFIX: u32 = 580;
const WORD_PREFIX: u32 = 560;
const SUBSTRING: u32 = 520;
const FILE_BELOW_FOLDER: u32 = 10;
/// Points lost per level below the first, for at most `MAX_DEPTH_STEPS` levels.
const DEPTH_STEP: u32 = 15;
const MAX_DEPTH_STEPS: u32 = 4;
/// Deeper than this below its root, only an exact name (or file stem) is
/// found: the deep insides of SDKs and tool checkouts would otherwise fill
/// the list for any short word. Path browsing still reaches them.
const PARTIAL_MATCH_MAX_DEPTH: u8 = 6;

/// Points an entry loses for sitting `depth` levels below its root (1 = directly in it).
fn depth_penalty(depth: u8) -> u32 {
    u32::from(depth.saturating_sub(1)).min(MAX_DEPTH_STEPS) * DEPTH_STEP
}

/// Score of a file or folder name `hay` for `needle` (both normalised;
/// `len` = needle chars), strict tiers only: exact name, or exact stem for a
/// file (2+ chars); prefix, squashed prefix, word prefix or every typed word a
/// word prefix (3+ chars); substring (4+ chars). Never acronyms, initials or
/// typos. Below [`PARTIAL_MATCH_MAX_DEPTH`] levels, exact only. Minus
/// [`depth_penalty`].
///
/// A needle of fewer than 3 chars can only be exact, so it is decided by
/// name and stem alone and the fuzzy ladder never runs for it (the fuzzy
/// score is 1000 exactly when needle and name are equal).
fn file_score(needle: &str, len: usize, hay: &str, is_dir: bool, depth: u8) -> Option<u32> {
    if len < 3 {
        let exact = !needle.is_empty()
            && (hay == needle || (!is_dir && hay.rfind('.').is_some_and(|dot| dot > 0 && &hay[..dot] == needle)));
        if !exact {
            return None;
        }
        let file = if is_dir { 0 } else { FILE_BELOW_FOLDER };
        return Some(EXACT - file - depth_penalty(depth));
    }
    let f = fuzzy::score_prenormalized(needle, hay);
    let stem_exact = !is_dir && hay.rfind('.').is_some_and(|dot| dot > 0 && &hay[..dot] == needle);
    let tier = if f == Some(1000) || stem_exact {
        EXACT
    } else if depth > PARTIAL_MATCH_MAX_DEPTH {
        return None;
    } else {
        match f? {
            900 => PREFIX,
            850 => SQUASHED_PREFIX,
            800 => WORD_PREFIX,
            _ if len >= 4 && hay.contains(needle) => SUBSTRING,
            _ => return None,
        }
    };
    let file = if is_dir { 0 } else { FILE_BELOW_FOLDER };
    Some(tier - file - depth_penalty(depth))
}

/// The scoring before short needles skipped the fuzzy ladder: what
/// [`file_score`] must keep returning.
#[cfg(test)]
fn file_score_reference(needle: &str, len: usize, hay: &str, is_dir: bool, depth: u8) -> Option<u32> {
    let f = fuzzy::score_prenormalized(needle, hay);
    let stem_exact = !is_dir && hay.rfind('.').is_some_and(|dot| dot > 0 && &hay[..dot] == needle);
    let tier = if f == Some(1000) || stem_exact {
        EXACT
    } else if len < 3 || depth > PARTIAL_MATCH_MAX_DEPTH {
        return None;
    } else {
        match f? {
            900 => PREFIX,
            850 => SQUASHED_PREFIX,
            800 => WORD_PREFIX,
            _ if len >= 4 && hay.contains(needle) => SUBSTRING,
            _ => return None,
        }
    };
    let file = if is_dir { 0 } else { FILE_BELOW_FOLDER };
    Some(tier - file - depth_penalty(depth))
}

/// Whether a change at one of `paths` can alter the index: not inside a
/// skipped folder (see [`path_is_skipped`]). Unknown places count.
fn change_is_relevant(paths: &[PathBuf], roots: &[PathBuf]) -> bool {
    paths.is_empty() || paths.iter().any(|p| !path_is_skipped(p, roots))
}

/// Whether a watcher event can change the index. The index holds only names
/// and folder-ness, so only creations, removals and renames count, plus
/// `Other` and events whose backend lost track (`need_rescan`). Content
/// writes (`Modify(Any)` on Windows), metadata and access do not trigger a
/// rescan: a file written continuously must not rescan the roots over and over.
/// They still mark the index dirty (except plain content writes and reads, see
/// [`ignored_event_may_matter`]), so hidden or system attribute changes are
/// left to the periodic refresh.
fn event_triggers_rescan(kind: &EventKind, need_rescan: bool) -> bool {
    need_rescan
        || matches!(
            kind,
            EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_)) | EventKind::Other
        )
}

/// Whether the periodic refresh must rescan: when changes are known to be
/// unapplied (`dirty`), when nothing watches the roots (then it is the only
/// way changes are found), or when the last completed full scan is older than
/// [`MAX_CLEAN_AGE`] (`age`; `None`: no scan yet). The age cap bounds the
/// damage of a watcher that stopped without telling: on Windows the `notify`
/// backend never reports a buffer overflow or a vanished root to its handler,
/// it just stops watching. On a PC in use, ordinary file saves mark the index
/// dirty, so the saving is mostly while the PC is idle.
fn periodic_due(dirty: bool, watching: bool, age: Option<Duration>) -> bool {
    dirty || !watching || age.is_some_and(|a| a >= MAX_CLEAN_AGE)
}

/// What the watcher knows about the index, shared with its callback.
#[derive(Default)]
struct WatchState {
    /// Changes may have happened that no scan has seen yet. Set whenever the
    /// watcher cannot vouch for the index; cleared when a scan starts.
    dirty: AtomicBool,
    /// Every root is under a live watcher that can be trusted to report.
    watching: AtomicBool,
    /// Each root and whether it existed when the watch was set up.
    seen: Mutex<Vec<(PathBuf, bool)>>,
}

impl WatchState {
    /// Called once the watches are set up: remembers which roots exist, and
    /// vouches (`watching`) only when every root is watched (`all_watched`)
    /// and is on a local fixed drive (see [`root_is_trusted`]).
    fn arm(&self, roots: &[PathBuf], all_watched: bool, exists: impl Fn(&Path) -> bool, fixed: impl Fn(char) -> bool) {
        let trusted = roots.iter().all(|r| root_is_trusted(r, &fixed));
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = roots.iter().map(|r| (r.clone(), exists(r))).collect();
        self.watching.store(all_watched && trusted, Ordering::SeqCst);
    }

    /// A root that went away, or came back, without the watcher saying so
    /// (an ejected drive, a dropped share): stops vouching and marks dirty.
    /// It stays unwatched until the next `watch` or `set_roots`, so every
    /// tick then scans (today's behaviour).
    fn check_roots(&self, exists: impl Fn(&Path) -> bool) {
        // The lock is not held while the disk is asked.
        let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let changed = seen.iter().any(|(root, was)| exists(root) != *was);
        if changed {
            self.dirty.store(true, Ordering::SeqCst);
            self.watching.store(false, Ordering::SeqCst);
        }
    }

    fn periodic_due(&self, age: Option<Duration>) -> bool {
        periodic_due(self.dirty.load(Ordering::SeqCst), self.watching.load(Ordering::SeqCst), age)
    }
}

/// Whether events from `root` can be trusted: only a folder on a local fixed
/// drive. UNC paths (network shares) and anything without a drive letter are
/// not; `fixed` says whether a drive letter is a fixed disk.
fn root_is_trusted(root: &Path, fixed: impl Fn(char) -> bool) -> bool {
    let text = root.to_string_lossy().replace('/', r"\");
    // `\\?\C:\...` is a drive path; `\\?\UNC\...` and `\\server\...` are not.
    let text = match text.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_string(),
        None if text.starts_with(r"\\") => return false,
        None => text,
    };
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic() => fixed(letter),
        _ => false,
    }
}

/// Whether drive `letter` is a fixed disk (not removable, network or unknown).
fn os_drive_is_fixed(letter: char) -> bool {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    const DRIVE_FIXED: u32 = 3;
    unsafe { GetDriveTypeW(&HSTRING::from(format!(r"{letter}:\"))) == DRIVE_FIXED }
}

fn path_exists(p: &Path) -> bool {
    p.exists()
}

/// Whether an event the rescan logic ignores could still be a change to the
/// index (a hidden or system attribute flipped, a kind the backend could not
/// name). Only plain content writes and reads cannot.
fn ignored_event_may_matter(kind: &EventKind) -> bool {
    !matches!(kind, EventKind::Access(_) | EventKind::Modify(ModifyKind::Data(_)))
}

/// Whether the event removes or renames one of `roots`, or a folder above it.
fn event_ends_a_root(kind: &EventKind, paths: &[PathBuf], roots: &[PathBuf]) -> bool {
    if !matches!(kind, EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))) {
        return false;
    }
    let norm = |p: &Path| p.to_string_lossy().replace('/', r"\").trim_end_matches('\\').to_ascii_lowercase();
    paths.iter().any(|p| {
        let p = norm(p);
        roots.iter().any(|r| {
            let r = norm(r);
            r == p || r.starts_with(&format!(r"{p}\"))
        })
    })
}

/// What the watcher callback does with one result. Marks `state` when the
/// index may have gone stale, and returns the paths of an event that should
/// trigger a rescan. No disk access.
fn on_watch_result(
    state: &WatchState,
    roots: &Mutex<Vec<PathBuf>>,
    res: notify::Result<notify::Event>,
) -> Option<Vec<PathBuf>> {
    // An error means events were missed. On Windows the backend never reports
    // runtime errors (a buffer overflow, a vanished root): the watcher can stop
    // without telling, and the age cap of the periodic tick bounds the damage.
    let Ok(event) = res else {
        state.dirty.store(true, Ordering::SeqCst);
        return None;
    };
    // The backend says it lost track: whatever the kind, wherever the path.
    if event.need_rescan() {
        state.dirty.store(true, Ordering::SeqCst);
    }
    let triggers = event_triggers_rescan(&event.kind, event.need_rescan());
    if !triggers && !ignored_event_may_matter(&event.kind) {
        return None;
    }
    let roots = roots.lock().unwrap_or_else(|e| e.into_inner());
    // A root that goes away is no longer watched, even if it comes back: it
    // stays unwatched until the next `watch`/`set_roots`, so every tick scans
    // (today's behaviour).
    if event_ends_a_root(&event.kind, &event.paths, &roots) {
        state.watching.store(false, Ordering::SeqCst);
        state.dirty.store(true, Ordering::SeqCst);
    }
    if !change_is_relevant(&event.paths, &roots) {
        return None;
    }
    drop(roots);
    // Marked until a scan starts; an event the rescan logic ignores still
    // stays marked, for the periodic refresh.
    state.dirty.store(true, Ordering::SeqCst);
    triggers.then_some(event.paths)
}

/// When a watcher-triggered rescan wanted at `ready` may start, given when the
/// last full scan started and how long it took (`None`: none yet): not before
/// `max(30 s, 3 x duration)` after that scan started.
fn next_rescan_at(last: Option<(Instant, Duration)>, ready: Instant) -> Instant {
    match last {
        Some((start, took)) => ready.max(start + MIN_RESCAN_GAP.max(took * RESCAN_GAP_FACTOR)),
        None => ready,
    }
}

/// The last scan as `next_rescan_at` takes it: a scan still running (no
/// duration yet) counts as having taken as long as it has run so far, so a
/// rescan never starts right behind it.
fn last_scan_so_far(last: Option<(Instant, Option<Duration>)>, now: Instant) -> Option<(Instant, Duration)> {
    last.map(|(start, took)| (start, took.unwrap_or_else(|| now.saturating_duration_since(start))))
}

/// When a burst of changes that began at `first` and last saw a change at
/// `last` should be rescanned: `QUIET` after the last change, capped at
/// `MAX_WAIT` after the first.
fn rescan_deadline(first: Instant, last: Instant) -> Instant {
    (last + QUIET).min(first + MAX_WAIT)
}

/// Waits out a burst of changes whose first came at `first`: until changes
/// have been quiet for `QUIET`, but never past `first + MAX_WAIT`, even while
/// changes keep arriving (a queued change is not read once the deadline has
/// passed). `recv` waits up to the given time for the next change and says
/// whether it is relevant; `now` is the clock. False when the channel closed.
fn wait_out_burst(
    first: Instant,
    mut recv: impl FnMut(Duration) -> Result<bool, RecvTimeoutError>,
    now: impl Fn() -> Instant,
) -> bool {
    let mut deadline = rescan_deadline(first, first);
    loop {
        let left = deadline.saturating_duration_since(now());
        if left.is_zero() {
            return true;
        }
        match recv(left) {
            Ok(true) => deadline = rescan_deadline(first, now()),
            Ok(false) => {}
            Err(RecvTimeoutError::Timeout) => return true,
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

/// Whether `path` lies inside a folder the scan skips: some part of it below
/// its root is a dot-name, or a folder named like one of the junk folders
/// (AppData, node_modules, ...) with something below it. A last part that is a
/// junk name is not skipped: it may be a file. Pure string work, no disk
/// access, so the watcher can call it for every event. Paths outside every
/// root are not skipped.
fn path_is_skipped(path: &Path, roots: &[PathBuf]) -> bool {
    let Some(rel) = roots.iter().find_map(|r| path.strip_prefix(r).ok()) else { return false };
    let mut parts = rel.components().peekable();
    while let Some(part) = parts.next() {
        let Some(name) = part.as_os_str().to_str() else { continue };
        let is_last = parts.peek().is_none();
        if name.starts_with('.') || (!is_last && is_junk_dir(name)) {
            return true;
        }
    }
    false
}

pub struct FilesProvider {
    launcher: Arc<dyn Launcher>,
    /// For the Copy path action.
    clipboard: Arc<dyn Clipboard>,
    index: RwLock<Index>,
    roots: Arc<Mutex<Vec<PathBuf>>>,
    /// Bumped by `set_roots`; a scan started before the bump is thrown away.
    generation: AtomicU64,
    /// Whether a scan runs, and whether another was asked for meanwhile.
    refresh_state: Mutex<RefreshState>,
    /// Start and duration of the last full scan; no duration while it runs.
    last_scan: Mutex<Option<(Instant, Option<Duration>)>>,
    /// Signalled when a refresh run (with its follow-ups) ends.
    idle: Condvar,
    /// `%USERPROFILE%`, shown as `~` in subtitles.
    home: Option<PathBuf>,
    /// Alive while watching; replaced by each `watch`.
    watcher: Mutex<Option<RecommendedWatcher>>,
    /// Set once the first scan has been swapped in.
    scanned: AtomicBool,
    /// What the watcher vouches for; decides whether the periodic refresh scans.
    watch_state: Arc<WatchState>,
}

#[derive(Default)]
struct RefreshState {
    running: bool,
    again: bool,
}

/// Clears the refresh state if a scan panics, so later refreshes still run.
struct ClearOnPanic<'a>(&'a FilesProvider);

impl Drop for ClearOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let mut last = self.0.last_scan.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((start, None)) = *last {
                *last = Some((start, Some(start.elapsed())));
            }
            drop(last);
            *self.0.refresh_state.lock().unwrap_or_else(|e| e.into_inner()) = RefreshState::default();
            self.0.idle.notify_all();
        }
    }
}

impl FilesProvider {
    pub fn new(launcher: Arc<dyn Launcher>, clipboard: Arc<dyn Clipboard>) -> Arc<Self> {
        Self::with_index(launcher, clipboard, Index::default(), false)
    }

    /// An index that counts as scanned already, and a fake clipboard (tests).
    #[cfg(test)]
    pub fn with_entries(launcher: Arc<dyn Launcher>, e: Vec<FileEntry>) -> Arc<Self> {
        Self::with_clipboard(launcher, Arc::new(crate::clipboard::FakeClipboard::default()), e)
    }

    #[cfg(test)]
    pub fn with_clipboard(launcher: Arc<dyn Launcher>, clipboard: Arc<dyn Clipboard>, e: Vec<FileEntry>) -> Arc<Self> {
        Self::with_index(launcher, clipboard, Index::from_entries(e), true)
    }

    fn with_index(launcher: Arc<dyn Launcher>, clipboard: Arc<dyn Clipboard>, index: Index, scanned: bool) -> Arc<Self> {
        Arc::new(Self {
            launcher,
            clipboard,
            index: RwLock::new(index),
            roots: Arc::new(Mutex::new(default_roots())),
            generation: AtomicU64::new(0),
            refresh_state: Mutex::new(RefreshState::default()),
            last_scan: Mutex::new(None),
            idle: Condvar::new(),
            home: std::env::var_os("USERPROFILE").map(PathBuf::from),
            watcher: Mutex::new(None),
            scanned: AtomicBool::new(scanned),
            watch_state: Arc::new(WatchState::default()),
        })
    }

    /// Empty = [`default_roots`]. The current index keeps answering until the
    /// next `refresh`; a scan already running for the old roots is discarded.
    /// A running watcher moves to the new roots.
    pub fn set_roots(&self, roots: Vec<PathBuf>) {
        let roots = if roots.is_empty() { default_roots() } else { roots };
        let old = std::mem::replace(&mut *self.roots.lock().unwrap_or_else(|e| e.into_inner()), roots.clone());
        // The index is stale until the next scan, which clears this when it starts.
        self.watch_state.dirty.store(true, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(w) = self.watcher.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            for r in &old {
                let _ = w.unwatch(r);
            }
            // A root that cannot be watched (e.g. missing) misses events: the
            // periodic refresh then rescans every time.
            let mut all = true;
            for r in &roots {
                all &= w.watch(r, RecursiveMode::Recursive).is_ok();
            }
            self.watch_state.arm(&roots, all, path_exists, os_drive_is_fixed);
        }
    }

    fn roots(&self) -> Vec<PathBuf> {
        self.roots.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Full rescan of the roots (seconds; no lock held while walking), then
    /// swap. If a refresh is already running, this only asks it for one
    /// follow-up scan and returns at once, before that scan has run: any
    /// number of requests during a scan collapse into one follow-up, and
    /// scans never overlap.
    pub fn refresh(&self) {
        let home = self.home.clone();
        self.refresh_using(|roots, hint| scan_index(roots, home.as_deref(), MAX_ENTRIES, MAX_DEPTH, MAX_PER_FOLDER, hint));
    }

    /// The periodic refresh: a scan only when [`periodic_due`] says so (the
    /// watcher lost track of something, there is none, or the last scan is
    /// older than [`MAX_CLEAN_AGE`]).
    pub fn refresh_if_due(&self) {
        let home = self.home.clone();
        self.refresh_if_due_using(
            |roots, hint| scan_index(roots, home.as_deref(), MAX_ENTRIES, MAX_DEPTH, MAX_PER_FOLDER, hint),
            path_exists,
            Instant::now(),
        );
    }

    /// How long ago the last completed full scan finished, at `now` (`None`:
    /// none has completed yet; a running one does not reset the age).
    fn scan_age(&self, now: Instant) -> Option<Duration> {
        let last = *self.last_scan.lock().unwrap_or_else(|e| e.into_inner());
        last.and_then(|(start, took)| took.map(|t| now.saturating_duration_since(start + t)))
    }

    fn refresh_if_due_using(
        &self,
        scan: impl FnMut(&[PathBuf], Sizes) -> Index,
        exists: impl Fn(&Path) -> bool,
        now: Instant,
    ) {
        self.watch_state.check_roots(exists);
        if self.watch_state.periodic_due(self.scan_age(now)) {
            self.refresh_using(scan);
        }
    }

    /// `refresh` with the scan passed in (tests).
    #[cfg(test)]
    fn refresh_with(&self, mut scan: impl FnMut(&[PathBuf]) -> Vec<FileEntry>) {
        self.refresh_using(|roots, _| Index::from_entries(scan(roots)));
    }

    /// `refresh`, then wait until no refresh or follow-up is running (tests).
    #[cfg(test)]
    fn refresh_blocking(&self) {
        self.refresh();
        self.wait_idle();
    }

    /// Wait until no refresh or follow-up is running (tests).
    #[cfg(test)]
    fn wait_idle(&self) {
        let mut st = self.refresh_state.lock().unwrap_or_else(|e| e.into_inner());
        while st.running {
            st = self.idle.wait(st).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// The caller that finds no scan running runs it, then one follow-up if
    /// any was asked for meanwhile (again while more are asked for).
    fn refresh_using(&self, mut scan: impl FnMut(&[PathBuf], Sizes) -> Index) {
        {
            let mut st = self.refresh_state.lock().unwrap_or_else(|e| e.into_inner());
            if st.running {
                st.again = true;
                return;
            }
            st.running = true;
        }
        let _clear = ClearOnPanic(self);
        loop {
            self.scan_and_swap(&mut scan);
            let mut st = self.refresh_state.lock().unwrap_or_else(|e| e.into_inner());
            if !st.again {
                st.running = false;
                self.idle.notify_all();
                return;
            }
            st.again = false;
        }
    }

    /// Scans until a scan finishes with the roots it started with, and swaps
    /// that one in: a stale result never replaces a newer one.
    fn scan_and_swap(&self, scan: &mut impl FnMut(&[PathBuf], Sizes) -> Index) {
        loop {
            let generation = self.generation.load(Ordering::SeqCst);
            // Cleared before the walk, so a change during it marks the index dirty again.
            self.watch_state.dirty.store(false, Ordering::SeqCst);
            let started = Instant::now();
            *self.last_scan.lock().unwrap_or_else(|e| e.into_inner()) = Some((started, None));
            let hint = self.index.read().unwrap_or_else(|e| e.into_inner()).sizes();
            let fresh = scan(&self.roots(), hint);
            *self.last_scan.lock().unwrap_or_else(|e| e.into_inner()) = Some((started, Some(started.elapsed())));
            // Measured before the write lock is taken (it walks every folder).
            let measured = timing::enabled().then(|| (fresh.len(), fresh.memory()));
            let mut index = self.index.write().unwrap_or_else(|e| e.into_inner());
            if self.generation.load(Ordering::SeqCst) == generation {
                let old = std::mem::replace(&mut *index, fresh);
                self.scanned.store(true, Ordering::SeqCst);
                drop(index);
                drop(old); // freed outside the lock
                if let Some((entries, m)) = measured {
                    record_memory(entries, m);
                }
                return;
            }
        }
    }

    /// Watch the roots recursively. Creations, removals and renames trigger
    /// `refresh` once changes have been quiet for 2 s (at most 10 s after the
    /// first change of a burst). If the watcher cannot be created the error
    /// is returned and the periodic refresh rescans every time (see [`periodic_due`]).
    pub fn watch(self: &Arc<Self>) -> notify::Result<()> {
        self.watch_using(os_drive_is_fixed)
    }

    /// `watch` with the drive-type lookup passed in (tests).
    fn watch_using(self: &Arc<Self>, fixed: impl Fn(char) -> bool) -> notify::Result<()> {
        let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
        let shared_roots = self.roots.clone();
        let state = self.watch_state.clone();
        // Not vouching for anything until every root is watched.
        state.watching.store(false, Ordering::SeqCst);
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            // Events inside skipped folders (AppData, caches, dot-folders)
            // never reach the rescan logic; whatever the watcher cannot vouch
            // for marks the index dirty. No disk access.
            if let Some(paths) = on_watch_result(&state, &shared_roots, res) {
                let _ = tx.send(paths);
            }
        })
        .inspect_err(|_| self.watch_state.dirty.store(true, Ordering::SeqCst))?;
        let mut all = true;
        for root in self.roots() {
            if let Err(e) = watcher.watch(&root, RecursiveMode::Recursive) {
                // A missing root is skipped by the scan too, but is not watched:
                // it appearing later would go unseen.
                all = false;
                if root.exists() {
                    self.watch_state.dirty.store(true, Ordering::SeqCst);
                    return Err(e);
                }
            }
        }
        self.watch_state.arm(&self.roots(), all, path_exists, fixed);
        let weak = Arc::downgrade(self);
        std::thread::spawn(move || {
            // Ends when the watcher (and with it `tx`) or the provider is dropped.
            while let Ok(paths) = rx.recv() {
                let Some(p) = weak.upgrade() else { return };
                let roots = p.roots();
                drop(p);
                if !change_is_relevant(&paths, &roots) {
                    continue;
                }
                let burst = wait_out_burst(
                    Instant::now(),
                    |left| rx.recv_timeout(left).map(|paths| change_is_relevant(&paths, &roots)),
                    Instant::now,
                );
                if !burst {
                    return;
                }
                // Too soon after the last scan, or one is running: wait,
                // swallowing further events (they collapse into the one rescan
                // at the end of the wait). Re-checked after each wait, so a scan
                // that started meanwhile pushes the rescan back too.
                loop {
                    let Some(p) = weak.upgrade() else { return };
                    let last = *p.last_scan.lock().unwrap_or_else(|e| e.into_inner());
                    drop(p);
                    let now = Instant::now();
                    let left = next_rescan_at(last_scan_so_far(last, now), now).saturating_duration_since(now);
                    if left.is_zero() {
                        break;
                    }
                    match rx.recv_timeout(left) {
                        Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                let Some(p) = weak.upgrade() else { return };
                p.refresh();
            }
        });
        *self.watcher.lock().unwrap_or_else(|e| e.into_inner()) = Some(watcher);
        Ok(())
    }
}

/// Records the size of a freshly built index (the caller checks that timing
/// is on). The `index.*` marks carry a count or kilobytes in the log's
/// `micros` field, not a time.
fn record_memory(entries: usize, m: Memory) {
    timing::mark("index.entries", entries as u64);
    timing::mark("index.used_kb", (m.used / 1024) as u64);
    timing::mark("index.reserved_kb", (m.reserved / 1024) as u64);
    timing::mark("index.norms_kb", (m.norms / 1024) as u64);
}

/// The icon key of a file: `ext:.pdf`, or `builtin:file` when it has none.
pub(crate) fn file_icon(name: &str, is_dir: bool) -> String {
    if is_dir {
        return "folder".into();
    }
    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some(ext) if !ext.is_empty() => format!("ext:.{}", ext.to_lowercase()),
        _ => "builtin:file".into(),
    }
}

impl FilesProvider {
    /// Read lock only, no disk: every entry is checked against cheap char
    /// masks first, `score` ([`file_score`]) runs only on candidates that can
    /// still make the best 20, and result strings are built for those 20 alone.
    /// `None` when `gate` says a newer search has begun: checked every 4096
    /// slots, and the scan stops there. Otherwise the same rows, in the same
    /// order, as without a gate.
    fn search(
        &self,
        q: &Query,
        score: impl Fn(&str, usize, &str, bool, u8) -> Option<u32>,
        gate: Option<&crate::engine::QueryGate>,
    ) -> Option<Vec<ResultItem>> {
        let text = q.text.trim();
        if text.chars().count() < MIN_QUERY_CHARS {
            return Some(Vec::new());
        }
        let needle = fuzzy::normalize(text);
        if needle.is_empty() {
            return Some(Vec::new());
        }
        let len = needle.chars().count();
        // Every tier needs each non-break char of the needle somewhere in the name.
        let need = needle.chars().filter(|&c| !fuzzy::is_word_break(c)).fold(0, |m, c| m | char_bit(c));
        // Every tier but substring needs a word of the name to start with the
        // needle's first char (when that char is not itself a word break).
        let first = needle.chars().next().filter(|&c| !fuzzy::is_word_break(c)).map(char_bit);

        let index = self.index.read().unwrap_or_else(|e| e.into_inner());
        // Min-heap of (score, Reverse(position)): the top is the entry to drop
        // next (lowest score, then latest), so equal scores keep index order.
        let mut best: BinaryHeap<Reverse<(u32, Reverse<usize>)>> = BinaryHeap::with_capacity(MAX_RESULTS + 1);
        for (i, s) in index.slots.iter().enumerate() {
            if i % CANCEL_CHECK_SLOTS == 0 && gate.is_some_and(|g| g.superseded()) {
                return None;
            }
            if s.chars & need != need {
                continue;
            }
            let substring_only = first.is_some_and(|b| s.initials & b == 0);
            if substring_only && len < 4 {
                continue;
            }
            let depth = index.depth(s);
            if best.len() == MAX_RESULTS {
                let tier = if substring_only { SUBSTRING } else { EXACT };
                let file = if s.is_dir { 0 } else { FILE_BELOW_FOLDER };
                let Some(Reverse((worst, _))) = best.peek() else { continue };
                if tier - file - depth_penalty(depth) <= *worst {
                    continue;
                }
            }
            let hay = index.norm(s);
            if substring_only && !hay.contains(needle.as_str()) {
                continue;
            }
            let Some(score) = score(&needle, len, hay, s.is_dir, depth) else { continue };
            best.push(Reverse((score, Reverse(i))));
            if best.len() > MAX_RESULTS {
                best.pop();
            }
        }
        let mut picked: Vec<(u32, usize)> = best.into_iter().map(|Reverse((score, Reverse(i)))| (score, i)).collect();
        picked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        Some(picked.into_iter().map(|(score, i)| self.row(&index, &index.slots[i], score)).collect())
    }


    fn row(&self, index: &Index, s: &Slot, score: u32) -> ResultItem {
        ResultItem {
            id: index.path(s).to_string_lossy().into_owned(),
            folder: if s.is_dir {
                let mut f = index.path(s).to_string_lossy().into_owned();
                if !f.ends_with('\\') {
                    f.push('\\');
                }
                f
            } else {
                String::new()
            },
            provider: "files",
            title: index.name(s).to_string(),
            subtitle: shorten_home(index.dir(s), self.home.as_deref()),
            score,
            icon: file_icon(index.name(s), s.is_dir),
            pinnable: true,
            actions: if s.is_dir {
                vec![Action::OpenFolder, Action::CopyPath]
            } else {
                vec![Action::Reveal, Action::CopyPath]
            },
            ..Default::default()
        }
    }
}

impl Provider for FilesProvider {
    fn name(&self) -> &'static str {
        "files"
    }

    /// Read lock only, no disk: see [`FilesProvider::search`]. A search that a
    /// newer one has superseded returns nothing (the engine then drops the
    /// whole answer; see [`crate::engine::Engine::query_newest`]).
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        self.search(q, file_score, crate::engine::current_gate().as_ref()).unwrap_or_default()
    }

    fn item(&self, id: &str) -> Option<ResultItem> {
        let index = self.index.read().unwrap_or_else(|e| e.into_inner());
        let s = index.find(Path::new(id))?;
        Some(self.row(&index, s, 0))
    }

    fn ready(&self) -> bool {
        self.scanned.load(Ordering::SeqCst)
    }

    /// Only for indexed paths; the id is the full path.
    fn act(&self, id: &str, action: Action) -> Result<Outcome, String> {
        let known = self.index.read().unwrap_or_else(|e| e.into_inner()).find(Path::new(id)).is_some();
        if !known {
            return Err(NOT_FOUND.into());
        }
        match action {
            Action::Reveal => self.launcher.reveal(id)?,
            Action::OpenFolder => self.launcher.open(id)?,
            Action::CopyPath => self.clipboard.set_text(id)?,
            _ => return Err("can't do that here".into()),
        }
        Ok(Outcome::Hide)
    }

    fn activate(&self, id: &str, modifier: Modifier) -> Result<Outcome, String> {
        let path = Path::new(id);
        let known = self.index.read().unwrap_or_else(|e| e.into_inner()).find(path).is_some();
        // Checking the disk is fine here: activate is not the query path.
        if !known || !path.exists() {
            return Err(NOT_FOUND.into());
        }
        match modifier {
            Modifier::None | Modifier::CtrlShift => self.launcher.open(id)?,
            Modifier::Ctrl => self.launcher.reveal(id)?,
        }
        Ok(Outcome::Hide)
    }

    fn rank_key(&self, id: &str) -> Option<String> {
        Some(id.to_string())
    }

    /// File names are many and noisy: they never crowd apps, shortcuts and
    /// Settings pages out of the list.
    fn plain_share(&self) -> usize {
        PLAIN_SHARE
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: "report.pdf".into(),
            about: "find a file or folder by name; Ctrl+Enter shows it in Explorer".into(),
            fill: String::new(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuzzy;
    use crate::clipboard::FakeClipboard;
    use crate::launch::testing::RecordingLauncher;
    use crate::provider::Action;
    use std::fs;
    use std::path::Path;

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    fn entry(path: &str, is_dir: bool) -> FileEntry {
        FileEntry::new(PathBuf::from(path), is_dir)
    }

    fn provider(entries: Vec<FileEntry>) -> (Arc<FilesProvider>, Arc<RecordingLauncher>) {
        let rec = Arc::new(RecordingLauncher::default());
        (FilesProvider::with_entries(rec.clone(), entries), rec)
    }

    fn names(entries: &[FileEntry]) -> Vec<String> {
        let mut out: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
        out.sort();
        out
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"").unwrap();
    }

    fn set_attributes(path: &Path, attrs: u32) {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_FLAGS_AND_ATTRIBUTES};
        unsafe { SetFileAttributesW(&HSTRING::from(path.as_os_str()), FILE_FLAGS_AND_ATTRIBUTES(attrs)) }.unwrap();
    }

    #[test]
    fn scan_finds_files_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("report.pdf"));
        touch(&dir.path().join("Projects").join("flick").join("notes.txt"));
        let out = scan(&[dir.path().to_path_buf()]);
        assert_eq!(names(&out), ["Projects", "flick", "notes.txt", "report.pdf"]);
        let notes = out.iter().find(|e| e.name == "notes.txt").unwrap();
        assert_eq!(notes.path, dir.path().join("Projects").join("flick").join("notes.txt"));
        assert!(!notes.is_dir);
        let projects = out.iter().find(|e| e.name == "Projects").unwrap();
        assert!(projects.is_dir);
        assert_eq!(projects.name_norm, "projects");
        // Depth counts levels below the root: Projects 1, flick 2, notes.txt 3.
        assert_eq!((projects.depth, notes.depth), (1, 3));
    }

    #[test]
    fn scan_skips_hidden_dot_and_junk_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("keep.txt"));
        touch(&root.join(".gitconfig"));
        touch(&root.join(".git").join("HEAD"));
        for junk in [
            "node_modules",
            "target",
            "dist",
            "build",
            "__pycache__",
            "venv",
            ".venv",
            "AppData",
            "$RECYCLE.BIN",
            "vendor",
            "site-packages",
            "Pods",
            "bower_components",
            "obj",
            "miniconda3",
            "Anaconda3",
            "miniforge3",
            "mambaforge",
        ] {
            touch(&root.join(junk).join("inside.txt"));
        }
        // Junk names only skip folders; a file called "build" stays.
        touch(&root.join("src").join("build"));
        touch(&root.join("hidden.txt"));
        set_attributes(&root.join("hidden.txt"), 0x2);
        touch(&root.join("system.txt"));
        set_attributes(&root.join("system.txt"), 0x4);
        touch(&root.join("Secret").join("inside.txt"));
        set_attributes(&root.join("Secret"), 0x2);

        let out = scan(&[root.to_path_buf()]);
        assert_eq!(names(&out), ["build", "keep.txt", "src"]);
    }

    #[test]
    fn hidden_and_system_attribute_bits_are_skipped() {
        assert!(skip_by_attributes(0x2));
        assert!(skip_by_attributes(0x4));
        assert!(skip_by_attributes(0x2 | 0x10));
        assert!(!skip_by_attributes(0x10));
        assert!(!skip_by_attributes(0x20));
        assert!(!skip_by_attributes(0));
    }

    #[test]
    fn scan_stops_at_max_depth() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("a").join("b").join("c").join("deep.txt"));
        touch(&dir.path().join("a").join("shallow.txt"));
        let out = scan_with_limits(&[dir.path().to_path_buf()], MAX_ENTRIES, 2, MAX_PER_FOLDER);
        assert_eq!(names(&out), ["a", "b", "shallow.txt"]);
    }

    #[test]
    fn scan_survives_missing_root() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("found.txt"));
        let out = scan(&[PathBuf::from(r"C:\definitely\missing\root"), dir.path().to_path_buf()]);
        assert_eq!(names(&out), ["found.txt"]);
    }

    #[test]
    fn scan_caps_total_entries_shallow_first() {
        let dir = tempfile::tempdir().unwrap();
        // Two roots, each with one shallow file and one deep folder.
        let (a, b) = (dir.path().join("rootA"), dir.path().join("rootB"));
        touch(&a.join("a1.txt"));
        touch(&a.join("adeep").join("a2.txt"));
        touch(&b.join("b1.txt"));
        touch(&b.join("bdeep").join("b2.txt"));
        let out = scan_with_limits(&[a, b], 4, MAX_DEPTH, MAX_PER_FOLDER);
        assert_eq!(out.len(), 4);
        // Each root gets half the cap, filled breadth-first: depth 1 before depth 2.
        assert_eq!(names(&out), ["a1.txt", "adeep", "b1.txt", "bdeep"]);
    }

    #[test]
    fn accented_file_found_by_ascii_query() {
        let (p, _) = provider(vec![entry(r"C:\Users\x\Documents\Café.pdf", false)]);
        let out = p.query(&q("cafe"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Café.pdf");
        assert_eq!(out[0].id, r"C:\Users\x\Documents\Café.pdf");
        assert_eq!(out[0].provider, "files");
    }

    #[test]
    fn short_query_returns_nothing() {
        let (p, _) = provider(vec![entry(r"C:\d\a.txt", false), entry(r"C:\d\ab.txt", false)]);
        assert!(p.query(&q("")).is_empty());
        assert!(p.query(&q("a")).is_empty());
        assert!(p.query(&q("  a  ")).is_empty());
        assert_eq!(p.query(&q("ab")).len(), 1);
    }

    const SHORT_NAMES: [&str; 36] = [
        "a", "a.txt", "A", "ab", "ab.txt", "fi", "fi.txt", "fi.tar.gz", "file", "File.txt", "files", "final.pdf",
        "re", "re.md", "report.pdf", "Reports", "Annual Report.docx", "ca", "ca.exe", "Calculator", "calc.exe",
        "ti", "tidy", "ti-84", "My Fi", "fi fi", ".fi", ".a", "a.", "ca.", "cache", "x-ca", "Café", "résumé.doc",
        "re.re", "fi.re.ca",
    ];

    #[test]
    fn short_needles_score_as_before_without_the_fuzzy_ladder() {
        for needle in ["a", "fi", "re", "ca", "ti", "ab", "file", "report", "cal", "calc", "fi fi", "annual rep", ".fi"] {
            let needle = fuzzy::normalize(needle);
            let len = needle.chars().count();
            for name in SHORT_NAMES {
                let hay = fuzzy::normalize(name);
                for is_dir in [false, true] {
                    for depth in [1, 2, 6, 7, 9] {
                        assert_eq!(
                            file_score(&needle, len, &hay, is_dir, depth),
                            file_score_reference(&needle, len, &hay, is_dir, depth),
                            "{needle:?} vs {name:?} dir={is_dir} depth={depth}"
                        );
                    }
                }
            }
        }
        assert!(SHORT_NAMES.len() >= 30);
    }

    /// An index with enough variety for short and long needles.
    fn varied_provider(count: usize) -> Arc<FilesProvider> {
        let words = ["fi", "re", "ca", "ti", "file", "report", "Calc", "tidy", "Café", "notes", "a", "final copy"];
        let entries = (0..count)
            .map(|i| {
                let w = words[i % words.len()];
                let name = match i % 5 {
                    0 => w.to_string(),
                    1 => format!("{w}.txt"),
                    2 => format!("{w} {}.pdf", i % 7),
                    3 => format!("x{w}y"),
                    _ => format!("{w}-{w}"),
                };
                entry(&format!(r"C:\d{}\e{}\{name}", i % 9, i % 4), i % 6 == 0)
            })
            .collect();
        provider(entries).0
    }

    #[test]
    fn a_search_gives_the_rows_and_order_it_gave_before() {
        let p = varied_provider(9000);
        let newest = Arc::new(AtomicU64::new(0));
        let gate = crate::engine::QueryGate::begin_for_test(&newest, 1);
        let ids = |rows: &[ResultItem]| rows.iter().map(|r| (r.id.clone(), r.score)).collect::<Vec<_>>();
        for text in ["fi", "re", "ca", "ti", "a", "file", "report", "calc", "tid", "final co", "caf", "zz"] {
            let old = p.search(&q(text), file_score_reference, None).unwrap();
            let new = p.search(&q(text), file_score, None).unwrap();
            let gated = p.search(&q(text), file_score, Some(&gate)).unwrap();
            assert_eq!(ids(&new), ids(&old), "{text}");
            assert_eq!(ids(&gated), ids(&old), "{text}");
            assert_eq!(ids(&p.query(&q(text))), ids(&old), "{text}");
        }
        assert!(!p.search(&q("fi"), file_score, None).unwrap().is_empty());
        assert!(!p.search(&q("file"), file_score, None).unwrap().is_empty());
    }

    #[test]
    fn a_superseded_search_stops_and_says_so() {
        let p = varied_provider(9000);
        let newest = Arc::new(AtomicU64::new(0));
        let gate = crate::engine::QueryGate::begin_for_test(&newest, 1);
        assert!(p.search(&q("file"), file_score, Some(&gate)).is_some());
        // A newer search begins: this one gives up, and that is not an empty answer.
        let _newer = crate::engine::QueryGate::begin_for_test(&newest, 2);
        assert!(p.search(&q("file"), file_score, Some(&gate)).is_none());
        // A text too short to search is an answer, not a cancellation.
        assert_eq!(p.search(&q("a"), file_score, Some(&gate)).map(|r| r.len()), Some(0));
    }

    /// `file_score` for raw text, as the query path calls it.
    fn fs(needle: &str, name: &str, is_dir: bool, depth: u8) -> Option<u32> {
        let needle = fuzzy::normalize(needle);
        file_score(&needle, needle.chars().count(), &fuzzy::normalize(name), is_dir, depth)
    }

    #[test]
    fn file_scores_use_strict_tiers_and_depth() {
        assert_eq!(fs("calc", "Calculator", true, 5), Some(540));
        assert_eq!(fs("calc", "calculator.py", false, 1), Some(590));
        // No acronym, initials or typo tiers for files.
        assert_eq!(fs("tpr", "Third Party Report.pdf", false, 1), None);
        assert_eq!(fs("tdo", "to-do output.txt", false, 1), None);
        assert_eq!(fs("blnder", "Blender.blend", false, 1), None);
        // Two chars: an exact name only.
        assert_eq!(fs("bl", "Blender", true, 1), None);
        assert_eq!(fs("db", "db", true, 1), Some(640));
        assert_eq!(fs("db", "db.sqlite", false, 1), Some(630));
        // Exact, exact stem, prefix, word prefix, substring.
        assert_eq!(fs("report", "report.pdf", false, 1), Some(630));
        assert_eq!(fs("report", "Reports", true, 1), Some(600));
        assert_eq!(fs("report", "Annual Report.docx", false, 1), Some(550));
        assert_eq!(fs("report", "preport.txt", false, 1), Some(510));
        assert_eq!(fs("wifi", "Wi-Fi notes.txt", false, 1), Some(570));
        // Substring needs four chars.
        assert_eq!(fs("rep", "myrepo", true, 1), None);
        // Every typed word a word prefix; typed in name order it is a plain prefix.
        assert_eq!(fs("rep annual", "Annual Report.docx", false, 1), Some(550));
        assert_eq!(fs("annual rep", "Annual Report.docx", false, 1), Some(590));
        // A leading dot is not a stem: `env` is a squashed prefix of `.env`, not its exact name.
        assert_eq!(fs("env", ".env", false, 1), Some(570));
        // Depth: 0, 15, 30, 45, then 60 for anything deeper.
        let by_depth: Vec<_> = (1..=7).map(|d| fs("db", "db", true, d)).collect();
        assert_eq!(by_depth, [640, 625, 610, 595, 580, 580, 580].map(Some));
    }

    #[test]
    fn no_file_ever_reaches_the_app_name_band() {
        let names = ["calc", "calc.exe", "Calculator", "calc tools", "x-calc", "c.a.l.c", "CALC (1).txt"];
        for needle in ["calc", "ca", "calc tools", "c a l c", "calc.exe"] {
            for name in names {
                for is_dir in [false, true] {
                    if let Some(score) = fs(needle, name, is_dir, 1) {
                        assert!(score < 650, "{needle:?} vs {name:?}: {score}");
                    }
                }
            }
        }
    }

    #[test]
    fn shallow_entry_ranks_above_the_same_name_deeper() {
        let (p, _) = provider(vec![
            entry(r"C:\r\a\b\c\notes.txt", false).at_depth(4),
            entry(r"C:\r\notes.txt", false),
        ]);
        let out = p.query(&q("notes"));
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].id.as_str(), out[0].score), (r"C:\r\notes.txt", 630));
        assert_eq!((out[1].id.as_str(), out[1].score), (r"C:\r\a\b\c\notes.txt", 585));
    }

    #[test]
    fn folders_rank_above_files() {
        let (p, _) = provider(vec![entry(r"C:\d\taxes.txt", false), entry(r"C:\e\taxes", true)]);
        let out = p.query(&q("taxes"));
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].id.as_str(), out[0].score), (r"C:\e\taxes", 640));
        assert_eq!(out[1].score, 630);
    }

    #[test]
    fn deep_partial_matches_do_not_fill_the_list() {
        let (p, _) = provider(vec![
            entry(r"C:\r\sdk\dev\tests\gallery\demo\calculator", true).at_depth(7),
            entry(r"C:\r\sdk\dev\tests\gallery\demo\calculator\calculator_demo.dart", false).at_depth(8),
            entry(r"C:\r\sdk\engine\src\shell\common\cache_calculator.cc", false).at_depth(7),
            entry(r"C:\r\Project\app\src\answers\calc.rs", false).at_depth(5),
            entry(r"C:\r\Project\app\src\answers\calc_tools", true).at_depth(6),
            entry(r"C:\r\Exercises\Calculator", true).at_depth(2),
        ]);
        let ids = |text: &str| p.query(&q(text)).into_iter().map(|r| r.id).collect::<Vec<_>>();
        // Partial matches six levels down or less still show; deeper ones do not.
        assert_eq!(
            ids("calc"),
            [r"C:\r\Exercises\Calculator", r"C:\r\Project\app\src\answers\calc.rs", r"C:\r\Project\app\src\answers\calc_tools"]
        );
        // The exact name is still found at any depth.
        assert_eq!(ids("calculator"), [r"C:\r\Exercises\Calculator", r"C:\r\sdk\dev\tests\gallery\demo\calculator"]);
        assert_eq!(ids("calculator_demo"), [r"C:\r\sdk\dev\tests\gallery\demo\calculator\calculator_demo.dart"]);
        assert_eq!(fs("calc", "Calculator", true, 6), Some(540));
        assert_eq!(fs("calc", "Calculator", true, 7), None);
        assert_eq!(fs("calculator", "Calculator", true, 9), Some(580));
    }

    #[test]
    fn returns_at_most_20() {
        let entries = (0..50).map(|i| entry(&format!(r"C:\d\report {i}.pdf"), false)).collect();
        let (p, _) = provider(entries);
        let out = p.query(&q("report"));
        assert_eq!(out.len(), 20);
        // Equal scores keep index order (shallow first after a scan).
        assert_eq!(out[0].title, "report 0.pdf");
        assert_eq!(out[19].title, "report 19.pdf");
    }

    #[test]
    fn best_matches_survive_the_cap() {
        let mut entries: Vec<FileEntry> =
            (0..40).map(|i| entry(&format!(r"C:\d\old report {i}.pdf"), false)).collect();
        entries.push(entry(r"C:\d\report.pdf", false));
        let (p, _) = provider(entries);
        let out = p.query(&q("report"));
        assert_eq!(out.len(), 20);
        assert_eq!(out[0].title, "report.pdf");
        assert!(out.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn substring_inside_a_word_still_matches_when_the_list_is_full() {
        // "epor" starts no word of "report", so only the substring tier can match it.
        let mut entries: Vec<FileEntry> = (0..30).map(|i| entry(&format!(r"C:\d\epor {i}.txt"), false)).collect();
        entries.push(entry(r"C:\d\report.pdf", true));
        let (p, _) = provider(entries);
        let out = p.query(&q("epor"));
        assert_eq!(out.len(), 20);
        // File prefix 590; the folder's substring 520 loses.
        assert!(out.iter().all(|r| r.score == 590));
        let (p, _) = provider(vec![entry(r"C:\d\report.pdf", true)]);
        assert_eq!(p.query(&q("epor"))[0].score, 520);
    }

    #[test]
    fn watcher_rescans_after_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let (p, _) = provider(vec![]);
        p.set_roots(vec![dir.path().to_path_buf()]);
        p.watch().unwrap();
        touch(&dir.path().join("watched report.pdf"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while p.query(&q("watched")).is_empty() {
            assert!(std::time::Instant::now() < deadline, "watcher did not rescan");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(p.query(&q("watched"))[0].title, "watched report.pdf");
    }

    #[test]
    fn subtitle_is_parent_folder_with_home_shortened() {
        let home = Path::new(r"C:\Users\x");
        assert_eq!(shorten_home(Path::new(r"C:\Users\x\Documents\Tax"), Some(home)), r"~\Documents\Tax");
        assert_eq!(shorten_home(Path::new(r"C:\Users\x"), Some(home)), "~");
        assert_eq!(shorten_home(Path::new(r"D:\Stuff"), Some(home)), r"D:\Stuff");
        assert_eq!(shorten_home(Path::new(r"D:\Stuff"), None), r"D:\Stuff");
        // Roots written with `/` in the config still show `\`.
        assert_eq!(shorten_home(Path::new(r"C:\Users\x\Documents/Demo\Trip"), Some(home)), r"~\Documents\Demo\Trip");
        assert_eq!(shorten_home(Path::new("D:/Stuff/Old"), None), r"D:\Stuff\Old");

        let (p, _) = provider(vec![entry(r"D:\Stuff\taxes.txt", false)]);
        assert_eq!(p.query(&q("taxes"))[0].subtitle, r"D:\Stuff");
    }

    #[test]
    fn item_only_for_indexed_paths() {
        let (p, _) = provider(vec![entry(r"C:\x\Report.PDF", false), entry(r"C:\x\docs", true)]);
        let f = p.item(r"C:\x\Report.PDF").unwrap();
        assert_eq!((f.icon.as_str(), f.title.as_str(), f.score), ("ext:.pdf", "Report.PDF", 0));
        assert!(f.pinnable);
        assert_eq!(p.item(r"C:\x\docs").unwrap().icon, "folder");
        let (p2, _) = provider(vec![entry(r"C:\x\Makefile", false)]);
        assert_eq!(p2.item(r"C:\x\Makefile").unwrap().icon, "builtin:file");
        assert!(p.item(r"C:\x\other.txt").is_none());
        assert!(p.ready());

        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("a.txt"));
        let p = FilesProvider::new(Arc::new(RecordingLauncher::default()), Arc::new(FakeClipboard::default()));
        assert!(!p.ready());
        p.set_roots(vec![dir.path().to_path_buf()]);
        p.refresh_blocking();
        assert!(p.ready());
    }

    #[test]
    fn reveal_uses_the_launcher() {
        let (p, rec) = provider(vec![entry(r"C:\x\a.txt", false), entry(r"C:\x\docs", true)]);
        assert_eq!(p.act(r"C:\x\a.txt", Action::Reveal), Ok(Outcome::Hide));
        assert_eq!(p.act(r"C:\x\docs", Action::OpenFolder), Ok(Outcome::Hide));
        assert_eq!(rec.calls(), [r"reveal C:\x\a.txt".to_string(), r"open C:\x\docs".to_string()]);
        assert!(p.act(r"C:\x\nope.txt", Action::Reveal).is_err());
        assert_eq!(p.act(r"C:\x\a.txt", Action::Admin), Err("can't do that here".to_string()));
        assert_eq!(rec.calls().len(), 2);
    }

    #[test]
    fn rows_offer_location_or_explorer_and_copy() {
        let (p, _) = provider(vec![entry(r"C:\x\notes.txt", false), entry(r"C:\x\notes", true)]);
        assert_eq!(p.item(r"C:\x\notes.txt").unwrap().actions, [Action::Reveal, Action::CopyPath]);
        assert_eq!(p.item(r"C:\x\notes").unwrap().actions, [Action::OpenFolder, Action::CopyPath]);
        let rows = p.query(&q("notes"));
        assert_eq!(rows.len(), 2);
        for r in rows {
            let first = if r.icon == "folder" { Action::OpenFolder } else { Action::Reveal };
            assert_eq!(r.actions, [first, Action::CopyPath], "{}", r.id);
            assert_eq!(r.complete, "");
        }
    }

    #[test]
    fn a_folder_hit_says_it_can_be_unfolded() {
        let (p, _) = provider(vec![entry(r"C:\x\notes.txt", false), entry(r"C:\x\notes", true)]);
        let rows = p.query(&q("notes"));
        let folder = rows.iter().find(|r| r.id == r"C:\x\notes").unwrap();
        let file = rows.iter().find(|r| r.id == r"C:\x\notes.txt").unwrap();
        assert_eq!(folder.folder, r"C:\x\notes\");
        assert_eq!(file.folder, "");
        assert_eq!(p.item(r"C:\x\notes").unwrap().folder, r"C:\x\notes\");
        assert_eq!(p.item(r"C:\x\notes.txt").unwrap().folder, "");
    }

    fn with_fake_clipboard(entries: Vec<FileEntry>) -> (Arc<FilesProvider>, Arc<RecordingLauncher>, Arc<FakeClipboard>) {
        let rec = Arc::new(RecordingLauncher::default());
        let clip = Arc::new(FakeClipboard::default());
        (FilesProvider::with_clipboard(rec.clone(), clip.clone(), entries), rec, clip)
    }

    #[test]
    fn copy_path_copies_the_full_path_and_hides() {
        let (p, rec, clip) = with_fake_clipboard(vec![entry(r"C:\d\a.txt", false)]);
        assert_eq!(p.act(r"C:\d\a.txt", Action::CopyPath), Ok(Outcome::Hide));
        assert_eq!(*clip.0.lock().unwrap(), [r"C:\d\a.txt".to_string()]);
        assert!(rec.calls().is_empty());
    }

    #[test]
    fn act_refuses_unindexed_paths() {
        let (p, rec, clip) = with_fake_clipboard(vec![entry(r"C:\d\a.txt", false)]);
        assert_eq!(p.act(r"C:\nope.txt", Action::CopyPath), Err("file not found".to_string()));
        assert_eq!(p.act(r"C:\nope", Action::OpenFolder), Err("file not found".to_string()));
        assert!(clip.0.lock().unwrap().is_empty());
        assert!(rec.calls().is_empty());
    }

    #[test]
    fn enter_opens_ctrl_enter_reveals() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.pdf");
        touch(&file);
        let id = file.to_str().unwrap().to_string();
        let (p, rec) = provider(vec![FileEntry::new(file.clone(), false)]);
        assert_eq!(p.activate(&id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(p.activate(&id, Modifier::CtrlShift), Ok(Outcome::Hide));
        assert_eq!(p.activate(&id, Modifier::Ctrl), Ok(Outcome::Hide));
        assert_eq!(rec.calls(), [format!("open {id}"), format!("open {id}"), format!("reveal {id}")]);
        assert_eq!(p.rank_key(&id), Some(id.clone()));
    }

    #[test]
    fn missing_file_is_an_error() {
        let (p, rec) = provider(vec![entry(r"C:\definitely\missing\gone.pdf", false)]);
        assert_eq!(p.activate(r"C:\definitely\missing\gone.pdf", Modifier::None), Err("file not found".into()));
        // Not in the index at all: same answer, nothing started.
        assert_eq!(p.activate(r"C:\Windows\notepad.exe", Modifier::None), Err("file not found".into()));
        assert!(rec.calls().is_empty());
    }

    #[test]
    fn set_roots_empty_uses_defaults() {
        let (p, _) = provider(vec![]);
        p.set_roots(vec![PathBuf::from(r"C:\x")]);
        assert_eq!(p.roots(), [PathBuf::from(r"C:\x")]);
        p.set_roots(vec![]);
        assert_eq!(p.roots(), default_roots());
    }

    #[test]
    fn default_roots_are_the_user_folder() {
        assert_eq!(roots_for_profile(Some(OsStr::new(r"C:\Users\x"))), vec![PathBuf::from(r"C:\Users\x")]);
        assert!(roots_for_profile(None).is_empty());
        // `default_roots` is that function applied to the environment.
        assert_eq!(default_roots(), roots_for_profile(std::env::var_os("USERPROFILE").as_deref()));
    }

    #[test]
    fn events_in_skipped_folders_are_ignored() {
        let roots = [PathBuf::from(r"C:\Users\x")];
        let skipped = |p: &str| path_is_skipped(Path::new(p), &roots);
        assert!(skipped(r"C:\Users\x\AppData\Local\Temp\a.tmp"));
        assert!(skipped(r"C:\Users\x\appdata\Roaming\b"));
        assert!(skipped(r"C:\Users\x\.cache\x"));
        assert!(skipped(r"C:\Users\x\code\node_modules\y"));
        assert!(!skipped(r"C:\Users\x\Pictures\cat.jpg"));
        assert!(!skipped(r"C:\Users\x\Documents\notes.txt"));
        // Outside every root: unknown, not skipped.
        assert!(!skipped(r"D:\elsewhere\file.txt"));
    }

    #[test]
    fn depth_penalty_counts_from_profile_subfolders() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path();
        touch(&profile.join("Documents").join("a.pdf"));
        touch(&profile.join("Documents").join("sub").join("b.pdf"));
        touch(&profile.join("top.pdf"));
        let roots = [profile.to_path_buf()];
        let ranked = scan_index(&roots, Some(profile), MAX_ENTRIES, MAX_DEPTH, MAX_PER_FOLDER, Sizes::default()).entries();
        let depth = |n: &str| ranked.iter().find(|e| e.name == n).unwrap().depth;
        // As when Documents itself was the root: a.pdf 1, sub 1, b.pdf 2.
        assert_eq!((depth("a.pdf"), depth("sub"), depth("b.pdf")), (1, 1, 2));
        // Directly in the profile: no deeper than that.
        assert!(depth("top.pdf") <= depth("a.pdf"));
        // A configured root that is not the profile keeps counting from itself.
        let plain = scan_index(&roots, None, MAX_ENTRIES, MAX_DEPTH, MAX_PER_FOLDER, Sizes::default()).entries();
        assert_eq!(plain.iter().find(|e| e.name == "a.pdf").unwrap().depth, 2);
    }

    #[test]
    fn refresh_swaps_in_a_new_scan() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("fresh report.pdf"));
        let (p, _) = provider(vec![entry(r"C:\d\old report.pdf", false)]);
        p.set_roots(vec![dir.path().to_path_buf()]);
        // Stale until refreshed: the old index keeps answering.
        assert_eq!(p.query(&q("report"))[0].title, "old report.pdf");
        p.refresh_blocking();
        let out = p.query(&q("report"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "fresh report.pdf");
    }

    #[test]
    fn stale_scan_does_not_overwrite_newer_roots() {
        let (p, _) = provider(vec![]);
        p.set_roots(vec![PathBuf::from(r"C:\old")]);
        let p2 = p.clone();
        let mut scanned: Vec<Vec<PathBuf>> = Vec::new();
        p.refresh_with(|roots| {
            scanned.push(roots.to_vec());
            if scanned.len() == 1 {
                // Roots change while the first scan runs.
                p2.set_roots(vec![PathBuf::from(r"C:\new")]);
            }
            let name = if roots[0] == Path::new(r"C:\old") { "old hit.txt" } else { "new hit.txt" };
            vec![FileEntry::new(roots[0].join(name), false)]
        });
        assert_eq!(scanned, [vec![PathBuf::from(r"C:\old")], vec![PathBuf::from(r"C:\new")]]);
        let out = p.query(&q("hit"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "new hit.txt");
    }

    #[test]
    fn each_root_gets_an_equal_share() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("big"), dir.path().join("small"));
        for i in 0..10 {
            touch(&a.join(format!("a{i:02}.txt")));
        }
        for i in 0..3 {
            touch(&b.join(format!("b{i}.txt")));
        }
        // Share 8 / 2 = 4 each; "small" uses 3 and does not donate the rest.
        let out = scan_with_limits(&[a.clone(), b], 8, MAX_DEPTH, MAX_PER_FOLDER);
        assert_eq!(out.iter().filter(|e| e.path.starts_with(&a)).count(), 4);
        assert_eq!(out.iter().filter(|e| e.name.starts_with('b')).count(), 3);
        assert_eq!(out.len(), 7);
    }

    #[test]
    fn one_folder_contributes_at_most_the_per_folder_cap() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..30 {
            touch(&dir.path().join("huge").join(format!("f{i:02}.txt")));
        }
        let out = scan_with_limits(&[dir.path().to_path_buf()], MAX_ENTRIES, MAX_DEPTH, 10);
        assert_eq!(out.iter().filter(|e| e.name.starts_with('f')).count(), 10);
        assert_eq!(out.len(), 11); // plus "huge" itself

        // Entries past the cap are skipped, but a folder read before it is still walked.
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("a").join("x.txt"));
        touch(&dir.path().join("b.txt"));
        touch(&dir.path().join("c.txt"));
        let out = scan_with_limits(&[dir.path().to_path_buf()], MAX_ENTRIES, MAX_DEPTH, 2);
        assert_eq!(names(&out), ["a", "b.txt", "x.txt"]);
    }

    #[test]
    fn only_name_changes_trigger_a_rescan() {
        use notify::event::{AccessKind, CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind, RenameMode};
        assert!(event_triggers_rescan(&EventKind::Create(CreateKind::Any), false));
        assert!(event_triggers_rescan(&EventKind::Create(CreateKind::Folder), false));
        assert!(event_triggers_rescan(&EventKind::Remove(RemoveKind::Any), false));
        assert!(event_triggers_rescan(&EventKind::Modify(ModifyKind::Name(RenameMode::From)), false));
        assert!(event_triggers_rescan(&EventKind::Modify(ModifyKind::Name(RenameMode::To)), false));
        assert!(event_triggers_rescan(&EventKind::Other, false));
        // Content writes (Windows reports them as Modify(Any)), metadata and access do not.
        assert!(!event_triggers_rescan(&EventKind::Modify(ModifyKind::Any), false));
        assert!(!event_triggers_rescan(&EventKind::Modify(ModifyKind::Data(DataChange::Content)), false));
        assert!(!event_triggers_rescan(&EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)), false));
        assert!(!event_triggers_rescan(&EventKind::Access(AccessKind::Any), false));
        // The backend lost events: rescan whatever the kind.
        assert!(event_triggers_rescan(&EventKind::Modify(ModifyKind::Any), true));
        assert!(event_triggers_rescan(&EventKind::Any, true));
    }

    #[test]
    fn rescan_waits_for_quiet_but_not_forever() {
        use std::time::{Duration, Instant};
        let first = Instant::now();
        let s = Duration::from_secs;
        // 2 s after the last event...
        assert_eq!(rescan_deadline(first, first), first + s(2));
        assert_eq!(rescan_deadline(first, first + s(5)), first + s(7));
        // ...but never later than 10 s after the first one of the burst.
        assert_eq!(rescan_deadline(first, first + s(9)), first + s(10));
        assert_eq!(rescan_deadline(first, first + s(30)), first + s(10));
    }

    /// `wait_out_burst` against a fake clock and a fake event stream: `next`
    /// answers each wait and says how far the clock moved meanwhile. Returns
    /// the result, the fake time it returned at (from `first`) and the waits asked
    /// for. Gives up (as a closed channel) after 100 waits.
    fn burst_with(
        mut next: impl FnMut(Duration) -> (Duration, Result<bool, RecvTimeoutError>),
    ) -> (bool, Duration, Vec<Duration>) {
        use std::cell::{Cell, RefCell};
        let first = Instant::now();
        let clock = Cell::new(first);
        let waits = RefCell::new(Vec::new());
        let done = wait_out_burst(
            first,
            |left| {
                waits.borrow_mut().push(left);
                if waits.borrow().len() > 100 {
                    return Err(RecvTimeoutError::Disconnected);
                }
                let (moved, answer) = next(left);
                clock.set(clock.get() + moved);
                answer
            },
            || clock.get(),
        );
        (done, clock.get() - first, waits.into_inner())
    }

    #[test]
    fn a_steady_stream_of_changes_still_flushes_at_max_wait() {
        let s = Duration::from_secs;
        // A relevant change every second, or one already queued: never quiet.
        let (done, at, waits) = burst_with(|_| (s(1), Ok(true)));
        assert!(done, "flushed, not given up");
        assert_eq!(at, MAX_WAIT);
        assert!(waits.iter().all(|w| !w.is_zero()), "no queued change is read past the deadline");
    }

    #[test]
    fn a_burst_ends_after_quiet_or_when_the_channel_closes() {
        let s = Duration::from_secs;
        // One more change after 1 s, then quiet: QUIET after that change.
        let mut calls = 0;
        let (done, at, _) = burst_with(|left| {
            calls += 1;
            if calls == 1 { (s(1), Ok(true)) } else { (left, Err(RecvTimeoutError::Timeout)) }
        });
        assert!(done);
        assert_eq!(at, s(1) + QUIET);
        // Irrelevant changes do not push the deadline.
        let (done, at, _) = burst_with(|_| (s(1), Ok(false)));
        assert!(done);
        assert_eq!(at, QUIET);
        let (done, _, _) = burst_with(|_| (Duration::ZERO, Err(RecvTimeoutError::Disconnected)));
        assert!(!done);
    }

    #[test]
    fn watcher_rescans_keep_a_gap_from_the_last_scan() {
        use std::time::{Duration, Instant};
        let s = Duration::from_secs;
        let t0 = Instant::now();
        // No scan yet, or the gap has passed: as soon as wanted.
        assert_eq!(next_rescan_at(None, t0), t0);
        assert_eq!(next_rescan_at(Some((t0, s(2))), t0 + s(40)), t0 + s(40));
        // Quick scan: 30 s after it started.
        assert_eq!(next_rescan_at(Some((t0, s(2))), t0 + s(5)), t0 + s(30));
        // Slow scan: three times its duration.
        assert_eq!(next_rescan_at(Some((t0, s(20))), t0 + s(25)), t0 + s(60));
        // A scan still running counts with the time it has run so far.
        assert_eq!(last_scan_so_far(None, t0), None);
        assert_eq!(last_scan_so_far(Some((t0, Some(s(4)))), t0 + s(90)), Some((t0, s(4))));
        assert_eq!(last_scan_so_far(Some((t0, None)), t0 + s(5)), Some((t0, s(5))));
        // Running 40 s: the rescan waits until 3 x 40 s after it started.
        assert_eq!(next_rescan_at(last_scan_so_far(Some((t0, None)), t0 + s(40)), t0 + s(40)), t0 + s(120));
    }

    #[test]
    fn refresh_requests_during_a_scan_collapse_into_one_follow_up() {
        use std::sync::atomic::AtomicUsize;
        let (p, _) = provider(vec![]);
        let scans = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let worker = {
            let (p, scans) = (p.clone(), scans.clone());
            std::thread::spawn(move || {
                p.refresh_with(move |_| {
                    if scans.fetch_add(1, Ordering::SeqCst) == 0 {
                        started_tx.send(()).unwrap();
                        go_rx.recv().unwrap();
                    }
                    vec![]
                })
            })
        };
        started_rx.recv().unwrap();
        // A scan is running: these return at once and do not scan themselves.
        for _ in 0..3 {
            p.refresh_with(|_| panic!("a second scan must not run alongside the first"));
        }
        go_tx.send(()).unwrap();
        worker.join().unwrap();
        p.wait_idle();
        assert_eq!(scans.load(Ordering::SeqCst), 2, "one scan plus exactly one follow-up");
    }

    #[test]
    fn watcher_ignores_changes_in_skipped_folders() {
        let roots = [PathBuf::from(r"C:\Users\x\Documents")];
        let rel = |p: &str| change_is_relevant(&[PathBuf::from(p)], &roots);
        assert!(rel(r"C:\Users\x\Documents\report.pdf"));
        assert!(rel(r"C:\Users\x\Documents\Tax\node_modules"));
        assert!(!rel(r"C:\Users\x\Documents\app\node_modules\x\index.js"));
        assert!(!rel(r"C:\Users\x\Documents\.git\index"));
        assert!(!rel(r"C:\Users\x\Documents\notes\.swp"));
        // Unknown places and pathless events are rescanned to be safe.
        assert!(rel(r"D:\elsewhere\file.txt"));
        assert!(change_is_relevant(&[], &roots));
    }

    #[test]
    fn the_periodic_refresh_scans_when_dirty_or_unwatched() {
        let young = Some(Duration::from_secs(60));
        assert!(!periodic_due(false, true, young));
        assert!(!periodic_due(false, true, None));
        assert!(periodic_due(true, true, young));
        assert!(periodic_due(false, false, young));
        assert!(periodic_due(true, false, young));
        // Age does not matter once it is due anyway.
        let old = Some(MAX_CLEAN_AGE * 3);
        assert!(periodic_due(true, false, old));
    }

    #[test]
    fn a_clean_watched_index_is_rescanned_once_the_last_scan_is_two_hours_old() {
        let min = |m: u64| Duration::from_secs(m * 60);
        assert!(!periodic_due(false, true, Some(min(119))));
        assert!(periodic_due(false, true, Some(min(120))));
        assert!(periodic_due(false, true, Some(min(121))));

        let p = watched_provider();
        let mut scans = 0;
        let t0 = Instant::now();
        // A completed scan that finished at t0.
        *p.last_scan.lock().unwrap() = Some((t0, Some(Duration::ZERO)));
        tick_at(&p, &mut scans, t0 + min(119));
        assert_eq!(scans, 0);
        tick_at(&p, &mut scans, t0 + min(120));
        assert_eq!(scans, 1, "too old: scanned although clean and watched");
        // The scan reset the age: the next tick, 30 minutes later, has nothing to do.
        let after = p.last_scan.lock().unwrap().unwrap().0;
        tick_at(&p, &mut scans, after + min(30));
        assert_eq!(scans, 1);
        // A scan still running does not count as completed.
        *p.last_scan.lock().unwrap() = Some((t0, None));
        tick_at(&p, &mut scans, t0 + min(500));
        assert_eq!(scans, 1);
    }

    fn tick_at(p: &FilesProvider, scans: &mut usize, now: Instant) {
        p.refresh_if_due_using(
            |_, _| {
                *scans += 1;
                Index::default()
            },
            path_exists,
            now,
        );
    }

    /// A provider whose watcher is alive and clean, and a scan that only counts.
    fn watched_provider() -> Arc<FilesProvider> {
        let (p, _) = provider(vec![]);
        p.watch_state.watching.store(true, Ordering::SeqCst);
        p
    }

    fn tick(p: &FilesProvider, scans: &mut usize) {
        tick_with(p, scans, path_exists);
    }

    fn tick_with(p: &FilesProvider, scans: &mut usize, exists: impl Fn(&Path) -> bool) {
        p.refresh_if_due_using(
            |_, _| {
                *scans += 1;
                Index::default()
            },
            exists,
            Instant::now(),
        );
    }

    fn event(kind: EventKind, path: &str) -> notify::Result<notify::Event> {
        Ok(notify::Event::new(kind).add_path(PathBuf::from(path)))
    }

    #[test]
    fn a_clean_watched_index_is_not_rescanned_by_the_periodic_tick() {
        let p = watched_provider();
        let mut scans = 0;
        for _ in 0..3 {
            tick(&p, &mut scans);
        }
        assert_eq!(scans, 0);
    }

    #[test]
    fn a_watcher_error_marks_the_index_dirty_and_the_next_tick_scans_once() {
        let p = watched_provider();
        let mut scans = 0;
        let sent = on_watch_result(&p.watch_state, &p.roots, Err(notify::Error::generic("lost")));
        assert!(sent.is_none());
        tick(&p, &mut scans);
        assert_eq!(scans, 1);
        // The scan cleared it: nothing more to do.
        tick(&p, &mut scans);
        assert_eq!(scans, 1);
    }

    #[test]
    fn an_unwatched_provider_rescans_on_every_tick() {
        let (p, _) = provider(vec![]);
        let mut scans = 0;
        tick(&p, &mut scans);
        tick(&p, &mut scans);
        assert_eq!(scans, 2);
    }

    #[test]
    fn a_change_during_a_scan_is_not_lost_and_a_scan_clears_dirty_when_it_starts() {
        let p = watched_provider();
        p.watch_state.dirty.store(true, Ordering::SeqCst);
        let mut seen_at_start = None;
        let state = p.watch_state.clone();
        p.refresh_using(|_, _| {
            seen_at_start = Some(state.dirty.load(Ordering::SeqCst));
            // The watcher reports a change while the walk runs.
            state.dirty.store(true, Ordering::SeqCst);
            Index::default()
        });
        assert_eq!(seen_at_start, Some(false));
        assert!(p.watch_state.dirty.load(Ordering::SeqCst), "the change made during the scan still counts");
    }

    #[test]
    fn what_the_watcher_cannot_vouch_for_marks_dirty() {
        use notify::event::{AccessKind, CreateKind, DataChange, Flag, MetadataKind, ModifyKind, RemoveKind, RenameMode};
        let (p, _) = provider(vec![]);
        let root = PathBuf::from(r"C:\Users\x");
        *p.roots.lock().unwrap() = vec![root.clone()];
        let dirty = || p.watch_state.dirty.swap(false, Ordering::SeqCst);
        let run = |res| on_watch_result(&p.watch_state, &p.roots, res);

        // A change the rescan applies: dirty until the scan starts, and sent on.
        let sent = run(event(EventKind::Create(CreateKind::File), r"C:\Users\x\a.txt"));
        assert_eq!(sent, Some(vec![PathBuf::from(r"C:\Users\x\a.txt")]));
        assert!(dirty());
        assert!(run(event(EventKind::Remove(RemoveKind::Any), r"C:\Users\x\a.txt")).is_some());
        assert!(dirty());
        assert!(run(event(EventKind::Modify(ModifyKind::Name(RenameMode::To)), r"C:\Users\x\b")).is_some());
        assert!(dirty());

        // The backend lost events: dirty, whatever the kind and even in a skipped folder.
        let lost = notify::Event::new(EventKind::Any).add_path(PathBuf::from(r"C:\Users\x\AppData\q")).set_flag(Flag::Rescan);
        assert!(run(Ok(lost)).is_none());
        assert!(dirty());
        let lost = notify::Event::new(EventKind::Modify(ModifyKind::Any)).set_flag(Flag::Rescan);
        assert!(run(Ok(lost)).is_some());
        assert!(dirty());

        // Ignored by the rescan logic, but could be an attribute change: dirty, not sent.
        for kind in [
            EventKind::Modify(ModifyKind::Any),
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)),
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime)),
            EventKind::Modify(ModifyKind::Other),
            EventKind::Any,
        ] {
            assert!(run(event(kind, r"C:\Users\x\Docs\a.txt")).is_none());
            assert!(dirty(), "{kind:?}");
        }
        // Content writes and reads cannot change the index.
        for kind in [EventKind::Modify(ModifyKind::Data(DataChange::Content)), EventKind::Access(AccessKind::Any)] {
            assert!(run(event(kind, r"C:\Users\x\Docs\a.txt")).is_none());
            assert!(!dirty(), "{kind:?}");
        }
        // Inside a skipped folder nothing counts.
        assert!(run(event(EventKind::Modify(ModifyKind::Any), r"C:\Users\x\AppData\Local\t.tmp")).is_none());
        assert!(run(event(EventKind::Create(CreateKind::Any), r"C:\Users\x\.git\x")).is_none());
        assert!(!dirty());
        // A watcher error always counts.
        assert!(run(Err(notify::Error::generic("x"))).is_none());
        assert!(dirty());
    }

    #[test]
    fn a_root_that_disappears_stops_vouching() {
        use notify::event::{ModifyKind, RemoveKind, RenameMode};
        let (p, _) = provider(vec![]);
        *p.roots.lock().unwrap() = vec![PathBuf::from(r"C:\Users\x\Docs")];
        p.watch_state.watching.store(true, Ordering::SeqCst);
        let run = |res| on_watch_result(&p.watch_state, &p.roots, res);
        // Something inside the root going away does not.
        run(event(EventKind::Remove(RemoveKind::Folder), r"C:\Users\x\Docs\sub"));
        assert!(p.watch_state.watching.load(Ordering::SeqCst));
        // The root itself, or a folder above it, does.
        run(event(EventKind::Remove(RemoveKind::Folder), r"c:\users\X\docs"));
        assert!(!p.watch_state.watching.load(Ordering::SeqCst));
        p.watch_state.watching.store(true, Ordering::SeqCst);
        run(event(EventKind::Modify(ModifyKind::Name(RenameMode::From)), r"C:\Users\x"));
        assert!(!p.watch_state.watching.load(Ordering::SeqCst));
        assert!(p.watch_state.dirty.load(Ordering::SeqCst));
        assert!(!event_ends_a_root(&EventKind::Create(notify::event::CreateKind::Any), &[PathBuf::from(r"C:\Users\x")], &[PathBuf::from(r"C:\Users\x")]));
    }

    #[test]
    fn new_roots_mark_the_index_dirty() {
        let p = watched_provider();
        p.set_roots(vec![PathBuf::from(r"C:\elsewhere")]);
        assert!(p.watch_state.dirty.load(Ordering::SeqCst));
        let mut scans = 0;
        tick(&p, &mut scans);
        assert_eq!(scans, 1);
    }

    #[test]
    fn a_watcher_that_cannot_start_leaves_the_periodic_refresh_on() {
        let dir = tempfile::tempdir().unwrap();
        let (p, _) = provider(vec![]);
        p.set_roots(vec![dir.path().to_path_buf(), dir.path().join("missing")]);
        p.watch_using(|_| true).unwrap();
        // A missing root is not watched: the periodic refresh must keep scanning.
        assert!(!p.watch_state.watching.load(Ordering::SeqCst));
        let mut scans = 0;
        tick(&p, &mut scans);
        tick(&p, &mut scans);
        assert_eq!(scans, 2);
        // All roots present: the watcher vouches again.
        let (p, _) = provider(vec![]);
        p.set_roots(vec![dir.path().to_path_buf()]);
        p.watch_using(|_| true).unwrap();
        assert!(p.watch_state.watching.load(Ordering::SeqCst));
    }

    fn fixed_c_only(letter: char) -> bool {
        letter.eq_ignore_ascii_case(&'c')
    }

    #[test]
    fn only_roots_on_a_local_fixed_drive_are_trusted() {
        let trusted = |p: &str| root_is_trusted(Path::new(p), fixed_c_only);
        assert!(trusted(r"C:\Users\x"));
        assert!(trusted(r"c:\"));
        assert!(trusted("C:/Users/x"));
        assert!(trusted(r"\\?\C:\Users\x"));
        // Another kind of drive (removable, network, unknown).
        assert!(!trusted(r"D:\Docs"));
        assert!(!trusted(r"\\?\D:\Docs"));
        // Network shares, whatever the spelling.
        assert!(!trusted(r"\\server\share\Docs"));
        assert!(!trusted("//server/share/Docs"));
        assert!(!trusted(r"\\?\UNC\server\share"));
        // No drive letter at all.
        assert!(!trusted(r"Users\x"));
        assert!(!trusted(""));
        assert!(!trusted(r"1:\x"));
    }

    #[test]
    fn the_real_drive_lookup_answers_for_the_system_drive() {
        // A letter that cannot exist is not fixed (no panic either way).
        assert!(!os_drive_is_fixed('#'));
    }

    #[test]
    fn a_root_that_vanishes_between_ticks_is_rescanned_without_any_event() {
        let (p, _) = provider(vec![]);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        p.watch_state.arm(&[root.clone()], true, path_exists, |_| true);
        assert!(p.watch_state.watching.load(Ordering::SeqCst));
        let mut scans = 0;
        // Unchanged root: no scan.
        tick_with(&p, &mut scans, path_exists);
        tick_with(&p, &mut scans, path_exists);
        assert_eq!(scans, 0);
        // The folder goes away, and no event arrives.
        fs::remove_dir_all(&root).unwrap();
        tick_with(&p, &mut scans, path_exists);
        assert_eq!(scans, 1);
        assert!(!p.watch_state.watching.load(Ordering::SeqCst));
        // Unwatched from now on: every tick scans, as before this round.
        tick_with(&p, &mut scans, path_exists);
        assert_eq!(scans, 2);
    }

    #[test]
    fn a_root_that_comes_back_is_noticed_too() {
        let (p, _) = provider(vec![]);
        let root = PathBuf::from(r"E:\usb\docs");
        p.watch_state.arm(&[root.clone()], false, |_| false, |_| true);
        // It was missing when the watch was set up (so not watched) ...
        assert!(!p.watch_state.watching.load(Ordering::SeqCst));
        p.watch_state.watching.store(true, Ordering::SeqCst);
        // ... and now it exists: the index is stale and the watch is not vouched for.
        let mut scans = 0;
        tick_with(&p, &mut scans, |_| true);
        assert_eq!(scans, 1);
        assert!(!p.watch_state.watching.load(Ordering::SeqCst));
    }

    #[test]
    fn a_network_root_is_never_vouched_for() {
        let (p, _) = provider(vec![]);
        let unc = PathBuf::from(r"\\server\share\docs");
        p.watch_state.arm(&[unc.clone()], true, |_| true, fixed_c_only);
        assert!(!p.watch_state.watching.load(Ordering::SeqCst));
        let mut scans = 0;
        for _ in 0..3 {
            tick_with(&p, &mut scans, |_| true);
        }
        assert_eq!(scans, 3, "due on every tick while such a root is configured");
        assert!(periodic_due(false, p.watch_state.watching.load(Ordering::SeqCst), None));
        // One untrusted root among local ones is enough.
        p.watch_state.arm(&[PathBuf::from(r"C:\Users\x"), unc], true, |_| true, fixed_c_only);
        assert!(!p.watch_state.watching.load(Ordering::SeqCst));
        p.watch_state.arm(&[PathBuf::from(r"C:\Users\x")], true, |_| true, fixed_c_only);
        assert!(p.watch_state.watching.load(Ordering::SeqCst));
        // Not every root watched: not vouched for either.
        p.watch_state.arm(&[PathBuf::from(r"C:\Users\x")], false, |_| true, fixed_c_only);
        assert!(!p.watch_state.watching.load(Ordering::SeqCst));
    }

    #[test]
    fn a_change_driven_scan_clears_what_the_watcher_marked() {
        use notify::event::CreateKind;
        let p = watched_provider();
        *p.roots.lock().unwrap() = vec![PathBuf::from(r"C:\Users\x")];
        let sent = on_watch_result(&p.watch_state, &p.roots, event(EventKind::Create(CreateKind::File), r"C:\Users\x\new.txt"));
        assert!(sent.is_some());
        assert!(p.watch_state.dirty.load(Ordering::SeqCst));
        // The throttled rescan the watcher thread starts is `refresh`: same path as here.
        let mut scans = 0;
        p.refresh_using(|_, _| {
            scans += 1;
            Index::default()
        });
        assert_eq!(scans, 1);
        assert!(!p.watch_state.dirty.load(Ordering::SeqCst));
        // So the 30-minute tick has nothing left to do.
        let mut ticks = 0;
        tick_with(&p, &mut ticks, |_| true);
        assert_eq!(ticks, 0);
    }

    fn temp_tree(files: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..files {
            touch(&dir.path().join(format!("folder number {}", i % 7)).join(format!("some file name {i}.txt")));
        }
        dir
    }

    #[test]
    fn a_scanned_index_keeps_no_spare_memory() {
        let dir = temp_tree(60);
        let roots = [dir.path().to_path_buf()];
        let scan = |hint| scan_index(&roots, None, MAX_ENTRIES, MAX_DEPTH, MAX_PER_FOLDER, hint);
        let first = scan(Sizes::default());
        let m = first.memory();
        assert!(first.len() >= 60);
        assert!(m.used > 0 && m.norms > 0);
        assert!(m.reserved as f64 <= m.used as f64 * 1.05, "{m:?}");
        // Sized from a previous index (here a bigger one): still trimmed at the end.
        let big = Sizes { slots: 5000, names: 200_000, norms: 200_000, dirs: 500 };
        let hinted = scan(big);
        assert_eq!(hinted.entries(), first.entries());
        let m = hinted.memory();
        assert!(m.reserved as f64 <= m.used as f64 * 1.05, "{m:?}");
        // The hint from an index of the same tree reserves enough: no regrowth.
        let again = scan(first.sizes());
        assert_eq!(again.entries(), first.entries());
        assert!(again.memory().reserved as f64 <= again.memory().used as f64 * 1.05);
    }

    #[test]
    fn memory_grows_with_the_entries() {
        let small = scan_index(&[temp_tree(10).path().to_path_buf()], None, MAX_ENTRIES, MAX_DEPTH, MAX_PER_FOLDER, Sizes::default()).memory();
        let big = scan_index(&[temp_tree(100).path().to_path_buf()], None, MAX_ENTRIES, MAX_DEPTH, MAX_PER_FOLDER, Sizes::default()).memory();
        assert!(big.used > small.used && big.reserved > small.reserved && big.norms > small.norms);
        assert!(small.norms < small.used);
        assert_eq!(Index::default().memory(), Default::default());
        assert_eq!(Index::default().sizes(), Sizes::default());
    }

    #[test]
    fn a_scan_gets_the_size_of_the_index_it_replaces() {
        let (p, _) = provider(vec![entry(r"C:\d\a.txt", false), entry(r"C:\d\b.txt", false)]);
        let mut hints = Vec::new();
        for _ in 0..2 {
            p.refresh_using(|_, hint| {
                hints.push(hint);
                Index::from_entries(vec![entry(r"C:\d\a.txt", false)])
            });
        }
        assert_eq!(hints[0].slots, 2);
        assert_eq!(hints[1].slots, 1);
    }

    /// Step 5 speed check. Run in release:
    /// `cargo test --release --manifest-path src-tauri/Cargo.toml -- --ignored query_speed_300k --nocapture`
    #[test]
    #[ignore]
    fn query_speed_300k() {
        query_speed(300_000, 20.0);
    }

    /// Same at the index cap. Run in release:
    /// `cargo test --release --manifest-path src-tauri/Cargo.toml -- --ignored query_speed_1m --nocapture`
    #[test]
    #[ignore]
    fn query_speed_1m() {
        query_speed(MAX_ENTRIES, 30.0);
    }

    fn query_speed(count: usize, limit_ms: f64) {
        use std::time::Instant;
        const WORDS: [&str; 48] = [
            "report", "invoice", "facture", "lettre", "Café", "notes", "meeting", "draft", "final", "backup",
            "project", "flick", "launcher", "budget", "summary", "photo", "Screenshot", "IMG", "DSC", "scan",
            "contract", "agreement", "resume", "letter", "presentation", "slides", "data", "export", "config", "setup",
            "README", "license", "changelog", "test", "results", "analysis", "thesis", "chapter", "lecture", "homework",
            "tax", "payslip", "statement", "travel", "ticket", "booking", "copy", "old",
        ];
        const EXTS: [&str; 12] = ["pdf", "docx", "xlsx", "jpg", "png", "txt", "md", "zip", "mp4", "pptx", "rs", "py"];
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut rnd = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let dirs: Vec<PathBuf> = (0..3000)
            .map(|i| {
                let mut d = PathBuf::from(r"C:\Users\someone").join(["Desktop", "Documents", "Downloads"][i % 3]);
                for _ in 0..1 + rnd(4) {
                    d.push(WORDS[rnd(WORDS.len())]);
                }
                d.push(format!("{i}"));
                d
            })
            .collect();
        let entries: Vec<FileEntry> = (0..count)
            .map(|i| {
                let name = match rnd(6) {
                    0 => format!("IMG_2023{:02}{:02}_{:06}.jpg", 1 + rnd(12), 1 + rnd(28), rnd(999_999)),
                    1 => format!("{} {}.{}", WORDS[rnd(48)], 2000 + rnd(26), EXTS[rnd(12)]),
                    2 => format!("{}_{}_v{}.{}", WORDS[rnd(48)], WORDS[rnd(48)], rnd(9), EXTS[rnd(12)]),
                    3 => format!("{} {} ({}).{}", WORDS[rnd(48)], WORDS[rnd(48)], rnd(5), EXTS[rnd(12)]),
                    4 => format!("{}-{}-{}", WORDS[rnd(48)], WORDS[rnd(48)], rnd(100)),
                    _ => format!("{}{}.{}", WORDS[rnd(48)].to_lowercase(), rnd(1000), EXTS[rnd(12)]),
                };
                FileEntry::new(dirs[rnd(dirs.len())].join(name), i % 10 == 0)
            })
            .collect();
        let (p, _) = provider(entries);
        {
            let ix = p.index.read().unwrap();
            let m = ix.memory();
            println!(
                "entries {}: used {} KB, reserved {} KB, norms {} KB ({:.1} % of used)",
                ix.len(), m.used / 1024, m.reserved / 1024, m.norms / 1024, m.norms as f64 * 100.0 / m.used as f64
            );
        }
        let queries = [
            "re", "in", "fi", "ti", "pd", "ca", "img", "rep", "doc", "2023", "notes", "tax", "invoice", "resum", "final pd",
            "meeting", "xq", "zzz", "fact 20", "screensh", "flick la", "statem",
        ];
        let mut worst = (0.0f64, "");
        let mut all = Vec::new();
        for text in queries {
            let mut times = Vec::new();
            for _ in 0..5 {
                let t = Instant::now();
                let out = p.query(&q(text));
                times.push(t.elapsed().as_secs_f64() * 1000.0);
                assert!(out.len() <= 20);
            }
            times.sort_by(|a, b| a.total_cmp(b));
            let (median, max) = (times[2], times[4]);
            println!("{text:>10}: median {median:6.2} ms, worst {max:6.2} ms");
            all.push(median);
            if max > worst.0 {
                worst = (max, text);
            }
        }
        all.sort_by(|a, b| a.total_cmp(b));
        println!("entries {count}: typical (median of medians) {:.2} ms, worst {:.2} ms ({:?})", all[all.len() / 2], worst.0, worst.1);
        if !cfg!(debug_assertions) {
            assert!(worst.0 <= limit_ms, "worst query took {:.2} ms", worst.0);
        }
    }

    #[test]
    fn has_help_line() {
        let (p, _) = provider(vec![]);
        let help = p.help();
        assert_eq!(help.len(), 1);
        assert_eq!(help[0].example, "report.pdf");
        assert_eq!(help[0].about, "find a file or folder by name; Ctrl+Enter shows it in Explorer");
        assert_eq!(help[0].fill, "");
    }

    #[test]
    fn files_take_at_most_five_rows_of_a_plain_list() {
        let (p, _) = provider(vec![]);
        assert_eq!(p.plain_share(), 5);
    }
}
