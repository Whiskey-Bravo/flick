//! Online lookups that wait for typing to pause: one key waits at a time, one
//! worker thread runs its job, and answers are kept in memory only.
// Until lib.rs registers the words provider (Task 13).
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::net::Fetch;

/// Why a lookup has no answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookupError {
    /// No connection to the service.
    Offline,
    /// The service answered: there is nothing for this key.
    NotFound,
    /// Any other failure (odd answer, server error).
    Bad,
}

/// What [`Lookups::ask`] knows about a key right now.
#[derive(Debug)]
pub enum Lookup<T> {
    /// Asked; the answer comes later, announced through `on_ready`.
    Waiting,
    Ready(Arc<T>),
    Failed(LookupError),
}

/// How long typing must pause before a key is looked up.
pub const PAUSE: Duration = Duration::from_millis(400);
/// Answers kept in memory; the oldest goes first.
pub const CACHE_MAX: usize = 100;

type Job<T> = Box<dyn FnOnce(&dyn Fetch) -> Result<T, LookupError> + Send>;

struct Waiting<T> {
    key: String,
    since: Instant,
    job: Job<T>,
}

struct State<T> {
    /// Answers (`Ok` or `NotFound`), oldest first.
    cache: VecDeque<(String, Result<Arc<T>, LookupError>)>,
    /// The one key waiting for the pause to end.
    waiting: Option<Waiting<T>>,
    /// The key whose job is running.
    running: Option<String>,
    /// The latest failure that is not kept (`Offline`/`Bad`), told once.
    failed: Option<(String, LookupError)>,
    /// A worker thread is alive.
    worker: bool,
}

pub struct Lookups<T> {
    fetch: Arc<dyn Fetch>,
    on_ready: Arc<dyn Fn() + Send + Sync>,
    pause: Duration,
    state: Mutex<State<T>>,
    wake: Condvar,
}

