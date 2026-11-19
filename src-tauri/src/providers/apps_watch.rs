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

/// What a watcher result means.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Signal {
    /// A read of a shortcut, not a change.
    Ignore,
    /// Something changed.
    Change,
    /// The watcher reported a failure: events may be missed. On Windows the
    /// backend does not report runtime failures (it can stop without telling);
    /// the full refresh every `FULL_AFTER` when the bar opens bounds that.
    Lost,
}

pub fn signal(res: &notify::Result<notify::Event>) -> Signal {
    match res {
        Err(_) => Signal::Lost,
        Ok(e) if matches!(e.kind, EventKind::Access(_)) => Signal::Ignore,
        Ok(_) => Signal::Change,
    }
}

/// What `signal` does: `Lost` calls `on_lost` (the caller stops relying on
/// the watcher) and, like `Change`, asks for a refresh (`true`), which
/// re-reads whatever was missed.
pub fn route(signal: Signal, on_lost: &dyn Fn()) -> bool {
    match signal {
        Signal::Ignore => false,
        Signal::Change => true,
        Signal::Lost => {
            on_lost();
            true
        }
    }
}

/// Watch the folders that exist now (recursively; one created later is not
/// watched) and call `request` once per quiet burst of changes, on a thread
/// of its own. A watcher error calls `on_lost` and also requests a refresh
/// (but on Windows the watcher can stop without any error).
/// `Err` when no folder can be watched; the caller then relies on refreshing
/// when the bar opens. The thread ends when the returned watcher is dropped.
pub fn watch(
    roots: &[PathBuf],
    quiet: Duration,
    request: impl Fn() + Send + 'static,
    on_lost: impl Fn() + Send + 'static,
) -> notify::Result<RecommendedWatcher> {
    let (tx, rx) = mpsc::channel::<Signal>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let s = signal(&res);
        if s != Signal::Ignore {
            let _ = tx.send(s);
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
                    Ok(s) => route(s, &on_lost),
                    Err(RecvTimeoutError::Timeout) => false,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            } else {
                match rx.recv() {
                    Ok(s) => route(s, &on_lost),
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
        let _watcher = watch(&[missing, dir.path().to_path_buf()], Duration::from_millis(500), move || {
            c.fetch_add(1, Ordering::SeqCst);
            let _ = tx.send(());
        }, || {})
        .expect("a watcher on the folder that exists");
        let sub = dir.path().join("Some Vendor");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("x.lnk"), b"").unwrap();
        std::fs::write(dir.path().join("y.lnk"), b"").unwrap();
        rx.recv_timeout(Duration::from_secs(8)).expect("a refresh request in time");
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_err(), "only one request");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn no_folder_to_watch_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(watch(&[dir.path().join("nope")], QUIET, || {}, || {}).is_err());
        assert!(watch(&[], QUIET, || {}, || {}).is_err());
    }

    #[test]
    fn an_error_requests_a_refresh_and_stops_the_watching_flag() {
        use std::sync::atomic::AtomicBool;
        let watching = AtomicBool::new(true);
        let lost = || watching.store(false, Ordering::SeqCst);
        assert!(!route(Signal::Ignore, &lost));
        assert!(route(Signal::Change, &lost));
        assert!(watching.load(Ordering::SeqCst), "ordinary events leave watching on");
        assert!(route(Signal::Lost, &lost));
        assert!(!watching.load(Ordering::SeqCst));
    }

    #[test]
    fn results_map_to_signals() {
        let err: notify::Result<notify::Event> = Err(notify::Error::generic("gone"));
        assert_eq!(signal(&err), Signal::Lost);
        let ev = |kind| Ok(notify::Event::new(kind));
        assert_eq!(signal(&ev(EventKind::Access(notify::event::AccessKind::Any))), Signal::Ignore);
        assert_eq!(signal(&ev(EventKind::Create(notify::event::CreateKind::File))), Signal::Change);
    }
}
