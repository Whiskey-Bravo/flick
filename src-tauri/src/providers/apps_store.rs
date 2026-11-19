//! Where the Store apps list comes from: the shell's own apps folder read in
//! process, or `Get-StartApps` through PowerShell.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::apps::AppEntry;

/// A source of the Store apps (packaged apps). `None` when it cannot answer,
/// so the caller keeps its last good list.
pub trait StoreSource: Send + Sync {
    fn list(&self) -> Option<Vec<AppEntry>>;

    /// As `list`, for a user's explicit Reload: every source is tried, no
    /// rate limit applies.
    fn list_forced(&self) -> Option<Vec<AppEntry>> {
        self.list()
    }
}

/// The apps folder read in process: no process start.
#[cfg_attr(test, allow(dead_code))]
pub struct InProcess;

impl StoreSource for InProcess {
    fn list(&self) -> Option<Vec<AppEntry>> {
        list()
    }
}

/// `Get-StartApps` through PowerShell (a process start, seconds at login).
#[cfg_attr(test, allow(dead_code))]
pub struct PowerShell;

impl StoreSource for PowerShell {
    fn list(&self) -> Option<Vec<AppEntry>> {
        super::apps::scan_store_apps()
    }
}

/// PowerShell runs at most this often as the fallback of a failed in-process read.
pub const FALLBACK_GAP: Duration = Duration::from_secs(10 * 60);

/// `primary`, and when it has no answer `fallback`, which is asked at most
/// once per `FALLBACK_GAP`.
pub struct Chain {
    primary: Arc<dyn StoreSource>,
    fallback: Arc<dyn StoreSource>,
    now: Arc<dyn Fn() -> Instant + Send + Sync>,
    last_fallback: Mutex<Option<Instant>>,
}

impl Chain {
    pub fn new(
        primary: Arc<dyn StoreSource>,
        fallback: Arc<dyn StoreSource>,
        now: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Self {
        Chain { primary, fallback, now, last_fallback: Mutex::new(None) }
    }
}

impl Chain {
    fn ask(&self, forced: bool) -> Option<Vec<AppEntry>> {
        if let Some(apps) = self.primary.list() {
            return Some(apps);
        }
        let now = (self.now)();
        {
            let mut last = self.last_fallback.lock().unwrap_or_else(|e| e.into_inner());
            if !forced && last.is_some_and(|t| now.saturating_duration_since(t) < FALLBACK_GAP) {
                return None;
            }
            *last = Some(now);
        }
        // No lock held while PowerShell runs.
        self.fallback.list()
    }
}

impl StoreSource for Chain {
    fn list(&self) -> Option<Vec<AppEntry>> {
        self.ask(false)
    }

