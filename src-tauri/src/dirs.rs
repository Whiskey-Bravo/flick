//! Folder listings for path browsing, `dl` and `recent`: the file-system,
//! known-folder and shell-link reads sit behind traits (fakes in [`testing`]),
//! and a small keyed [`Loader`] runs them on background threads so the query
//! path only ever reads memory.

// Nothing here is wired into the app until the providers are registered.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashMap;
use std::os::windows::fs::MetadataExt;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Debug, PartialEq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    /// Bytes; 0 for folders.
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReadError {
    NotFound,
    Denied,
}

pub trait FileSystem: Send + Sync {
    /// The folder's entries, hidden and system entries skipped.
    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntry>, ReadError>;
    /// `Some(true)` folder, `Some(false)` file, `None` missing.
    fn kind(&self, path: &Path) -> Option<bool>;
}

pub trait KnownFolders: Send + Sync {
    fn downloads(&self) -> Option<PathBuf>;
    fn recent(&self) -> Option<PathBuf>;
}

pub trait Links: Send + Sync {
    /// The file-system path a `.lnk` points at.
    fn target(&self, lnk: &Path) -> Option<PathBuf>;
}

/// The current time (a fixed one in tests).
pub type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

/// The real file system.
pub struct SystemFs;

/// Not-found and path-not-found are `NotFound`; anything else (access denied,
/// a drive that is not ready, a lost network share) is `Denied`.
fn read_error(e: &std::io::Error) -> ReadError {
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PATH_NOT_FOUND: i32 = 3;
    match e.raw_os_error() {
        Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => ReadError::NotFound,
        None if e.kind() == std::io::ErrorKind::NotFound => ReadError::NotFound,
        _ => ReadError::Denied,
    }
}

impl FileSystem for SystemFs {
    /// Entries whose details can't be read are left out.
    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntry>, ReadError> {
        let entries = std::fs::read_dir(dir).map_err(|e| read_error(&e))?;
        let mut out = Vec::new();
        for entry in entries.flatten() {
            // From the directory listing itself: no extra disk access per entry.
            let Ok(meta) = entry.metadata() else { continue };
            let attrs = meta.file_attributes();
            if attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0 {
                continue;
            }
            let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
            out.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir,
                size: if is_dir { 0 } else { meta.len() },
                modified: meta.modified().ok(),
            });
        }
        Ok(out)
    }

    fn kind(&self, path: &Path) -> Option<bool> {
        std::fs::metadata(path).ok().map(|m| m.is_dir())
    }
}

/// Windows' own Downloads and Recent locations.
#[allow(dead_code)] // never built in tests; built by the app wiring
pub struct SystemFolders;

#[allow(dead_code)] // never built in tests
impl SystemFolders {
    fn known(id: &windows::core::GUID) -> Option<PathBuf> {
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};
        // SAFETY: the returned buffer is owned by us and freed once, after copying.
        unsafe {
            let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
            let path = raw.to_string().ok();
            CoTaskMemFree(Some(raw.0 as *const _));
            path.filter(|p| !p.is_empty()).map(PathBuf::from)
        }
    }
}

impl KnownFolders for SystemFolders {
    fn downloads(&self) -> Option<PathBuf> {
        Self::known(&windows::Win32::UI::Shell::FOLDERID_Downloads)
    }

    fn recent(&self) -> Option<PathBuf> {
        Self::known(&windows::Win32::UI::Shell::FOLDERID_Recent)
    }
}

/// Real `.lnk` targets, read without resolving.
#[allow(dead_code)] // never built in tests; built by the app wiring
pub struct ShellLinks;

impl Links for ShellLinks {
    fn target(&self, lnk: &Path) -> Option<PathBuf> {
        crate::launch::shortcut_target(lnk.to_str()?).map(PathBuf::from)
    }
}

/// What [`Loader::get`] has for a key right now.
pub enum Load<T> {
    /// A read runs and nothing is known yet.
    Pending,
    Ready(Arc<T>),
    NotFound,
    Denied,
    /// A read has run longer than the loader's `slow` with nothing to show.
    Slow,
}

impl<T> Clone for Load<T> {
    fn clone(&self) -> Self {
        match self {
            Load::Pending => Load::Pending,
            Load::Ready(v) => Load::Ready(Arc::clone(v)),
            Load::NotFound => Load::NotFound,
            Load::Denied => Load::Denied,
            Load::Slow => Load::Slow,
        }
    }
}

/// A value is served without a new read for this long.
pub const FRESH: Duration = Duration::from_secs(5);
/// A first read taking longer than this shows as [`Load::Slow`].
pub const SLOW: Duration = Duration::from_secs(3);
/// Keys kept; the least recently asked for goes first.
const MAX_KEYS: usize = 64;

