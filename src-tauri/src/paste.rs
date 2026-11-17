//! Pasting into the window that was in front before the bar opened.
//!
//! Safety first: when there is any doubt (no remembered window, it is gone,
//! it is elevated, it did not come to the front) nothing is pasted and the
//! caller only copies. Every decision lives in `target_of` and `paste_into`;
//! the Win32 part is thin.

// Not called until the window-show wiring lands (Part 9, Task 8), which removes this.
#![allow(dead_code)]

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::winlist::EXCLUDED_CLASSES;

#[derive(Clone, Debug, PartialEq)]
pub struct WindowFacts {
    pub hwnd: isize,
    pub pid: u32,
    pub class: String,
}

/// The window a paste may go to: never none, never Flick's own process (bar
/// or Settings window), never the desktop or the taskbar.
pub fn target_of(fg: Option<WindowFacts>, own_pid: u32) -> Option<isize> {
    let f = fg?;
    if f.hwnd == 0 || f.pid == own_pid {
        return None;
    }
    if EXCLUDED_CLASSES.iter().any(|c| c.eq_ignore_ascii_case(&f.class)) {
        return None;
    }
    Some(f.hwnd)
}

static PREVIOUS: Mutex<Option<isize>> = Mutex::new(None);

/// Remember the foreground window (call before the bar takes focus). Stores
/// `None` when it is not a safe target, so an old target never lingers.
pub fn remember_foreground() {
    let t = target_of(sys::foreground_facts(), std::process::id());
    *PREVIOUS.lock().unwrap_or_else(|e| e.into_inner()) = t;
}

pub fn previous() -> Option<isize> {
    *PREVIOUS.lock().unwrap_or_else(|e| e.into_inner())
}

pub trait Desktop: Send + Sync {
    fn foreground(&self) -> Option<isize>;
    fn exists(&self, hwnd: isize) -> bool;
    /// Windows refuses Flick's keys (higher integrity).
    fn blocks_input(&self, hwnd: isize) -> bool;
    fn bring_to_front(&self, hwnd: isize) -> bool;
    fn send_ctrl_v(&self) -> Result<(), String>;
    fn send_win_v(&self) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pasted {
    Pasted,
    NoTarget,
    Gone,
    Blocked,
    NotInFront,
    KeysFailed,
}

pub const PASTE_WAIT: Duration = Duration::from_millis(500);
pub const POLL: Duration = Duration::from_millis(10);

/// Paste into `target` only when it is certain to receive the keys.
pub fn paste_into(target: Option<isize>, d: &dyn Desktop, wait: Duration, poll: Duration) -> Pasted {
    let Some(t) = target else {
        return Pasted::NoTarget;
    };
    if !d.exists(t) {
        return Pasted::Gone;
    }
    if d.blocks_input(t) {
        return Pasted::Blocked;
    }
    d.bring_to_front(t);
    let start = Instant::now();
    loop {
        if d.foreground() == Some(t) {
            break;
        }
        if start.elapsed() >= wait {
            return Pasted::NotInFront;
        }
        std::thread::sleep(poll);
    }
    match d.send_ctrl_v() {
        Ok(()) => Pasted::Pasted,
        Err(_) => Pasted::KeysFailed,
    }
}

/// The real thing. Never use in tests.
pub struct SystemDesktop {
    windows: crate::winlist::SystemWindows,
}

impl SystemDesktop {
    pub fn new() -> Self {
        Self { windows: crate::winlist::SystemWindows::new() }
    }
}

impl Desktop for SystemDesktop {
    fn foreground(&self) -> Option<isize> {
        sys::foreground_facts().map(|f| f.hwnd)
    }
    fn exists(&self, hwnd: isize) -> bool {
        use crate::winlist::WindowOps;
        self.windows.exists(hwnd)
    }
    fn blocks_input(&self, hwnd: isize) -> bool {
        match sys::pid_of(hwnd) {
            Some(pid) => {
                crate::winlist::process_elevated(pid) && !crate::winlist::process_elevated(std::process::id())
            }
            // Unknown owner: do not risk it.
            None => true,
        }
    }
    fn bring_to_front(&self, hwnd: isize) -> bool {
        use crate::winlist::WindowOps;
        self.windows.focus(hwnd).is_ok()
    }
    fn send_ctrl_v(&self) -> Result<(), String> {
        use windows::Win32::UI::Input::KeyboardAndMouse::{VIRTUAL_KEY, VK_CONTROL};
        sys::send_chord(VK_CONTROL, VIRTUAL_KEY(0x56))
    }
    fn send_win_v(&self) -> Result<(), String> {
        use windows::Win32::UI::Input::KeyboardAndMouse::{VIRTUAL_KEY, VK_LWIN};
        sys::send_chord(VK_LWIN, VIRTUAL_KEY(0x56))
    }
}

mod sys {
    use super::WindowFacts;
    use crate::controls::key_input;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, VIRTUAL_KEY};
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId};

    pub fn pid_of(hwnd: isize) -> Option<u32> {
        let mut pid = 0u32;
        // SAFETY: the out pointer is valid; a stale handle just yields 0.
        unsafe { GetWindowThreadProcessId(HWND(hwnd as *mut _), Some(&mut pid)) };
        (pid != 0).then_some(pid)
    }

    pub fn foreground_facts() -> Option<WindowFacts> {
        // SAFETY: plain queries; a null handle means no foreground window.
        unsafe {
            let h = GetForegroundWindow();
            if h.0.is_null() {
                return None;
            }
            let hwnd = h.0 as isize;
            let pid = pid_of(hwnd)?;
            let mut buf = [0u16; 256];
            let n = GetClassNameW(h, &mut buf);
            let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            Some(WindowFacts { hwnd, pid, class })
        }
    }

    /// `modifier` down, `key` down, `key` up, `modifier` up.
    pub fn send_chord(modifier: VIRTUAL_KEY, key: VIRTUAL_KEY) -> Result<(), String> {
        let inputs = [
            key_input(modifier, false),
            key_input(key, false),
            key_input(key, true),
            key_input(modifier, true),
        ];
        // SAFETY: a valid slice of fully initialised INPUTs.
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent == inputs.len() as u32 {
            Ok(())
        } else {
            Err("could not send the keys".to_string())
        }
    }
}

