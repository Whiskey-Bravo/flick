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
            self.again.store(true, Ordering::Release);
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || loop {
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
            break;
        });
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
}
