//! Run a child process with no console window, a deadline and captured stdout.

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const POLL: Duration = Duration::from_millis(50);

/// Why [`run_hidden`] produced no output.
#[derive(Debug, PartialEq)]
pub enum RunError {
    /// The program could not be started (or its stdout could not be read).
    Spawn,
    /// It ran but exited non-zero.
    Exit,
    /// It had not finished within the timeout and was killed.
    Timeout,
}

/// Run `exe` with `args` (no window, stdin and stderr discarded) and return
/// its stdout once it exits successfully. A child still running at `timeout`
/// is killed.
pub fn run_hidden(exe: &Path, args: &[&str], timeout: Duration) -> Result<Vec<u8>, RunError> {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|_| RunError::Spawn)?;
    let deadline = Instant::now() + timeout;
    let stop = |child: &mut std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
    };
    // Drain stdout on a helper thread so a full pipe never stalls the child
    // while this thread waits on it with a deadline.
    let Some(mut stdout) = child.stdout.take() else {
        stop(&mut child);
        return Err(RunError::Spawn);
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let read = stdout.read_to_end(&mut buf);
        let _ = tx.send(read.ok().map(|_| buf));
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) => {
                stop(&mut child);
                return Err(RunError::Timeout);
            }
            Err(_) => {
                stop(&mut child);
                return Err(RunError::Spawn);
            }
        }
    };
    if !status.success() {
        return Err(RunError::Exit);
    }
    // The pipe closes when the child exits; the grace only covers a
    // descendant still holding it open.
    let wait = deadline.saturating_duration_since(Instant::now()).max(POLL);
    match rx.recv_timeout(wait) {
        Ok(Some(stdout)) => Ok(stdout),
        Ok(None) => Err(RunError::Spawn),
        Err(_) => Err(RunError::Timeout),
    }
}