struct Slot<T> {
    /// The last finished read and when it finished.
    done: Option<(Result<Arc<T>, ReadError>, Instant)>,
    /// The read in flight: when it started and its number.
    reading: Option<(Instant, u64)>,
    /// Tick of the last `get`.
    used: u64,
}

struct State<T> {
    slots: HashMap<String, Slot<T>>,
    tick: u64,
}

/// Keyed background reads, memory only. See [`Loader::get`].
pub struct Loader<T> {
    on_ready: Arc<dyn Fn() + Send + Sync>,
    fresh: Duration,
    slow: Duration,
    state: Mutex<State<T>>,
}

impl<T: Send + Sync + 'static> Loader<T> {
    pub fn new(on_ready: Arc<dyn Fn() + Send + Sync>, fresh: Duration, slow: Duration) -> Arc<Self> {
        Arc::new(Self { on_ready, fresh, slow, state: Mutex::new(State { slots: HashMap::new(), tick: 0 }) })
    }

    /// What is known for `key`, never waiting. A value younger than `fresh`
    /// is returned as is; an older one (or an error) is returned while `load`
    /// runs again in the background. With nothing known, `load` starts and the
    /// answer is `Pending`, then `Slow` once it has run longer than `slow`.
    /// At most one read per key is in flight: `load` is dropped unrun while
    /// one is. Every finished read calls `on_ready` once; a first read also
    /// calls it once at `slow` if still running, so the caller asks again.
    /// A stuck read is never joined; if it ends later, its result is stored.
    pub fn get(self: &Arc<Self>, key: &str, load: impl FnOnce() -> Result<T, ReadError> + Send + 'static) -> Load<T> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.tick += 1;
        let tick = state.tick;
        if !state.slots.contains_key(key) {
            if state.slots.len() >= MAX_KEYS {
                evict_oldest(&mut state.slots);
            }
            state.slots.insert(key.to_string(), Slot { done: None, reading: None, used: tick });
        }
        let Some(slot) = state.slots.get_mut(key) else { return Load::Pending };
        slot.used = tick;
        let now = Instant::now();
        let out = match &slot.done {
            Some((Ok(v), _)) => Load::Ready(Arc::clone(v)),
            Some((Err(ReadError::NotFound), _)) => Load::NotFound,
            Some((Err(ReadError::Denied), _)) => Load::Denied,
            None => match slot.reading {
                Some((since, _)) if now.duration_since(since) >= self.slow => Load::Slow,
                _ => Load::Pending,
            },
        };
        let stale = slot.done.as_ref().map_or(true, |(_, at)| now.duration_since(*at) >= self.fresh);
        if stale && slot.reading.is_none() {
            slot.reading = Some((now, tick));
            let first = slot.done.is_none();
            drop(state);
            self.start(key.to_string(), tick, load, first);
        }
        out
    }

    /// The last value read for `key`, however old.
    pub fn peek(&self, key: &str) -> Option<Arc<T>> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match &state.slots.get(key)?.done {
            Some((Ok(v), _)) => Some(Arc::clone(v)),
            _ => None,
        }
    }

    fn start(self: &Arc<Self>, key: String, id: u64, load: impl FnOnce() -> Result<T, ReadError> + Send + 'static, first: bool) {
        if first {
            let weak: Weak<Self> = Arc::downgrade(self);
            let (key, slow) = (key.clone(), self.slow);
            std::thread::spawn(move || {
                std::thread::sleep(slow);
                let Some(me) = weak.upgrade() else { return };
                let running = {
                    let state = me.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.slots.get(&key).and_then(|s| s.reading).is_some_and(|(_, r)| r == id)
                };
                if running {
                    (me.on_ready)();
                }
            });
        }
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            // A panicking read must not leave the key "in flight" forever.
            let result = std::panic::catch_unwind(AssertUnwindSafe(load)).unwrap_or(Err(ReadError::Denied));
            {
                let mut state = me.state.lock().unwrap_or_else(|e| e.into_inner());
                // Dropped when the key was evicted meanwhile.
                if let Some(slot) = state.slots.get_mut(&key).filter(|s| s.reading.is_some_and(|(_, r)| r == id)) {
                    slot.done = Some((result.map(Arc::new), Instant::now()));
                    slot.reading = None;
                }
            }
            (me.on_ready)();
        });
    }
}

