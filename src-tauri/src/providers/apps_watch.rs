//! Watches the Start-menu folders: a new shortcut triggers one refresh once
//! the changes have been quiet for a moment.

use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Changes must be this quiet before the refresh runs.
pub const QUIET: Duration = Duration::from_secs(2);

/// Turns a burst of events into one "now" signal, `quiet` after the last one.
pub struct Debounce {
    pub quiet: Duration,
    last: Option<Instant>,
}

impl Debounce {
    pub fn new(quiet: Duration) -> Self {
        Debounce { quiet, last: None }
    }

    /// Feed the time and whether an event arrived at it. `true` once, when
    /// `quiet` has passed since the last event.
    pub fn due(&mut self, now: Instant, event: bool) -> bool {
        if event {
            self.last = Some(now);
        }
        match self.last {
            Some(t) if now.saturating_duration_since(t) >= self.quiet && !event => {
                self.last = None;
                true
            }
            _ => false,
        }
    }
}

/// Watch the folders that exist (recursively) and call `request` once per
/// quiet burst of changes, on a thread of its own. `Err` when no folder can
/// be watched; the caller then relies on refreshing when the bar opens. The
/// thread ends when the returned watcher is dropped.
pub fn watch(
    roots: &[PathBuf],
    quiet: Duration,
    request: impl Fn() + Send + 'static,
) -> notify::Result<RecommendedWatcher> {
    let (tx, rx) = mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        // A watcher error means missed events; refreshing on show covers them.
        // Reads of a shortcut are not changes.
        if res.is_ok_and(|e| !matches!(e.kind, EventKind::Access(_))) {
            let _ = tx.send(());
        }
    })?;
    let mut watched = 0;
    for root in roots.iter().filter(|r| r.is_dir()) {
        if watcher.watch(root, RecursiveMode::Recursive).is_ok() {
            watched += 1;
        }
    }
    if watched == 0 {
        return Err(notify::Error::generic("no Start menu folder to watch"));
    }
    std::thread::spawn(move || {
        // Ends when the watcher (and with it `tx`) is dropped.
        let mut debounce = Debounce::new(quiet);
        let mut pending = false;
        loop {
            let event = if pending {
                match rx.recv_timeout(POLL) {
                    Ok(()) => true,
                    Err(RecvTimeoutError::Timeout) => false,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            } else {
                match rx.recv() {
                    Ok(()) => true,
                    Err(_) => return,
                }
            };
            pending |= event;
            if debounce.due(Instant::now(), event) {
                pending = false;
                // A panic in the refresh must not end the watching.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&request));
            }
        }
    });
    Ok(watcher)
}

/// How often a pending change is checked against the quiet time.
const POLL: Duration = Duration::from_millis(100);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn secs(n: f64) -> Duration {
        Duration::from_secs_f64(n)
    }

    #[test]
    fn no_event_is_never_due() {
        let mut d = Debounce::new(QUIET);
        let t0 = Instant::now();
        for s in [0.0, 1.0, 5.0, 60.0] {
            assert!(!d.due(t0 + secs(s), false));
        }
    }

    #[test]
    fn due_once_after_the_quiet_time() {
        let mut d = Debounce::new(QUIET);
        let t0 = Instant::now();
        assert!(!d.due(t0, true));
        assert!(!d.due(t0 + secs(1.0), false));
        assert!(d.due(t0 + secs(2.0), false));
        assert!(!d.due(t0 + secs(2.5), false));
        assert!(!d.due(t0 + secs(10.0), false));
    }

    #[test]
    fn a_later_event_pushes_the_time_back() {
        let mut d = Debounce::new(QUIET);
        let t0 = Instant::now();
        assert!(!d.due(t0, true));
        assert!(!d.due(t0 + secs(1.5), true));
        assert!(!d.due(t0 + secs(3.0), false));
        assert!(d.due(t0 + secs(3.5), false));
    }

    #[test]
    fn a_new_burst_after_a_refresh_is_due_again() {
        let mut d = Debounce::new(QUIET);
        let t0 = Instant::now();
        d.due(t0, true);
        assert!(d.due(t0 + secs(2.0), false));
        assert!(!d.due(t0 + secs(5.0), true));
        assert!(d.due(t0 + secs(7.0), false));
    }

    #[test]
    fn a_new_lnk_in_a_temp_folder_requests_exactly_one_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let count = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel::<()>();
        let c = count.clone();
        let _watcher = watch(&[missing, dir.path().to_path_buf()], Duration::from_millis(300), move || {
            c.fetch_add(1, Ordering::SeqCst);
            let _ = tx.send(());
        })
        .expect("a watcher on the folder that exists");
        let sub = dir.path().join("Some Vendor");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("x.lnk"), b"").unwrap();
        std::fs::write(dir.path().join("y.lnk"), b"").unwrap();
        rx.recv_timeout(Duration::from_secs(5)).expect("a refresh request within 5 s");
        assert!(rx.recv_timeout(Duration::from_secs(1)).is_err(), "only one request");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn no_folder_to_watch_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(watch(&[dir.path().join("nope")], QUIET, || {}).is_err());
        assert!(watch(&[], QUIET, || {}).is_err());
    }
}
