//! Runs one background job at a time: a request that arrives while the job
//! runs does not start a second thread, it only asks for one more run after
//! the current one, so a refresh never overwrites a newer one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct Refresher {
    running: AtomicBool,
    again: AtomicBool,
    work: Box<dyn Fn() + Send + Sync>,
    /// Called after every completed run.
    done: Box<dyn Fn() + Send + Sync>,
}

impl Refresher {
    pub fn new(work: Box<dyn Fn() + Send + Sync>, done: Box<dyn Fn() + Send + Sync>) -> Arc<Self> {
        Arc::new(Refresher { running: AtomicBool::new(false), again: AtomicBool::new(false), work, done })
    }

    /// Never blocks. Starts a thread unless one is running; then it is
    /// told to run once more when it finishes.
    pub fn request(self: &Arc<Self>) {
        if self.running.swap(true, Ordering::AcqRel) {
            self.again.store(true, Ordering::SeqCst);
            // The run may have ended between the swap and the store above, having
            // missed `again`: check again, and start the run ourselves if it is over.
            if self.running.swap(true, Ordering::AcqRel) {
                return;
            }
        }
        let me = self.clone();
        std::thread::spawn(move || {
            // A panic in the job must not leave `running` stuck on.
            let mut guard = ClearOnDrop { me: &me, armed: true };
            loop {
                me.again.store(false, Ordering::Release);
                (me.work)();
                (me.done)();
                if me.again.swap(false, Ordering::AcqRel) {
                    continue;
                }
                me.running.store(false, Ordering::Release);
                // A request between the check and the store saw `running` and
                // only set `again`: take over unless another thread already did.
                if me.again.load(Ordering::Acquire) && !me.running.swap(true, Ordering::AcqRel) {
                    continue;
                }
                guard.armed = false;
                break;
            }
        });
    }
}

/// Clears `running` when the job thread unwinds.
struct ClearOnDrop<'a> {
    me: &'a Refresher,
    armed: bool,
}

impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.me.running.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;

    #[test]
    fn requests_during_a_run_coalesce_into_one_more_run() {
        let runs = Arc::new(AtomicUsize::new(0));
        let dones = Arc::new(AtomicUsize::new(0));
        let (started_tx, started) = mpsc::channel::<()>();
        let (gate_tx, gate) = mpsc::channel::<()>();
        let (started_tx, gate) = (Mutex::new(started_tx), Mutex::new(gate));
        let (r, d) = (runs.clone(), dones.clone());
        let refresher = Refresher::new(
            Box::new(move || {
                r.fetch_add(1, Ordering::SeqCst);
                started_tx.lock().unwrap().send(()).unwrap();
                gate.lock().unwrap().recv_timeout(Duration::from_secs(2)).unwrap();
            }),
            Box::new(move || {
                d.fetch_add(1, Ordering::SeqCst);
            }),
        );
        refresher.request();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..5 {
            refresher.request();
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "no second thread while one runs");
        gate_tx.send(()).unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        gate_tx.send(()).unwrap();
        while refresher.running.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        assert_eq!((runs.load(Ordering::SeqCst), dones.load(Ordering::SeqCst)), (2, 2));
        // Idle again: the next request runs at once.
        refresher.request();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        gate_tx.send(()).unwrap();
        while refresher.running.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        assert_eq!((runs.load(Ordering::SeqCst), dones.load(Ordering::SeqCst)), (3, 3));
    }

    fn wait_idle(r: &Refresher) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while r.running.load(Ordering::SeqCst) {
            assert!(std::time::Instant::now() < deadline, "refresher never went idle");
            std::thread::yield_now();
        }
    }

    #[test]
    fn the_last_request_always_gets_a_run() {
        use std::sync::atomic::AtomicU64;
        let asked = Arc::new(AtomicU64::new(0));
        let seen = Arc::new(AtomicU64::new(0));
        let (a, s) = (asked.clone(), seen.clone());
        let refresher = Refresher::new(
            Box::new(move || {
                // What had been asked for when this run began.
                s.store(a.load(Ordering::SeqCst), Ordering::SeqCst);
            }),
            Box::new(|| {}),
        );
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (r, a) = (refresher.clone(), asked.clone());
                std::thread::spawn(move || {
                    for _ in 0..3000 {
                        a.fetch_add(1, Ordering::SeqCst);
                        r.request();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        wait_idle(&refresher);
        assert_eq!(seen.load(Ordering::SeqCst), 12000, "a request was dropped");
    }

    #[test]
    fn a_panicking_job_does_not_wedge_the_refresher() {
        let runs = Arc::new(AtomicUsize::new(0));
        let r = runs.clone();
        let refresher = Refresher::new(
            Box::new(move || {
                if r.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("job failed (expected in this test)");
                }
            }),
            Box::new(|| {}),
        );
        refresher.request();
        wait_idle(&refresher);
        refresher.request();
        wait_idle(&refresher);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while runs.load(Ordering::SeqCst) < 2 {
            assert!(std::time::Instant::now() < deadline, "second run never started");
            std::thread::yield_now();
        }
    }
}