/// Drops the least recently asked-for key, preferring one with no read in flight.
fn evict_oldest<T>(slots: &mut HashMap<String, Slot<T>>) {
    let oldest = |idle_only: bool| {
        slots
            .iter()
            .filter(|(_, s)| !idle_only || s.reading.is_none())
            .min_by_key(|(_, s)| s.used)
            .map(|(k, _)| k.clone())
    };
    if let Some(key) = oldest(true).or_else(|| oldest(false)) {
        slots.remove(&key);
    }
}

/// Cache key of a folder: lowercase, `/` as `\`, exactly one trailing `\`.
pub fn folder_key(dir: &Path) -> String {
    let mut key = dir.to_string_lossy().replace('/', "\\").to_lowercase();
    while key.ends_with('\\') {
        key.pop();
    }
    key.push('\\');
    key
}

/// How long ago `then` was: `Just now`, `5 min ago`, `2 h ago`, `Yesterday`, `3 days ago`.
pub fn ago(now: SystemTime, then: SystemTime) -> String {
    let secs = now.duration_since(then).map_or(0, |d| d.as_secs());
    match secs {
        s if s < 60 => "Just now".into(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 86_400 => format!("{} h ago", s / 3600),
        s if s < 2 * 86_400 => "Yesterday".into(),
        s => format!("{} days ago", s / 86_400),
    }
}

/// `12 B`, `820 KB`, `1.4 MB`, `14 MB` (1024 based): one decimal below 10.
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 3] = ["KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 9.95 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
pub mod testing {
    //! Fakes for the traits above.

    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};

    use super::{folder_key, DirEntry, FileSystem, KnownFolders, Links, ReadError};

    pub fn file(name: &str, size: u64) -> DirEntry {
        DirEntry { name: name.into(), is_dir: false, size, modified: None }
    }

    pub fn folder(name: &str) -> DirEntry {
        DirEntry { name: name.into(), is_dir: true, size: 0, modified: None }
    }

    /// Lookup key of any path: [`folder_key`] without the trailing `\`.
    fn path_key(path: &Path) -> String {
        folder_key(path).trim_end_matches('\\').to_string()
    }

    /// Folders and kinds held in memory. A missing folder reads as `NotFound`.
    #[derive(Default)]
    pub struct FakeFs {
        dirs: Mutex<HashMap<String, Result<Vec<DirEntry>, ReadError>>>,
        kinds: Mutex<HashMap<String, bool>>,
        reads: AtomicUsize,
        blocked: Mutex<bool>,
        gate: Condvar,
    }

    impl FakeFs {
        /// `dir` lists `entries`; the folder and its entries get their kinds.
        pub fn add_dir(&self, dir: &str, entries: Vec<DirEntry>) {
            let mut kinds = self.kinds.lock().unwrap();
            kinds.insert(path_key(Path::new(dir)), true);
            for e in &entries {
                kinds.insert(path_key(&Path::new(dir).join(&e.name)), e.is_dir);
            }
            self.dirs.lock().unwrap().insert(folder_key(Path::new(dir)), Ok(entries));
        }

        pub fn fail_dir(&self, dir: &str, error: ReadError) {
            self.dirs.lock().unwrap().insert(folder_key(Path::new(dir)), Err(error));
        }

        pub fn set_kind(&self, path: &str, kind: Option<bool>) {
            let mut kinds = self.kinds.lock().unwrap();
            match kind {
                Some(is_dir) => kinds.insert(path_key(Path::new(path)), is_dir),
                None => kinds.remove(&path_key(Path::new(path))),
            };
        }

        /// `read_dir` calls that got past the gate.
        pub fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }

        /// Later `read_dir` calls wait until [`FakeFs::release`].
        pub fn block(&self) {
            *self.blocked.lock().unwrap() = true;
        }

        pub fn release(&self) {
            *self.blocked.lock().unwrap() = false;
            self.gate.notify_all();
        }
    }

    impl FileSystem for FakeFs {
        fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntry>, ReadError> {
            let mut blocked = self.blocked.lock().unwrap();
            while *blocked {
                blocked = self.gate.wait(blocked).unwrap();
            }
            drop(blocked);
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.dirs.lock().unwrap().get(&folder_key(dir)).cloned().unwrap_or(Err(ReadError::NotFound))
        }

        fn kind(&self, path: &Path) -> Option<bool> {
            self.kinds.lock().unwrap().get(&path_key(path)).copied()
        }
    }

    #[derive(Default)]
    pub struct FakeFolders {
        pub downloads: Option<PathBuf>,
        pub recent: Option<PathBuf>,
    }

    impl KnownFolders for FakeFolders {
        fn downloads(&self) -> Option<PathBuf> {
            self.downloads.clone()
        }

        fn recent(&self) -> Option<PathBuf> {
            self.recent.clone()
        }
    }

    /// `.lnk` path → target; unknown links have none.
    #[derive(Default)]
    pub struct FakeLinks {
        links: Mutex<HashMap<String, PathBuf>>,
    }

    impl FakeLinks {
        pub fn add(&self, lnk: &Path, target: &str) {
            self.links.lock().unwrap().insert(path_key(lnk), PathBuf::from(target));
        }
    }

    impl Links for FakeLinks {
        fn target(&self, lnk: &Path) -> Option<PathBuf> {
            self.links.lock().unwrap().get(&path_key(lnk)).cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver};
    use std::time::UNIX_EPOCH;

    /// A loader whose `on_ready` calls arrive on the receiver.
    fn loader(fresh: Duration, slow: Duration) -> (Arc<Loader<u32>>, Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let l = Loader::new(
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
            fresh,
            slow,
        );
        (l, rx)
    }

    fn wait(rx: &Receiver<()>) {
        rx.recv_timeout(Duration::from_secs(1)).expect("on_ready within 1 s");
    }

    fn show(load: Load<u32>) -> String {
        match load {
            Load::Pending => "Pending".into(),
            Load::Ready(v) => format!("Ready({v})"),
            Load::NotFound => "NotFound".into(),
            Load::Denied => "Denied".into(),
            Load::Slow => "Slow".into(),
        }
    }

    #[test]
    fn first_get_is_pending_then_ready() {
        let (l, rx) = loader(FRESH, SLOW);
        assert_eq!(show(l.get("a", || Ok(7))), "Pending");
        wait(&rx);
        assert_eq!(show(l.get("a", || unreachable!())), "Ready(7)");
        assert_eq!(l.peek("a").as_deref(), Some(&7));
    }

    #[test]
    fn one_read_per_key_at_a_time() {
        let (l, rx) = loader(FRESH, SLOW);
        let runs = Arc::new(AtomicUsize::new(0));
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let r = runs.clone();
        assert_eq!(
            show(l.get("a", move || {
                r.fetch_add(1, Ordering::SeqCst);
                let _ = gate_rx.recv();
                Ok(1)
            })),
            "Pending"
        );
        let r = runs.clone();
        assert_eq!(
            show(l.get("a", move || {
                r.fetch_add(1, Ordering::SeqCst);
                Ok(2)
            })),
            "Pending"
        );
        gate_tx.send(()).unwrap();
        wait(&rx);
        assert_eq!(show(l.get("a", || unreachable!())), "Ready(1)");
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_fresh_value_is_served_without_reading() {
        let (l, rx) = loader(Duration::from_secs(60), SLOW);
        l.get("a", || Ok(1));
        wait(&rx);
        let runs = Arc::new(AtomicUsize::new(0));
        let r = runs.clone();
        let got = l.get("a", move || {
            r.fetch_add(1, Ordering::SeqCst);
            Ok(2)
        });
        assert_eq!(show(got), "Ready(1)");
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "no read, no on_ready");
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_stale_value_is_served_while_reading_again() {
        let (l, rx) = loader(Duration::ZERO, SLOW);
        l.get("a", || Ok(1));
        wait(&rx);
        assert_eq!(show(l.get("a", || Ok(2))), "Ready(1)");
        wait(&rx);
        assert_eq!(l.peek("a").as_deref(), Some(&2));
    }

    #[test]
    fn a_slow_read_turns_into_slow_and_is_never_awaited() {
        let (l, rx) = loader(FRESH, Duration::from_millis(50));
        let runs = Arc::new(AtomicUsize::new(0));
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let r = runs.clone();
        let t = Instant::now();
        let first = l.get("a", move || {
            r.fetch_add(1, Ordering::SeqCst);
            let _ = gate_rx.recv();
            Ok(5)
        });
        assert!(t.elapsed() < Duration::from_millis(20), "get never waits for the read");
        assert_eq!(show(first), "Pending");
        // The timer, not the stuck read, says it is time to ask again.
        wait(&rx);
        assert!(t.elapsed() >= Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(60).saturating_sub(t.elapsed()));
        assert_eq!(show(l.get("a", || unreachable!())), "Slow");
        let r = runs.clone();
        assert_eq!(
            show(l.get("a", move || {
                r.fetch_add(1, Ordering::SeqCst);
                Ok(6)
            })),
            "Slow"
        );
        assert_eq!(runs.load(Ordering::SeqCst), 1, "no second read while one is in flight");
        gate_tx.send(()).unwrap();
        wait(&rx);
        assert_eq!(show(l.get("a", || unreachable!())), "Ready(5)");
    }

    #[test]
    fn errors_are_reported() {
        let (l, rx) = loader(FRESH, SLOW);
        l.get("a", || Err(ReadError::NotFound));
        wait(&rx);
        l.get("b", || Err(ReadError::Denied));
        wait(&rx);
        assert_eq!(show(l.get("a", || unreachable!())), "NotFound");
        assert_eq!(show(l.get("b", || unreachable!())), "Denied");
        assert!(l.peek("a").is_none());
    }

    #[test]
    fn oldest_keys_are_evicted_past_64() {
        let (l, rx) = loader(FRESH, SLOW);
        for i in 0..65u32 {
            l.get(&i.to_string(), move || Ok(i));
            wait(&rx);
        }
        assert!(l.peek("0").is_none());
        assert_eq!(l.peek("1").as_deref(), Some(&1));
        assert_eq!(l.peek("64").as_deref(), Some(&64));
    }

    #[test]
    fn folder_key_ignores_case_and_separators() {
        let a = folder_key(Path::new(r"C:\Games"));
        assert_eq!(a, r"c:\games\");
        assert_eq!(folder_key(Path::new("c:/games/")), a);
        assert_eq!(folder_key(Path::new(r"C:\Games\")), a);
        assert_eq!(folder_key(Path::new(r"C:\")), r"c:\");
        assert_eq!(folder_key(Path::new(r"\\Server\Share\")), r"\\server\share\");
    }

    fn set_attributes(path: &Path, attrs: u32) {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_FLAGS_AND_ATTRIBUTES};
        unsafe { SetFileAttributesW(&HSTRING::from(path.as_os_str()), FILE_FLAGS_AND_ATTRIBUTES(attrs)) }.unwrap();
    }

    #[test]
    fn system_fs_skips_hidden_and_system_entries() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), b"hello").unwrap();
        std::fs::create_dir(root.join("Sub")).unwrap();
        std::fs::write(root.join("hidden.txt"), b"").unwrap();
        set_attributes(&root.join("hidden.txt"), FILE_ATTRIBUTE_HIDDEN);
        std::fs::write(root.join("system.txt"), b"").unwrap();
        set_attributes(&root.join("system.txt"), FILE_ATTRIBUTE_SYSTEM);
        std::fs::create_dir(root.join("Secret")).unwrap();
        set_attributes(&root.join("Secret"), FILE_ATTRIBUTE_HIDDEN);

        let mut out = SystemFs.read_dir(root).unwrap();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = out.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Sub", "a.txt"]);
        assert!(out[0].is_dir);
        assert!(!out[1].is_dir);
        assert_eq!(out[1].size, 5);
        assert!(out[1].modified.is_some());
        assert_eq!(SystemFs.kind(&root.join("a.txt")), Some(false));
        assert_eq!(SystemFs.kind(&root.join("Sub")), Some(true));
        assert_eq!(SystemFs.kind(&root.join("nope.txt")), None);
    }

    #[test]
    fn system_fs_reports_missing_as_not_found() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(SystemFs.read_dir(&dir.path().join("nope")), Err(ReadError::NotFound));
        assert_eq!(SystemFs.read_dir(&dir.path().join("nope").join("deeper")), Err(ReadError::NotFound));
    }

    #[test]
    fn ago_formats() {
        let now = UNIX_EPOCH + Duration::from_secs(10_000_000);
        let before = |secs: u64| now - Duration::from_secs(secs);
        assert_eq!(ago(now, before(30)), "Just now");
        assert_eq!(ago(now, now + Duration::from_secs(90)), "Just now");
        assert_eq!(ago(now, before(5 * 60)), "5 min ago");
        assert_eq!(ago(now, before(59 * 60 + 59)), "59 min ago");
        assert_eq!(ago(now, before(2 * 3600)), "2 h ago");
        assert_eq!(ago(now, before(23 * 3600)), "23 h ago");
        assert_eq!(ago(now, before(30 * 3600)), "Yesterday");
        assert_eq!(ago(now, before(3 * 86_400)), "3 days ago");
    }

    #[test]
    fn size_formats() {
        assert_eq!(size_text(0), "0 B");
        assert_eq!(size_text(12), "12 B");
        assert_eq!(size_text(1023), "1023 B");
        assert_eq!(size_text(2048), "2.0 KB");
        assert_eq!(size_text(820 * 1024), "820 KB");
        assert_eq!(size_text(1_468_006), "1.4 MB");
        assert_eq!(size_text(14 * 1024 * 1024), "14 MB");
        assert_eq!(size_text(3 * 1024 * 1024 * 1024), "3.0 GB");
        assert_eq!(size_text(40 * 1024 * 1024 * 1024), "40 GB");
    }
}