#[cfg(test)]
pub struct FakeDesktop {
    pub calls: Mutex<Vec<String>>,
    foreground: Mutex<std::collections::VecDeque<Option<isize>>>,
    pub exists: bool,
    pub blocked: bool,
    pub keys_ok: bool,
}

#[cfg(test)]
impl FakeDesktop {
    /// `foreground` is the answer to successive `foreground()` calls; the last
    /// one repeats forever.
    pub fn new(foreground: Vec<Option<isize>>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            foreground: Mutex::new(foreground.into()),
            exists: true,
            blocked: false,
            keys_ok: true,
        }
    }
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn log(&self, s: &str) {
        self.calls.lock().unwrap().push(s.to_string());
    }
}

#[cfg(test)]
impl Desktop for FakeDesktop {
    fn foreground(&self) -> Option<isize> {
        self.log("foreground");
        let mut q = self.foreground.lock().unwrap();
        if q.len() > 1 {
            q.pop_front().unwrap()
        } else {
            q.front().copied().flatten()
        }
    }
    fn exists(&self, _hwnd: isize) -> bool {
        self.log("exists");
        self.exists
    }
    fn blocks_input(&self, _hwnd: isize) -> bool {
        self.log("blocks_input");
        self.blocked
    }
    fn bring_to_front(&self, hwnd: isize) -> bool {
        self.log(&format!("bring_to_front {hwnd}"));
        true
    }
    fn send_ctrl_v(&self) -> Result<(), String> {
        self.log("ctrl_v");
        if self.keys_ok {
            Ok(())
        } else {
            Err("no keys".into())
        }
    }
    fn send_win_v(&self) -> Result<(), String> {
        self.log("win_v");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: u32 = 100;
    const WAIT: Duration = Duration::from_millis(50);
    const TICK: Duration = Duration::from_millis(1);

    fn facts(hwnd: isize, pid: u32, class: &str) -> Option<WindowFacts> {
        Some(WindowFacts { hwnd, pid, class: class.to_string() })
    }

    fn no_input(d: &FakeDesktop) -> bool {
        d.calls().iter().all(|c| !c.starts_with("bring_to_front") && c != "ctrl_v" && c != "win_v")
    }

    #[test]
    fn flick_and_shell_windows_are_never_targets() {
        assert_eq!(target_of(facts(5, OWN, "Notepad"), OWN), None);
        for class in ["Shell_TrayWnd", "Progman", "WorkerW"] {
            assert_eq!(target_of(facts(5, 7, class), OWN), None, "{class}");
        }
        assert_eq!(target_of(facts(0, 7, "Notepad"), OWN), None);
        assert_eq!(target_of(None, OWN), None);
        assert_eq!(target_of(facts(5, 7, "Notepad"), OWN), Some(5));
    }

    #[test]
    fn no_target_or_a_gone_window_pastes_nothing() {
        let d = FakeDesktop::new(vec![Some(1)]);
        assert_eq!(paste_into(None, &d, WAIT, TICK), Pasted::NoTarget);
        assert!(no_input(&d));

        let mut d = FakeDesktop::new(vec![Some(1)]);
        d.exists = false;
        assert_eq!(paste_into(Some(1), &d, WAIT, TICK), Pasted::Gone);
        assert!(no_input(&d));
    }

    #[test]
    fn an_elevated_window_pastes_nothing() {
        let mut d = FakeDesktop::new(vec![Some(1)]);
        d.blocked = true;
        assert_eq!(paste_into(Some(1), &d, WAIT, TICK), Pasted::Blocked);
        assert!(no_input(&d));
    }

    #[test]
    fn pastes_only_once_the_target_is_in_front() {
        let d = FakeDesktop::new(vec![Some(2)]);
        let start = Instant::now();
        assert_eq!(paste_into(Some(1), &d, WAIT, TICK), Pasted::NotInFront);
        assert!(start.elapsed() >= WAIT);
        assert!(d.calls().iter().all(|c| c != "ctrl_v"));

        let d = FakeDesktop::new(vec![Some(2), Some(2), Some(1)]);
        assert_eq!(paste_into(Some(1), &d, WAIT, TICK), Pasted::Pasted);
        let calls = d.calls();
        assert_eq!(calls.iter().filter(|c| *c == "ctrl_v").count(), 1);
        assert_eq!(calls.iter().filter(|c| *c == "foreground").count(), 3);
        let front = calls.iter().position(|c| c.starts_with("bring_to_front")).unwrap();
        let keys = calls.iter().position(|c| c == "ctrl_v").unwrap();
        assert!(front < keys);
    }

    #[test]
    fn failed_keys_are_reported() {
        let mut d = FakeDesktop::new(vec![Some(1)]);
        d.keys_ok = false;
        assert_eq!(paste_into(Some(1), &d, WAIT, TICK), Pasted::KeysFailed);
    }
}
