//! Windows' clipboard history (Win+V) behind a trait. The real implementation
//! talks to WinRT; tests use `testing::FakeHistory` and never construct
//! `WinRtHistory`. Nothing read here is stored or logged: items live in
//! memory until `forget`.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::core::HRESULT;
use windows::ApplicationModel::DataTransfer::{
    Clipboard, ClipboardHistoryItem, ClipboardHistoryItemsResultStatus, SetHistoryItemAsContentStatus,
    StandardDataFormats,
};
use windows::Win32::Foundation::{E_NOINTERFACE, E_NOTIMPL, REGDB_E_CLASSNOTREG};
use windows::Win32::System::Com::CoIncrementMTAUsage;

use crate::launch;

pub const MAX_ITEMS: usize = 50;

#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    /// Windows' item id.
    pub id: String,
    pub text: String,
    pub copied: SystemTime,
    /// The public API has no pinned flag; kept for when one is found.
    pub pinned: bool,
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

/// Held items outlive the `with_com` call that read them and may be released
/// on any thread. If the last apartment in the process closed in between,
/// COM would unload the WinRT server and that release would crash, so keep
/// the MTA alive for the life of the process (once; never decremented).
fn keep_com_loaded() {
    static ONCE: Once = Once::new();
    // SAFETY: plain process-wide COM usage count; the cookie is deliberately
    // never passed to CoDecrementMTAUsage.
    ONCE.call_once(|| {
        let _ = unsafe { CoIncrementMTAUsage() };
    });
}

/// The real history over WinRT `Clipboard`. Holds the items of the last read
/// by id, so `set_content` acts on exactly the item a row was built from.
pub struct WinRtHistory {
    held: Mutex<HashMap<String, ClipboardHistoryItem>>,
}

impl WinRtHistory {
    pub fn new() -> Self {
        keep_com_loaded();
        WinRtHistory { held: Mutex::new(HashMap::new()) }
    }

    fn read_items(&self) -> Result<History, HistoryError> {
        if !Clipboard::IsHistoryEnabled().map_err(winrt_error)? {
            self.forget();
            return Ok(History::Off);
        }
        let result = Clipboard::GetHistoryItemsAsync()
            .map_err(winrt_error)?
            .get()
            .map_err(winrt_error)?;
        let status = result.Status().map_err(winrt_error)?;
        if status == ClipboardHistoryItemsResultStatus::ClipboardHistoryDisabled {
            self.forget();
            return Ok(History::Off);
        }
        if status != ClipboardHistoryItemsResultStatus::Success {
            // AccessDenied, or a status this code does not know.
            return Err(HistoryError::Failed);
        }
        let text = StandardDataFormats::Text().map_err(winrt_error)?;
        let files = StandardDataFormats::StorageItems().map_err(winrt_error)?;
        let mut clips = Vec::new();
        let mut held = HashMap::new();
        for item in result.Items().map_err(winrt_error)? {
            let content = item.Content().map_err(winrt_error)?;
            let has_text = content.Contains(&text).map_err(winrt_error)?;
            let has_files = content.Contains(&files).map_err(winrt_error)?;
            if !is_text_item(has_text, has_files) {
                continue;
            }
            let body = content.GetTextAsync().map_err(winrt_error)?.get().map_err(winrt_error)?;
            let id = item.Id().map_err(winrt_error)?.to_string_lossy();
            let copied = from_universal_time(item.Timestamp().map_err(winrt_error)?.UniversalTime);
            clips.push(Clip {
                id: id.clone(),
                text: body.to_string_lossy(),
                copied,
                pinned: false,
            });
            held.insert(id, item);
        }
        let clips = newest_first(clips);
        held.retain(|id, _| clips.iter().any(|c| &c.id == id));
        *lock(&self.held) = held;
        Ok(History::Items(clips))
    }
}

impl Default for WinRtHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipHistory for WinRtHistory {
    fn read(&self) -> Result<History, HistoryError> {
        launch::with_com(|| self.read_items())
    }

    fn set_content(&self, id: &str) -> Result<(), SetError> {
        let item = lock(&self.held).get(id).cloned().ok_or(SetError::Gone)?;
        launch::with_com(|| match Clipboard::SetHistoryItemAsContent(&item) {
            Ok(s) if s == SetHistoryItemAsContentStatus::Success => Ok(()),
            Ok(s) if s == SetHistoryItemAsContentStatus::ItemDeleted => Err(SetError::Gone),
            _ => Err(SetError::Failed),
        })
    }

    fn forget(&self) {
        lock(&self.held).clear();
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
            pinned: false,
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
