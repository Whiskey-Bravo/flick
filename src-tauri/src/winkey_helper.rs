//! The Win-key hook runs in a helper process: flick.exe started again with
//! [`HELPER_ARG`]. Windows skips a process's own low-level keyboard hook for
//! physical keys while one of that process's windows has keyboard focus (seen
//! with Flick's bar focused), so a second Win tap would reach Windows and open
//! Search. A helper never has focus, so its hook sees every key.
//!
//! The two talk in lines over the helper's stdin and stdout:
//! - Flick to helper: `take 1` / `take 0`, whenever whether a lone press may
//!   open Flick changes. The helper ends when its stdin closes, so it never
//!   outlives Flick.
//! - Helper to Flick: `ready` or `unavailable <reason>` once, then `lone` for
//!   each taken press and `healthy 1` / `healthy 0` when that changes.
//!
//! Tests cover only the line format: they never start the helper.

use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::winkey::WinKeyHook;

/// The argument that starts flick.exe as the helper.
pub const HELPER_ARG: &str = "--win-key-helper";

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// How often Flick checks whether the take state changed.
const TAKE_POLL: Duration = Duration::from_millis(50);
/// How often the helper checks its hook's health.
const HEALTH_POLL: Duration = Duration::from_millis(500);
/// The longest wait for the helper's first line (installing can retry for 3 s).
const READY_WAIT: Duration = Duration::from_secs(10);
/// The longest wait for the helper to end once its stdin is closed.
const EXIT_WAIT: Duration = Duration::from_secs(2);

const UNAVAILABLE: &str = "Windows wouldn't let Flick watch the Win key.";

/// A line from the helper.
#[derive(Debug, PartialEq, Eq)]
enum Report {
    Ready,
    Unavailable(String),
    Lone,
    Healthy(bool),
}

fn report_line(r: &Report) -> String {
    match r {
        Report::Ready => "ready".into(),
        Report::Unavailable(reason) => format!("unavailable {}", reason.replace(['\r', '\n'], " ")),
        Report::Lone => "lone".into(),
        Report::Healthy(h) => format!("healthy {}", u8::from(*h)),
    }
}

fn parse_report(line: &str) -> Option<Report> {
    let line = line.trim_end_matches(['\r', '\n']);
    match line {
        "ready" => Some(Report::Ready),
        "lone" => Some(Report::Lone),
        "healthy 1" => Some(Report::Healthy(true)),
        "healthy 0" => Some(Report::Healthy(false)),
        _ => line.strip_prefix("unavailable ").map(|r| Report::Unavailable(r.to_string())),
    }
}

fn take_line(take: bool) -> &'static str {
    if take {
        "take 1"
    } else {
        "take 0"
    }
}

fn parse_take(line: &str) -> Option<bool> {
    match line.trim_end_matches(['\r', '\n']) {
        "take 1" => Some(true),
        "take 0" => Some(false),
        _ => None,
    }
}

/// The running helper, as Flick sees it. Stopping it (or dropping it) closes
/// the helper's stdin, so it removes its hooks and ends.
pub struct WinKeyHelper {
    child: Child,
    stop: Arc<AtomicBool>,
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    healthy: Arc<AtomicBool>,
}

impl WinKeyHelper {
    /// Starts the helper and waits until its hook is installed (up to 10 s).
    ///
    /// `may_take` is polled every 50 ms on a thread of its own and must return
    /// at once. `on_lone` runs on the thread reading the helper; it must not
    /// wait on the thread that calls [`WinKeyHelper::stop`].
    pub fn start(
        may_take: Box<dyn Fn() -> bool + Send + Sync>,
        on_lone: Box<dyn Fn() + Send + Sync>,
    ) -> Result<WinKeyHelper, String> {
        let exe = std::env::current_exe().map_err(|_| UNAVAILABLE.to_string())?;
        let mut child = Command::new(exe)
            .arg(HELPER_ARG)
            .arg(std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|_| UNAVAILABLE.to_string())?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(UNAVAILABLE.to_string());
        };

        let stop = Arc::new(AtomicBool::new(false));
        let writer_stop = stop.clone();
        let writer = std::thread::Builder::new()
            .name("flick-winkey-take".into())
            .spawn(move || write_take(stdin, may_take, writer_stop));
        let writer = match writer {
            Ok(w) => w,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(UNAVAILABLE.to_string());
            }
        };

        let healthy = Arc::new(AtomicBool::new(true));
        let reader_healthy = healthy.clone();
        let (first_tx, first_rx) = mpsc::channel::<Report>();
        let reader = std::thread::Builder::new()
            .name("flick-winkey-lone".into())
            .spawn(move || read_reports(stdout, first_tx, on_lone, reader_healthy));

        let mut helper = WinKeyHelper { child, stop, writer: Some(writer), reader: reader.ok(), healthy };
        if helper.reader.is_none() {
            return Err(UNAVAILABLE.to_string());
        }
        match first_rx.recv_timeout(READY_WAIT) {
            Ok(Report::Ready) => Ok(helper),
            Ok(Report::Unavailable(reason)) => Err(reason),
            _ => {
                let _ = helper.child.kill();
                Err(UNAVAILABLE.to_string())
            }
        }
    }

    /// Ends the helper (the same as dropping it).
    pub fn stop(self) {}

    /// False once the helper's hook couldn't be reinstalled, or the helper ended.
    pub fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }
}

