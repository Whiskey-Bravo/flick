//! Battery, CPU, memory, disk and IP answers: typed keywords show live values.
//!
//! The query path reads the store's cache only; sampling happens in the background.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub use crate::clipboard::Clipboard;
use crate::provider::{Modifier, Outcome, Provider, Query, ResultItem};
use crate::status::{StatusKind, StatusStore, StatusValue};

const NAME: &str = "status";
/// Like computed answers: the engine puts them above every other row.
const TOP_SCORE: u32 = 1000;
const PREFIX: &str = "status:";

pub struct StatusProvider {
    store: Arc<StatusStore>,
    clipboard: Arc<dyn Clipboard>,
    online: AtomicBool,
    /// Copy text of the rows of the latest query, by the `<n>` of their id.
    copies: Mutex<Vec<String>>,
}

fn kind_of(text: &str) -> Option<StatusKind> {
    Some(match text.trim().to_lowercase().as_str() {
        "battery" => StatusKind::Battery,
        "cpu" => StatusKind::Cpu,
        "ram" | "memory" => StatusKind::Ram,
        "disk" | "disks" | "storage" => StatusKind::Disk,
        "ip" | "my ip" | "ip address" => StatusKind::Ip,
        _ => return None,
    })
}

impl StatusProvider {
    pub fn new(store: Arc<StatusStore>, clipboard: Arc<dyn Clipboard>) -> Self {
        StatusProvider { store, clipboard, online: AtomicBool::new(true), copies: Mutex::new(Vec::new()) }
    }

    /// Follows the `online` config key: off means the public IP is never fetched.
    pub fn set_online(&self, on: bool) {
        self.online.store(on, Ordering::Relaxed);
    }

    fn subtitle(kind: StatusKind, title: &str) -> &'static str {
        match kind {
            StatusKind::Battery => "Battery",
            StatusKind::Cpu => "Processor",
            StatusKind::Ram => "Memory",
            StatusKind::Disk => "Disk",
            StatusKind::Ip if title.starts_with("Public IP") => "Seen from the internet",
            StatusKind::Ip => "Local IP",
        }
    }
}

impl Provider for StatusProvider {
    fn name(&self) -> &'static str {
        NAME
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let Some(kind) = kind_of(&q.text) else { return vec![] };
        let lines = match self.store.get(kind, self.online.load(Ordering::Relaxed)) {
            StatusValue::Pending => vec![("Checking…".to_string(), String::new())],
            StatusValue::Lines(l) => l,
        };
        let mut copies = self.copies.lock().unwrap();
        copies.clear();
        lines
            .into_iter()
            .enumerate()
            .map(|(n, (title, copy))| {
                let subtitle = Self::subtitle(kind, &title).to_string();
                copies.push(copy);
                ResultItem {
                    id: format!("{PREFIX}{n}"),
                    provider: NAME,
                    title,
                    subtitle,
                    score: TOP_SCORE,
                    icon: "builtin:status".into(),
                    top: true,
                    pinnable: false,
                    ..Default::default()
                }
            })
            .collect()
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        let n: usize = id.strip_prefix(PREFIX).and_then(|n| n.parse().ok()).ok_or("unknown item")?;
        let text = self.copies.lock().unwrap().get(n).cloned().ok_or("unknown item")?;
        if text.is_empty() {
            return Ok(Outcome::Keep);
        }
        self.clipboard.set_text(&text)?;
        Ok(Outcome::Hide)
    }

    /// Values change by the second: never learned, saved or pinned.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::FakeClipboard;
    use crate::net::FakeFetch;
    use crate::status::{Battery, CpuTimes, Disk, Memory, StatusSource};
    use std::sync::mpsc;
    use std::time::Duration;

    struct Src;
    impl StatusSource for Src {
        fn battery(&self) -> Option<Battery> {
            Some(Battery { percent: 82, charging: false, minutes_left: None })
        }
        fn memory(&self) -> Memory {
            Memory { used: 1 << 30, total: 2 << 30 }
        }
        fn disks(&self) -> Vec<Disk> {
            vec![]
        }
        fn cpu_times(&self) -> CpuTimes {
            CpuTimes { idle: 0, kernel: 0, user: 0 }
        }
        fn local_ips(&self) -> Vec<String> {
            vec![]
        }
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    fn setup() -> (StatusProvider, Arc<FakeClipboard>, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let store = StatusStore::new(
            Arc::new(Src),
            Arc::new(FakeFetch::offline()),
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
            Duration::ZERO,
        );
        let clipboard = Arc::new(FakeClipboard::default());
        (StatusProvider::new(store, clipboard.clone()), clipboard, rx)
    }

    #[test]
    fn status_rows_are_top_and_enter_copies() {
        let (p, clipboard, rx) = setup();
        let pending = p.query(&q("battery"));
        assert_eq!(pending[0].title, "Checking…");
        assert_eq!(p.activate(&pending[0].id, Modifier::None), Ok(Outcome::Keep));
        rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let rows = p.query(&q("  Battery "));
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!((r.title.as_str(), r.score, r.top, r.pinnable, r.provider), ("82%", 1000, true, false, "status"));
        assert_eq!(p.rank_key(&r.id), None);
        assert_eq!(p.activate(&r.id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(*clipboard.0.lock().unwrap(), vec!["82%".to_string()]);
    }

    #[test]
    fn unrelated_text_gives_nothing() {
        let (p, _c, _rx) = setup();
        for t in ["", "batteries", "ram usage", "ipconfig", "my"] {
            assert!(p.query(&q(t)).is_empty(), "{t}");
        }
        for t in ["memory", "storage", "my ip", "ip address", "CPU"] {
            assert_eq!(p.query(&q(t)).len(), 1, "{t}");
        }
    }
}