    fn list_forced(&self) -> Option<Vec<AppEntry>> {
        self.ask(true)
    }
}

/// The source production uses; tests never reach the shell or PowerShell.
pub fn default_source() -> Arc<dyn StoreSource> {
    #[cfg(not(test))]
    {
        Arc::new(Chain::new(Arc::new(InProcess), Arc::new(PowerShell), Arc::new(Instant::now)))
    }
    #[cfg(test)]
    {
        Arc::new(testing::Fixed(None))
    }
}

/// Every packaged app of `shell:AppsFolder`, or `None` when the shell cannot
/// be read (or yields nothing). COM is initialised for the call and nothing
/// panics out of it.
pub fn list() -> Option<Vec<AppEntry>> {
    let pairs = std::panic::catch_unwind(|| crate::launch::with_com(|| unsafe { enumerate() })).ok()??;
    let apps: Vec<AppEntry> = pairs.iter().filter_map(|(name, id)| super::apps::store_entry(name, id)).collect();
    (!apps.is_empty()).then_some(apps)
}

/// (display name, parsing name) of every item of the apps folder.
///
/// # Safety
/// COM must be initialised on this thread.
unsafe fn enumerate() -> Option<Vec<(String, String)>> {
    use windows::Win32::UI::Shell::{
        BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, SHGetKnownFolderItem, KF_FLAG_DEFAULT,
        SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
    };

    /// The name as a string; the shell's buffer is always freed.
    unsafe fn name(item: &IShellItem, kind: SIGDN) -> Option<String> {
        let p = unsafe { item.GetDisplayName(kind) }.ok()?;
        let s = unsafe { p.to_string() }.ok();
        unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _)) };
        s
    }

    let folder: IShellItem = unsafe { SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) }.ok()?;
    let items: IEnumShellItems = unsafe { folder.BindToHandler(None, &BHID_EnumItems) }.ok()?;
    let mut out = Vec::new();
    loop {
        let mut batch: [Option<IShellItem>; 32] = Default::default();
        let mut got = 0u32;
        unsafe { items.Next(&mut batch, Some(&mut got)) }.ok()?;
        if got == 0 {
            break;
        }
        for item in batch.iter().take(got as usize).flatten() {
            if let (Some(n), Some(id)) = (unsafe { name(item, SIGDN_NORMALDISPLAY) }, unsafe { name(item, SIGDN_PARENTRELATIVEPARSING) }) {
                out.push((n, id));
            }
        }
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Answers with a fixed result and counts the calls.
    pub struct Fixed(pub Option<Vec<AppEntry>>);

    impl StoreSource for Fixed {
        fn list(&self) -> Option<Vec<AppEntry>> {
            self.0.clone()
        }
    }

    /// Like `Fixed`, counting its calls.
    pub struct Counting {
        pub answer: Option<Vec<AppEntry>>,
        pub calls: AtomicUsize,
    }

    impl Counting {
        pub fn new(answer: Option<Vec<AppEntry>>) -> Arc<Self> {
            Arc::new(Counting { answer, calls: AtomicUsize::new(0) })
        }
        pub fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl StoreSource for Counting {
        fn list(&self) -> Option<Vec<AppEntry>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer.clone()
        }
    }

    /// A clock that only moves when told to.
    #[derive(Clone)]
    pub struct Clock {
        base: Instant,
        offset: Arc<Mutex<Duration>>,
    }

    impl Clock {
        pub fn new() -> Self {
            Clock { base: Instant::now(), offset: Arc::new(Mutex::new(Duration::ZERO)) }
        }
        pub fn advance(&self, by: Duration) {
            *self.offset.lock().unwrap() += by;
        }
        pub fn now(&self) -> Instant {
            self.base + *self.offset.lock().unwrap()
        }
        pub fn as_fn(&self) -> Arc<dyn Fn() -> Instant + Send + Sync> {
            let c = self.clone();
            Arc::new(move || c.now())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::providers::apps::{store_entry, AppKind};

    fn calc() -> AppEntry {
        store_entry("Calculator", "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App").unwrap()
    }

    #[test]
    fn in_process_none_then_powershell_is_used() {
        let clock = Clock::new();
        let ps = Counting::new(Some(vec![calc()]));
        let chain = Chain::new(Arc::new(Fixed(None)), ps.clone(), clock.as_fn());
        assert_eq!(chain.list(), Some(vec![calc()]));
        assert_eq!(ps.calls(), 1);
    }

    #[test]
    fn powershell_fallback_runs_at_most_once_per_ten_minutes() {
        let clock = Clock::new();
        let ps = Counting::new(Some(vec![calc()]));
        let chain = Chain::new(Arc::new(Fixed(None)), ps.clone(), clock.as_fn());
        assert!(chain.list().is_some());
        clock.advance(Duration::from_secs(9 * 60));
        assert_eq!(chain.list(), None);
        assert_eq!(ps.calls(), 1);
        clock.advance(Duration::from_secs(61));
        assert!(chain.list().is_some());
        assert_eq!(ps.calls(), 2);
    }

    #[test]
    fn a_forced_listing_always_tries_powershell() {
        let clock = Clock::new();
        let ps = Counting::new(Some(vec![calc()]));
        let chain = Chain::new(Arc::new(Fixed(None)), ps.clone(), clock.as_fn());
        assert!(chain.list().is_some());
        clock.advance(Duration::from_secs(60));
        assert_eq!(chain.list(), None);
        assert_eq!(ps.calls(), 1);
        assert_eq!(chain.list_forced(), Some(vec![calc()]));
        assert_eq!(ps.calls(), 2);
        // The forced run counts as the last one: a normal one right after waits.
        assert_eq!(chain.list(), None);
        assert_eq!(ps.calls(), 2);
    }

    #[test]
    fn a_working_in_process_source_never_starts_powershell() {
        let clock = Clock::new();
        let ps = Counting::new(Some(vec![]));
        let chain = Chain::new(Arc::new(Fixed(Some(vec![calc()]))), ps.clone(), clock.as_fn());
        assert_eq!(chain.list(), Some(vec![calc()]));
        assert_eq!(ps.calls(), 0);
    }

    #[test]
    fn entries_are_store_entries() {
        assert_eq!(calc().kind, AppKind::Store);
        assert!(store_entry("", "a!b").is_none());
        assert!(store_entry("Desktop thing", r"C:\x\y.exe").is_none());
    }

    /// Reads the real shell and starts PowerShell (read-only listing): run by hand.
    #[test]
    #[ignore]
    fn store_listing_matches_powershell() {
        use std::collections::BTreeSet;
        let a = list();
        let b = super::super::apps::scan_store_apps();
        let set = |v: &Option<Vec<AppEntry>>| -> BTreeSet<(String, String)> {
            v.iter().flatten().map(|e| (e.name.clone(), e.target.clone())).collect()
        };
        let (a_set, b_set) = (set(&a), set(&b));
        println!("in-process: {:?} entries ({} distinct)", a.as_ref().map(Vec::len), a_set.len());
        println!("powershell: {:?} entries ({} distinct)", b.as_ref().map(Vec::len), b_set.len());
        for (n, id) in a_set.difference(&b_set) {
            println!("only in-process: {n}  [{id}]");
        }
        for (n, id) in b_set.difference(&a_set) {
            println!("only powershell: {n}  [{id}]");
        }
        assert!(a.is_some(), "in-process listing failed");
        assert_eq!(a_set, b_set);
    }
}
