//! Windows' clipboard history (Win+V) behind a trait. The real implementation
//! talks to WinRT; tests use `testing::FakeHistory` and never construct
//! `WinRtHistory`. Nothing read here is stored or logged: items live in
//! memory until `forget`.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::core::{HRESULT, HSTRING};
use windows::ApplicationModel::DataTransfer::{
    Clipboard, ClipboardHistoryItem, ClipboardHistoryItemsResultStatus, SetHistoryItemAsContentStatus,
    StandardDataFormats,
};
use windows::Win32::Foundation::{E_NOINTERFACE, E_NOTIMPL, REGDB_E_CLASSNOTREG};
use windows::Win32::System::Com::{
    CoIncrementMTAUsage, CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};

pub const MAX_ITEMS: usize = 50;

#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    /// Windows' item id.
    pub id: String,
    pub text: String,
    pub copied: SystemTime,
}

#[derive(Clone, Debug, PartialEq)]
pub enum History {
    Off,
    Items(Vec<Clip>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryError {
    /// The API cannot be used from this process.
    Unsupported,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetError {
    Gone,
    Failed,
}

pub trait ClipHistory: Send + Sync {
    /// Blocks; never call it on the query path.
    fn read(&self) -> Result<History, HistoryError>;
    fn set_content(&self, id: &str) -> Result<(), SetError>;
    /// Drops the items held from the last read.
    fn forget(&self);
}

/// 100 ns ticks between 1601-01-01 and 1970-01-01 (UTC).
const TICKS_TO_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
const TICKS_PER_SEC: i64 = 10_000_000;

/// WinRT `DateTime` ticks (100 ns since 1601-01-01 UTC) to `SystemTime`;
/// anything before 1970 clamps to `UNIX_EPOCH`.
pub fn from_universal_time(ticks: i64) -> SystemTime {
    let since = ticks.saturating_sub(TICKS_TO_UNIX_EPOCH);
    if since <= 0 {
        return UNIX_EPOCH;
    }
    let secs = (since / TICKS_PER_SEC) as u64;
    let nanos = ((since % TICKS_PER_SEC) * 100) as u32;
    UNIX_EPOCH + Duration::new(secs, nanos)
}

/// Text and no files.
pub fn is_text_item(has_text: bool, has_files: bool) -> bool {
    has_text && !has_files
}

/// Newest first (stable), at most `MAX_ITEMS`.
pub fn newest_first(mut clips: Vec<Clip>) -> Vec<Clip> {
    clips.sort_by(|a, b| b.copied.cmp(&a.copied));
    clips.truncate(MAX_ITEMS);
    clips
}

/// Activation errors mean the API cannot be used from this process.
fn history_error(code: HRESULT) -> HistoryError {
    if code == REGDB_E_CLASSNOTREG || code == E_NOINTERFACE || code == E_NOTIMPL {
        HistoryError::Unsupported
    } else {
        HistoryError::Failed
    }
}

fn winrt_error(e: windows::core::Error) -> HistoryError {
    history_error(e.code())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Held items outlive the call that read them and may be released on any
/// thread. If the last apartment in the process closed in between, COM would
/// unload the WinRT server and that release would crash, so keep the MTA
/// alive for the life of the process (once; never decremented).
fn keep_com_loaded() {
    static ONCE: Once = Once::new();
    // SAFETY: plain process-wide COM usage count; the cookie is deliberately
    // never passed to CoDecrementMTAUsage.
    ONCE.call_once(|| {
        let _ = unsafe { CoIncrementMTAUsage() };
    });
}

/// Run `f` in a fresh STA on a short-lived thread of its own. Every history
/// call goes through here, so the result never depends on the caller's
/// thread (a Loader thread, a Tauri command thread, an MTA or STA thread).
/// It is an STA, not the MTA, because the `Clipboard` statics refuse the MTA
/// (`RO_E_UNSUPPORTED_FROM_MTA`, 0x8000001D, seen by hand). The items it
/// returns are agile (`Send + Sync`), so a later call on another such thread
/// can use them after this one has exited; `keep_com_loaded` keeps the
/// server loaded in between.
fn in_sta<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    std::thread::scope(|s| {
        s.spawn(|| {
            // SAFETY: plain COM init/uninit pair on this new thread.
            let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
            let out = f();
            if com.is_ok() {
                unsafe { CoUninitialize() };
            }
            out
        })
        .join()
        .unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

/// Map raw items to clips, skipping any `to_clip` rejects (not text, or
/// unreadable), then keep the newest `MAX_ITEMS` and hold only those, by id.
fn gather<T>(
    items: impl IntoIterator<Item = T>,
    mut to_clip: impl FnMut(&T) -> Option<Clip>,
) -> (Vec<Clip>, HashMap<String, T>) {
    let mut clips = Vec::new();
    let mut held = HashMap::new();
    for item in items {
        if let Some(clip) = to_clip(&item) {
            held.insert(clip.id.clone(), item);
            clips.push(clip);
        }
    }
    let clips = newest_first(clips);
    held.retain(|id, _| clips.iter().any(|c| &c.id == id));
    (clips, held)
}

/// The items of the last read, by id, with a generation: every read `begin`s
/// a new one and `forget` bumps it, so a late or abandoned read can neither
/// bring back forgotten items nor overwrite a newer read's items.
struct Held<T> {
    state: Mutex<(u64, HashMap<String, T>)>,
}

impl<T: Clone> Held<T> {
    fn new() -> Self {
        Held { state: Mutex::new((0, HashMap::new())) }
    }

    fn begin(&self) -> u64 {
        let mut state = lock(&self.state);
        state.0 += 1;
        state.0
    }

    /// Replace the held items if no read or forget came after `generation`.
    /// Old (or refused) items are dropped after the lock is released.
    fn store_if(&self, generation: u64, items: HashMap<String, T>) -> bool {
        let (dropped, stored) = {
            let mut state = lock(&self.state);
            if state.0 == generation {
                (std::mem::replace(&mut state.1, items), true)
            } else {
                (items, false)
            }
        };
        drop(dropped);
        stored
    }

    fn get(&self, id: &str) -> Option<T> {
        lock(&self.state).1.get(id).cloned()
    }
}

impl<T: Send + 'static> Held<T> {
    /// Forgets the items. They are released on a thread of their own: the
    /// caller is the hiding bar's (UI) thread, and releasing a WinRT item
    /// is a cross-apartment call that may block.
    fn forget(&self) {
        let dropped = {
            let mut state = lock(&self.state);
            state.0 += 1;
            std::mem::take(&mut state.1)
        };
        if !dropped.is_empty() {
            std::thread::spawn(move || drop(dropped));
        }
    }
}

/// One history item as a clip; `None` when it is not plain text or any part
/// of it cannot be read.
fn clip_of(item: &ClipboardHistoryItem, text: &HSTRING, files: &HSTRING) -> Option<Clip> {
    let content = item.Content().ok()?;
    if !is_text_item(content.Contains(text).ok()?, content.Contains(files).ok()?) {
        return None;
    }
    let body = content.GetTextAsync().ok()?.get().ok()?;
    Some(Clip {
        id: item.Id().ok()?.to_string_lossy(),
        text: body.to_string_lossy(),
        copied: from_universal_time(item.Timestamp().ok()?.UniversalTime),
    })
}

type Read = Result<(History, HashMap<String, ClipboardHistoryItem>), HistoryError>;

/// Must run in an STA (see `in_sta`).
fn fetch() -> Read {
    if !Clipboard::IsHistoryEnabled().map_err(winrt_error)? {
        return Ok((History::Off, HashMap::new()));
    }
    let result = Clipboard::GetHistoryItemsAsync()
        .map_err(winrt_error)?
        .get()
        .map_err(winrt_error)?;
    let status = result.Status().map_err(winrt_error)?;
    if status == ClipboardHistoryItemsResultStatus::ClipboardHistoryDisabled {
        return Ok((History::Off, HashMap::new()));
    }
    if status != ClipboardHistoryItemsResultStatus::Success {
        // AccessDenied, or a status this code does not know.
        return Err(HistoryError::Failed);
    }
    let items = result.Items().map_err(winrt_error)?;
    let text = StandardDataFormats::Text().map_err(winrt_error)?;
    let files = StandardDataFormats::StorageItems().map_err(winrt_error)?;
    let (clips, held) = gather(items, |item| clip_of(item, &text, &files));
    Ok((History::Items(clips), held))
}

/// The real history over WinRT `Clipboard`. Holds the items of the last read
/// by id, so `set_content` acts on exactly the item a row was built from.
pub struct WinRtHistory {
    held: Held<ClipboardHistoryItem>,
}

impl WinRtHistory {
    pub fn new() -> Self {
        keep_com_loaded();
        WinRtHistory { held: Held::new() }
    }
}

impl Default for WinRtHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipHistory for WinRtHistory {
    fn read(&self) -> Result<History, HistoryError> {
        let generation = self.held.begin();
        match in_sta(fetch) {
            Ok((history, held)) => {
                self.held.store_if(generation, held);
                Ok(history)
            }
            Err(e) => {
                self.held.store_if(generation, HashMap::new());
                Err(e)
            }
        }
    }

    fn set_content(&self, id: &str) -> Result<(), SetError> {
        let item = self.held.get(id).ok_or(SetError::Gone)?;
        in_sta(move || match Clipboard::SetHistoryItemAsContent(&item) {
            Ok(s) if s == SetHistoryItemAsContentStatus::Success => Ok(()),
            Ok(s) if s == SetHistoryItemAsContentStatus::ItemDeleted => Err(SetError::Gone),
            _ => Err(SetError::Failed),
        })
    }

    fn forget(&self) {
        self.held.forget();
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Condvar;

    struct State {
        result: Result<History, HistoryError>,
        reads: usize,
        blocked: bool,
        set_calls: Vec<String>,
        gone: HashSet<String>,
        forgets: usize,
    }

    /// A scripted history: `read` returns what `set` gave (empty items by
    /// default) and waits while `block`ed; `set_content` records ids.
    pub struct FakeHistory {
        state: Mutex<State>,
        gate: Condvar,
    }

    impl FakeHistory {
        pub fn new() -> Self {
            FakeHistory {
                state: Mutex::new(State {
                    result: Ok(History::Items(Vec::new())),
                    reads: 0,
                    blocked: false,
                    set_calls: Vec::new(),
                    gone: HashSet::new(),
                    forgets: 0,
                }),
                gate: Condvar::new(),
            }
        }

        pub fn set(&self, result: Result<History, HistoryError>) {
            lock(&self.state).result = result;
        }

        /// Reads started so far (counted before any wait on the gate).
        pub fn reads(&self) -> usize {
            lock(&self.state).reads
        }

        pub fn block(&self) {
            lock(&self.state).blocked = true;
        }

        pub fn release(&self) {
            lock(&self.state).blocked = false;
            self.gate.notify_all();
        }

        pub fn set_calls(&self) -> Vec<String> {
            lock(&self.state).set_calls.clone()
        }

        pub fn mark_gone(&self, id: &str) {
            lock(&self.state).gone.insert(id.to_string());
        }

        pub fn forgets(&self) -> usize {
            lock(&self.state).forgets
        }
    }

    impl Default for FakeHistory {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ClipHistory for FakeHistory {
        fn read(&self) -> Result<History, HistoryError> {
            let mut state = lock(&self.state);
            state.reads += 1;
            while state.blocked {
                state = self.gate.wait(state).unwrap_or_else(|p| p.into_inner());
            }
            state.result.clone()
        }

        fn set_content(&self, id: &str) -> Result<(), SetError> {
            let mut state = lock(&self.state);
            state.set_calls.push(id.to_string());
            if state.gone.contains(id) {
                Err(SetError::Gone)
            } else {
                Ok(())
            }
        }

        fn forget(&self) {
            lock(&self.state).forgets += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeHistory;
    use super::*;
    use std::sync::Arc;

    fn clip(id: &str, secs: u64) -> Clip {
        Clip {
            id: id.to_string(),
            text: format!("text {id}"),
            copied: UNIX_EPOCH + Duration::from_secs(secs),
        }
    }

    #[test]
    fn universal_time_converts_from_1601() {
        assert_eq!(from_universal_time(116_444_736_000_000_000), UNIX_EPOCH);
        assert_eq!(
            from_universal_time(116_444_736_000_000_000 + 10_000_000),
            UNIX_EPOCH + Duration::from_secs(1)
        );
        assert_eq!(from_universal_time(0), UNIX_EPOCH);
    }

    #[test]
    fn activation_errors_mean_unsupported() {
        assert_eq!(history_error(REGDB_E_CLASSNOTREG), HistoryError::Unsupported);
        assert_eq!(history_error(E_NOINTERFACE), HistoryError::Unsupported);
        assert_eq!(history_error(E_NOTIMPL), HistoryError::Unsupported);
        assert_eq!(history_error(windows::Win32::Foundation::E_ACCESSDENIED), HistoryError::Failed);
        assert_eq!(history_error(windows::Win32::Foundation::E_FAIL), HistoryError::Failed);
    }

    #[test]
    fn only_text_without_files_is_listed() {
        assert!(is_text_item(true, false));
        assert!(!is_text_item(true, true));
        assert!(!is_text_item(false, false));
    }

    #[test]
    fn newest_first_sorts_and_keeps_50() {
        // 60 distinct times in a scrambled order (37 is coprime to 60).
        let clips: Vec<Clip> = (0..60u64).map(|i| clip(&i.to_string(), (i * 37) % 60)).collect();
        let out = newest_first(clips);
        assert_eq!(out.len(), MAX_ITEMS);
        assert!(out.windows(2).all(|w| w[0].copied >= w[1].copied));
        assert_eq!(out[0].copied, UNIX_EPOCH + Duration::from_secs(59));
    }

    #[test]
    fn newest_first_keeps_ties_in_order() {
        let out = newest_first(vec![clip("a", 5), clip("b", 5), clip("c", 9)]);
        let ids: Vec<&str> = out.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["c", "a", "b"]);
    }

    #[test]
    fn fake_history_gates_and_records() {
        let fake = Arc::new(FakeHistory::new());
        fake.set(Ok(History::Items(vec![clip("1", 1)])));
        fake.block();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = {
            let fake = fake.clone();
            std::thread::spawn(move || tx.send(fake.read()).unwrap())
        };
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "read waits while blocked"
        );
        fake.release();
        let got = rx.recv_timeout(Duration::from_secs(5)).expect("read returns once released");
        reader.join().unwrap();
        assert_eq!(got, Ok(History::Items(vec![clip("1", 1)])));
        assert_eq!(fake.reads(), 1);

        fake.set(Err(HistoryError::Unsupported));
        assert_eq!(fake.read(), Err(HistoryError::Unsupported));
        assert_eq!(fake.reads(), 2);

        assert_eq!(fake.set_content("1"), Ok(()));
        fake.mark_gone("2");
        assert_eq!(fake.set_content("2"), Err(SetError::Gone));
        assert_eq!(fake.set_calls(), ["1", "2"]);

        fake.forget();
        fake.forget();
        assert_eq!(fake.forgets(), 2);
    }

    #[test]
    fn one_unreadable_item_is_skipped() {
        // (clip, readable): "b" stands for an item whose text cannot be read.
        let items = vec![(clip("a", 1), true), (clip("b", 3), false), (clip("c", 2), true)];
        let (clips, held) = gather(items, |(c, readable)| readable.then(|| c.clone()));
        let ids: Vec<&str> = clips.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["c", "a"]);
        let mut held_ids: Vec<&String> = held.keys().collect();
        held_ids.sort();
        assert_eq!(held_ids, ["a", "c"]);
    }

    #[test]
    fn gather_holds_only_the_kept_items() {
        let items: Vec<Clip> = (0..60u64).map(|i| clip(&i.to_string(), i)).collect();
        let (clips, held) = gather(items, |c| Some(c.clone()));
        assert_eq!(clips.len(), MAX_ITEMS);
        assert_eq!(held.len(), MAX_ITEMS);
        assert!(clips.iter().all(|c| held.contains_key(&c.id)));
        assert!(!held.contains_key("0"), "the oldest are dropped");
    }

    fn map(ids: &[&str]) -> HashMap<String, String> {
        ids.iter().map(|id| (id.to_string(), format!("item {id}"))).collect()
    }

    #[test]
    fn a_late_read_never_restores_forgotten_items() {
        let held = Held::new();
        let generation = held.begin();
        held.forget();
        assert!(!held.store_if(generation, map(&["1"])));
        assert_eq!(held.get("1"), None);
    }

    #[test]
    fn an_older_read_never_overwrites_a_newer_one() {
        let held = Held::new();
        let older = held.begin();
        let newer = held.begin();
        assert!(held.store_if(newer, map(&["new"])));
        assert!(!held.store_if(older, map(&["old"])));
        assert_eq!(held.get("new"), Some("item new".to_string()));
        assert_eq!(held.get("old"), None);
    }

    #[test]
    fn forgotten_items_are_released_off_the_calling_thread() {
        /// Reports the thread it is dropped on.
        #[derive(Clone)]
        struct Item(std::sync::mpsc::Sender<std::thread::ThreadId>);
        impl Drop for Item {
            fn drop(&mut self) {
                let _ = self.0.send(std::thread::current().id());
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let held = Held::new();
        let generation = held.begin();
        held.store_if(generation, HashMap::from([("1".to_string(), Item(tx))]));
        held.forget();
        assert!(held.get("1").is_none());
        let dropped_on = rx.recv_timeout(Duration::from_secs(1)).expect("the item is released");
        assert_ne!(dropped_on, std::thread::current().id());
    }

    #[test]
    fn a_failed_read_clears_the_held_items() {
        let held = Held::new();
        let first = held.begin();
        held.store_if(first, map(&["1"]));
        let second = held.begin();
        assert_eq!(held.get("1"), Some("item 1".to_string()), "kept while a read runs");
        assert!(held.store_if(second, HashMap::new()));
        assert_eq!(held.get("1"), None);
    }

    /// Hand test: reads on one thread, lets it exit, then sets the newest
    /// item (already the clipboard's content, so nothing visibly changes) from
    /// another thread. Prints only result variants, never text. Skips when
    /// the newest history item is not text. Run with
    /// `cargo test --lib real_set_content_across_threads -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_set_content_across_threads() {
        let history = Arc::new(WinRtHistory::new());
        let reader = history.clone();
        let read = std::thread::spawn(move || reader.read()).join().unwrap();
        let clips = match read {
            Ok(History::Items(clips)) if !clips.is_empty() => clips,
            Ok(History::Items(_)) => return println!("Items(0): nothing to set"),
            Ok(History::Off) => return println!("Off"),
            Err(e) => return println!("{e:?}"),
        };
        let newest_any = in_sta(|| {
            let result = Clipboard::GetHistoryItemsAsync().ok()?.get().ok()?;
            let items = result.Items().ok()?;
            items.into_iter().filter_map(|i| i.Timestamp().ok()).map(|t| t.UniversalTime).max()
        });
        if newest_any.map(from_universal_time) != Some(clips[0].copied) {
            return println!("skipped: the newest history item is not text");
        }
        let id = clips[0].id.clone();
        let setter = history.clone();
        let got = std::thread::spawn(move || setter.set_content(&id)).join().unwrap();
        println!("{got:?}");
        assert_eq!(got, Ok(()));
        history.forget();
    }

    /// Hand test: reads the real history and prints only its shape (never
    /// any text). Run with
    /// `cargo test --lib real_history_reads -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_history_reads() {
        let history = WinRtHistory::new();
        match history.read() {
            Ok(History::Off) => println!("Off"),
            Ok(History::Items(items)) => println!("Items({})", items.len()),
            Err(e) => println!("{e:?}"),
        }
        history.forget();
    }
}