impl<T: Send + Sync + 'static> Lookups<T> {
    pub fn new(fetch: Arc<dyn Fetch>, on_ready: Arc<dyn Fn() + Send + Sync>, pause: Duration) -> Arc<Self> {
        Arc::new(Lookups {
            fetch,
            on_ready,
            pause,
            state: Mutex::new(State { cache: VecDeque::new(), waiting: None, running: None, failed: None, worker: false }),
            wake: Condvar::new(),
        })
    }

    /// A kept answer at once; else `key` becomes the one waiting key and its
    /// `job` runs on the worker once `key` has waited the pause. Memory only.
    pub fn ask(
        self: &Arc<Self>,
        key: &str,
        job: impl FnOnce(&dyn Fetch) -> Result<T, LookupError> + Send + 'static,
    ) -> Lookup<T> {
        let mut st = self.state.lock().unwrap();
        if let Some((_, answer)) = st.cache.iter().find(|(k, _)| k == key) {
            return match answer {
                Ok(v) => Lookup::Ready(v.clone()),
                Err(e) => Lookup::Failed(*e),
            };
        }
        match st.failed.take() {
            Some((k, e)) if k == key => return Lookup::Failed(e),
            _ => {}
        }
        if st.running.as_deref() == Some(key) {
            st.waiting = None;
            self.wake.notify_all();
            return Lookup::Waiting;
        }
        if st.waiting.as_ref().is_some_and(|w| w.key == key) {
            return Lookup::Waiting;
        }
        st.waiting = Some(Waiting { key: key.to_string(), since: Instant::now(), job: Box::new(job) });
        self.wake.notify_all();
        if !st.worker {
            st.worker = true;
            let me = self.clone();
            std::thread::spawn(move || me.work());
        }
        Lookup::Waiting
    }

    /// Drops the waiting key (the bar hid or the text moved on): nothing is sent for it.
    pub fn forget_waiting(&self) {
        self.state.lock().unwrap().waiting = None;
        self.wake.notify_all();
    }

    /// The worker: waits out the pause of the waiting key, runs its job alone,
    /// keeps the answer, tells `on_ready`; ends when nothing waits.
    fn work(&self) {
        let mut st = self.state.lock().unwrap();
        loop {
            let Some(w) = &st.waiting else {
                st.worker = false;
                return;
            };
            let due = w.since + self.pause;
            let now = Instant::now();
            if now < due {
                st = self.wake.wait_timeout(st, due - now).unwrap().0;
                continue;
            }
            let Waiting { key, job, .. } = st.waiting.take().unwrap();
            st.running = Some(key.clone());
            drop(st);
            let fetch = &*self.fetch;
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(fetch)))
                .unwrap_or(Err(LookupError::Bad));
            st = self.state.lock().unwrap();
            st.running = None;
            match out {
                Ok(v) => st.cache.push_back((key, Ok(Arc::new(v)))),
                Err(LookupError::NotFound) => st.cache.push_back((key, Err(LookupError::NotFound))),
                Err(e) => st.failed = Some((key, e)),
            }
            while st.cache.len() > CACHE_MAX {
                st.cache.pop_front();
            }
            drop(st);
            (self.on_ready)();
            st = self.state.lock().unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{FakeFetch, FetchError};
    use std::sync::mpsc;

    const PAUSE_TEST: Duration = Duration::from_millis(100);
    const WAIT: Duration = Duration::from_secs(2);

    fn store(fetch: Arc<FakeFetch>, pause: Duration) -> (Arc<Lookups<String>>, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let on_ready: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = tx.lock().unwrap().send(());
        });
        (Lookups::new(fetch, on_ready, pause), rx)
    }

    /// A job that fetches `https://x/<key>` and answers its body as text.
    fn job(key: &str) -> impl FnOnce(&dyn Fetch) -> Result<String, LookupError> + Send + 'static {
        let url = format!("https://x/{key}");
        move |f: &dyn Fetch| match f.get(&url) {
            Ok(body) => Ok(String::from_utf8(body).unwrap()),
            Err(FetchError::Offline) => Err(LookupError::Offline),
            Err(FetchError::Status(404)) => Err(LookupError::NotFound),
            Err(_) => Err(LookupError::Bad),
        }
    }

    fn ask(s: &Arc<Lookups<String>>, key: &str) -> Lookup<String> {
        s.ask(key, job(key))
    }

    #[test]
    fn a_cached_answer_needs_no_new_request() {
        let fetch = Arc::new(FakeFetch::new(Ok(b"yes".to_vec())).respond("https://x/gone", Err(FetchError::Status(404))));
        let (s, rx) = store(fetch.clone(), PAUSE_TEST);
        assert!(matches!(ask(&s, "a"), Lookup::Waiting));
        assert!(matches!(ask(&s, "a"), Lookup::Waiting), "asking again keeps waiting");
        rx.recv_timeout(WAIT).unwrap();
        match ask(&s, "a") {
            Lookup::Ready(v) => assert_eq!(*v, "yes"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(ask(&s, "gone"), Lookup::Waiting));
        rx.recv_timeout(WAIT).unwrap();
        assert!(matches!(ask(&s, "gone"), Lookup::Failed(LookupError::NotFound)));
        assert!(matches!(ask(&s, "a"), Lookup::Ready(_)));
        std::thread::sleep(PAUSE_TEST * 3);
        assert_eq!(fetch.urls(), vec!["https://x/a".to_string(), "https://x/gone".to_string()]);
    }

    #[test]
    fn offline_is_not_cached() {
        let fetch = Arc::new(FakeFetch::offline());
        let (s, rx) = store(fetch.clone(), PAUSE_TEST);
        assert!(matches!(ask(&s, "a"), Lookup::Waiting));
        rx.recv_timeout(WAIT).unwrap();
        assert!(matches!(ask(&s, "a"), Lookup::Failed(LookupError::Offline)));
        assert!(matches!(ask(&s, "a"), Lookup::Waiting), "asking again fetches again");
        rx.recv_timeout(WAIT).unwrap();
        assert_eq!(fetch.urls().len(), 2);
    }

    #[test]
    fn forget_waiting_sends_nothing() {
        let fetch = Arc::new(FakeFetch::new(Ok(b"yes".to_vec())));
        let (s, rx) = store(fetch.clone(), PAUSE_TEST);
        assert!(matches!(ask(&s, "a"), Lookup::Waiting));
        s.forget_waiting();
        assert!(rx.recv_timeout(PAUSE_TEST * 3).is_err());
        assert!(fetch.urls().is_empty());
    }

    #[test]
    fn only_the_last_key_is_looked_up() {
        let fetch = Arc::new(FakeFetch::new(Ok(b"yes".to_vec())));
        let (s, rx) = store(fetch.clone(), PAUSE_TEST);
        for k in ["a", "ab", "abc"] {
            assert!(matches!(ask(&s, k), Lookup::Waiting));
            std::thread::sleep(Duration::from_millis(20));
        }
        rx.recv_timeout(WAIT).unwrap();
        std::thread::sleep(PAUSE_TEST * 2);
        assert_eq!(fetch.urls(), vec!["https://x/abc".to_string()]);
    }

    #[test]
    fn the_oldest_answer_is_evicted_past_the_limit() {
        let fetch = Arc::new(FakeFetch::new(Ok(b"yes".to_vec())));
        let (s, rx) = store(fetch.clone(), Duration::ZERO);
        for n in 0..=CACHE_MAX {
            let key = n.to_string();
            assert!(matches!(ask(&s, &key), Lookup::Waiting));
            rx.recv_timeout(WAIT).unwrap();
            assert!(matches!(ask(&s, &key), Lookup::Ready(_)));
        }
        assert!(matches!(ask(&s, "1"), Lookup::Ready(_)), "younger answers stay");
        assert!(matches!(ask(&s, "0"), Lookup::Waiting), "the oldest was dropped");
    }
}