impl Drop for WinKeyHelper {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let me = std::thread::current().id();
        // The writer drops the helper's stdin as it ends: the helper's signal to quit.
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
        let deadline = Instant::now() + EXIT_WAIT;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        // The helper is gone, so its stdout is closed and the reader ends.
        if let Some(r) = self.reader.take() {
            if r.thread().id() != me {
                let _ = r.join();
            }
        }
    }
}

/// Flick's side: sends the take state whenever it changes, until told to stop
/// or the helper is gone.
fn write_take(mut stdin: ChildStdin, may_take: Box<dyn Fn() -> bool + Send + Sync>, stop: Arc<AtomicBool>) {
    let mut sent: Option<bool> = None;
    while !stop.load(Ordering::SeqCst) {
        let take = catch_unwind(AssertUnwindSafe(|| may_take())).unwrap_or(false);
        if sent != Some(take) {
            if writeln!(stdin, "{}", take_line(take)).and_then(|()| stdin.flush()).is_err() {
                return;
            }
            sent = Some(take);
        }
        std::thread::sleep(TAKE_POLL);
    }
}

/// Flick's side: hands the first line to `start`, then acts on the rest.
fn read_reports(
    stdout: std::process::ChildStdout,
    first: mpsc::Sender<Report>,
    on_lone: Box<dyn Fn() + Send + Sync>,
    healthy: Arc<AtomicBool>,
) {
    let mut first = Some(first);
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let Some(report) = parse_report(&line) else { continue };
        if let Some(tx) = first.take() {
            let ready = report == Report::Ready;
            let _ = tx.send(report);
            if !ready {
                return;
            }
            continue;
        }
        match report {
            Report::Lone => {
                let _ = catch_unwind(AssertUnwindSafe(|| on_lone()));
            }
            Report::Healthy(h) => healthy.store(h, Ordering::Relaxed),
            Report::Ready | Report::Unavailable(_) => {}
        }
    }
    // The helper ended: there's no hook any more.
    healthy.store(false, Ordering::Relaxed);
}

/// The helper's side: runs the hook until Flick closes our stdin. `parent` is
/// Flick's process id, allowed to take the foreground when a press opens it.
pub fn helper_main(parent: u32) {
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let send = {
        let out = out.clone();
        move |r: Report| {
            let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
            if writeln!(o, "{}", report_line(&r)).and_then(|()| o.flush()).is_err() {
                // Flick is gone.
                std::process::exit(0);
            }
        }
    };

    let take = Arc::new(AtomicBool::new(false));
    let stdin_take = take.clone();
    let _ = std::thread::Builder::new().name("flick-helper-stdin".into()).spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if let Some(t) = parse_take(&line) {
                stdin_take.store(t, Ordering::SeqCst);
            }
        }
        // Flick closed our stdin or ended: Windows removes the hooks as we exit.
        std::process::exit(0);
    });

    let may_take: Box<dyn Fn() -> bool + Send + Sync> = Box::new(move || take.load(Ordering::SeqCst));
    let lone_send = send.clone();
    let on_lone: Box<dyn Fn() + Send + Sync> = Box::new(move || {
        // SAFETY: a plain Win32 call; failing only means Flick falls back to
        // its own way of taking the foreground.
        let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(parent) };
        lone_send(Report::Lone);
    });
    let hook = match WinKeyHook::start(may_take, on_lone) {
        Ok(hook) => hook,
        Err(reason) => {
            send(Report::Unavailable(reason));
            return;
        }
    };
    send(Report::Ready);
    let mut told = true;
    loop {
        std::thread::sleep(HEALTH_POLL);
        let h = hook.healthy();
        if h != told {
            told = h;
            send(Report::Healthy(h));
        }
    }
}

/// The helper's parent id when this process was started as the helper.
pub fn helper_parent(args: &[String]) -> Option<u32> {
    let at = args.iter().position(|a| a == HELPER_ARG)?;
    args.get(at + 1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_round_trip() {
        for r in [Report::Ready, Report::Lone, Report::Healthy(true), Report::Healthy(false), Report::Unavailable("no hook".into())] {
            assert_eq!(parse_report(&report_line(&r)), Some(r));
        }
    }

    #[test]
    fn a_reason_stays_on_one_line() {
        let line = report_line(&Report::Unavailable("a\r\nb".into()));
        assert!(!line.contains('\n') && !line.contains('\r'));
        assert_eq!(parse_report(&line), Some(Report::Unavailable("a  b".into())));
    }

    #[test]
    fn unknown_lines_are_ignored() {
        assert_eq!(parse_report("hello"), None);
        assert_eq!(parse_report(""), None);
        assert_eq!(parse_take("take 2"), None);
    }

    #[test]
    fn take_lines_round_trip_with_windows_line_ends() {
        assert_eq!(parse_take(&format!("{}\r\n", take_line(true))), Some(true));
        assert_eq!(parse_take(take_line(false)), Some(false));
    }

    #[test]
    fn only_the_helper_argument_with_an_id_starts_the_helper() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(helper_parent(&args(&["flick.exe", HELPER_ARG, "42"])), Some(42));
        assert_eq!(helper_parent(&args(&["flick.exe"])), None);
        assert_eq!(helper_parent(&args(&["flick.exe", HELPER_ARG])), None);
        assert_eq!(helper_parent(&args(&["flick.exe", HELPER_ARG, "x"])), None);
    }
}
