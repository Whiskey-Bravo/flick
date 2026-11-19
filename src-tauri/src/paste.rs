//! Pasting into the window that was in front before the bar opened.
//!
//! Safety first: when there is any doubt (no remembered window, it is gone,
//! it is elevated, it did not come to the front) nothing is pasted and the
//! caller only copies. Every decision lives in `target_of` and `paste_into`;
//! the Win32 part is thin.

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
    /// Opens the real Start menu ([`crate::controls::open_start_menu`]).
    fn open_start_menu(&self) -> Result<(), String>;
    /// Windows' clipboard sequence number: it changes whenever the
    /// clipboard's content does.
    fn clipboard_sequence(&self) -> u32;
}

/// Reads the clipboard sequence number (real one: [`clipboard_sequence`]).
pub type Sequence = std::sync::Arc<dyn Fn() -> u32 + Send + Sync>;

/// The real clipboard sequence number. Never use in tests.
pub fn clipboard_sequence() -> u32 {
    // SAFETY: no arguments; reads a per-window-station counter.
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pasted {
    /// The keys went to the window in front.
    Done,
    NoTarget,
    Gone,
    Blocked,
    NotInFront,
    KeysFailed,
    /// The clipboard did not take the item (or kept changing) in time.
    ClipboardUnchanged,
    /// The clipboard changed again after it took the item.
    ClipboardChanged,
    /// The bar was shown again meanwhile.
    Superseded,
}

pub const PASTE_WAIT: Duration = Duration::from_millis(500);
pub const POLL: Duration = Duration::from_millis(10);

/// Paste into `target` only when it is certain to receive the keys and the
/// clipboard holds what was just put on it. `before` is the clipboard
/// sequence number read before the item was set: the paste waits until the
/// number has moved on and holds still for one poll, and sends the keys only
/// if it is still that number right before them. `still_current` is false
/// once the bar has been shown again. Everything shares one `wait` budget;
/// on any doubt nothing is sent and the item stays copied.
pub fn paste_into(
    target: Option<isize>,
    d: &dyn Desktop,
    before: u32,
    still_current: &dyn Fn() -> bool,
    wait: Duration,
    poll: Duration,
) -> Pasted {
    let Some(t) = target else {
        return Pasted::NoTarget;
    };
    if !still_current() {
        return Pasted::Superseded;
    }
    if !d.exists(t) {
        return Pasted::Gone;
    }
    if d.blocks_input(t) {
        return Pasted::Blocked;
    }
    let start = Instant::now();
    // The item reached the clipboard and settled there.
    let mut last = None;
    let taken = loop {
        if !still_current() {
            return Pasted::Superseded;
        }
        let now = d.clipboard_sequence();
        if now != before && last == Some(now) {
            break now;
        }
        last = Some(now);
        if start.elapsed() >= wait {
            return Pasted::ClipboardUnchanged;
        }
        std::thread::sleep(poll);
    };
    d.bring_to_front(t);
    loop {
        if !still_current() {
            return Pasted::Superseded;
        }
        if d.foreground() == Some(t) {
            break;
        }
        if start.elapsed() >= wait {
            return Pasted::NotInFront;
        }
        std::thread::sleep(poll);
    }
    // Right before the keys.
    if !still_current() {
        return Pasted::Superseded;
    }
    if d.clipboard_sequence() != taken {
        return Pasted::ClipboardChanged;
    }
    match d.send_ctrl_v() {
        Ok(()) => Pasted::Done,
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
    fn open_start_menu(&self) -> Result<(), String> {
        crate::controls::open_start_menu()
    }
    fn clipboard_sequence(&self) -> u32 {
        clipboard_sequence()
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
        // SAFETY: a valid slice of fully initialised INPUTs.
        chord(modifier, key, &mut |inputs| unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) })
    }

    /// The chord through `send` (which returns how many events went in).
    /// When not all went in, the key and the modifier are released (best
    /// effort), so Ctrl is never left held down.
    pub fn chord(modifier: VIRTUAL_KEY, key: VIRTUAL_KEY, send: &mut dyn FnMut(&[INPUT]) -> u32) -> Result<(), String> {
        let inputs = [
            key_input(modifier, false),
            key_input(key, false),
            key_input(key, true),
            key_input(modifier, true),
        ];
        if send(&inputs) == inputs.len() as u32 {
            return Ok(());
        }
        let _ = send(&[key_input(key, true), key_input(modifier, true)]);
        Err("could not send the keys".to_string())
    }
}

#[cfg(test)]
pub struct FakeDesktop {
    pub calls: Mutex<Vec<String>>,
    foreground: Mutex<std::collections::VecDeque<Option<isize>>>,
    sequence: Mutex<std::collections::VecDeque<u32>>,
    pub exists: bool,
    pub blocked: bool,
    pub keys_ok: bool,
}

#[cfg(test)]
impl FakeDesktop {
    /// `foreground` is the answer to successive `foreground()` calls; the last
    /// one repeats forever. The clipboard sequence number is 1 throughout
    /// (tests read 0 before the item was set) unless `sequence` scripts it.
    pub fn new(foreground: Vec<Option<isize>>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            foreground: Mutex::new(foreground.into()),
            sequence: Mutex::new(vec![1].into()),
            exists: true,
            blocked: false,
            keys_ok: true,
        }
    }
    /// Answers to successive `clipboard_sequence()` calls; the last repeats.
    pub fn sequence(self, sequence: Vec<u32>) -> Self {
        *self.sequence.lock().unwrap() = sequence.into();
        self
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
    fn open_start_menu(&self) -> Result<(), String> {
        self.log("start_menu");
        Ok(())
    }
    fn clipboard_sequence(&self) -> u32 {
        self.log("sequence");
        let mut q = self.sequence.lock().unwrap();
        if q.len() > 1 {
            q.pop_front().unwrap()
        } else {
            q.front().copied().unwrap_or(0)
        }
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

    fn current() -> bool {
        true
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
        assert_eq!(paste_into(None, &d, 0, &current, WAIT, TICK), Pasted::NoTarget);
        assert!(no_input(&d));

        let mut d = FakeDesktop::new(vec![Some(1)]);
        d.exists = false;
        assert_eq!(paste_into(Some(1), &d, 0, &current, WAIT, TICK), Pasted::Gone);
        assert!(no_input(&d));
    }

    #[test]
    fn an_elevated_window_pastes_nothing() {
        let mut d = FakeDesktop::new(vec![Some(1)]);
        d.blocked = true;
        assert_eq!(paste_into(Some(1), &d, 0, &current, WAIT, TICK), Pasted::Blocked);
        assert!(no_input(&d));
    }

    #[test]
    fn pastes_only_once_the_target_is_in_front() {
        let d = FakeDesktop::new(vec![Some(2)]);
        let start = Instant::now();
        assert_eq!(paste_into(Some(1), &d, 0, &current, WAIT, TICK), Pasted::NotInFront);
        assert!(start.elapsed() >= WAIT);
        assert!(d.calls().iter().all(|c| c != "ctrl_v"));

        let d = FakeDesktop::new(vec![Some(2), Some(2), Some(1)]);
        assert_eq!(paste_into(Some(1), &d, 0, &current, WAIT, TICK), Pasted::Done);
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
        assert_eq!(paste_into(Some(1), &d, 0, &current, WAIT, TICK), Pasted::KeysFailed);
    }

    #[test]
    fn pastes_only_once_the_clipboard_took_the_item() {
        // The clipboard never moves on from the number read before the set.
        let d = FakeDesktop::new(vec![Some(1)]).sequence(vec![0]);
        let start = Instant::now();
        assert_eq!(paste_into(Some(1), &d, 0, &current, WAIT, TICK), Pasted::ClipboardUnchanged);
        assert!(start.elapsed() >= WAIT);
        assert!(no_input(&d), "{:?}", d.calls());

        // It moves on late, settles, then the paste goes ahead.
        let d = FakeDesktop::new(vec![Some(1)]).sequence(vec![0, 0, 5, 6, 6]);
        assert_eq!(paste_into(Some(1), &d, 0, &current, WAIT, TICK), Pasted::Done);
        let calls = d.calls();
        let front = calls.iter().position(|c| c.starts_with("bring_to_front")).unwrap();
        let settled = calls.iter().rposition(|c| c == "sequence").unwrap();
        let keys = calls.iter().position(|c| c == "ctrl_v").unwrap();
        assert!(calls[..front].iter().filter(|c| *c == "sequence").count() >= 5, "{calls:?}");
        assert!(settled < keys, "checked right before the keys: {calls:?}");

        // Still changing when the wait runs out.
        let seq: Vec<u32> = (1..10_000).collect();
        let d = FakeDesktop::new(vec![Some(1)]).sequence(seq);
        assert_eq!(paste_into(Some(1), &d, 0, &current, WAIT, TICK), Pasted::ClipboardUnchanged);
        assert!(no_input(&d));
    }

    #[test]
    fn a_clipboard_changed_again_before_the_keys_is_not_pasted() {
        // Settles on 3, the target comes to the front, then someone else copies.
        let d = FakeDesktop::new(vec![Some(2), Some(1)]).sequence(vec![3, 3, 4]);
        assert_eq!(paste_into(Some(1), &d, 0, &current, WAIT, TICK), Pasted::ClipboardChanged);
        assert!(d.calls().iter().all(|c| c != "ctrl_v"));
    }

    #[test]
    fn a_bar_shown_again_stops_the_paste() {
        let d = FakeDesktop::new(vec![Some(1)]);
        assert_eq!(paste_into(Some(1), &d, 0, &|| false, WAIT, TICK), Pasted::Superseded);
        assert!(no_input(&d));

        // Shown again while waiting for the target: checked on every poll
        // and right before the keys.
        for allowed in 0..7 {
            let d = FakeDesktop::new(vec![Some(2), Some(2), Some(1)]);
            let checks = std::sync::atomic::AtomicUsize::new(0);
            let still = || checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < allowed;
            assert_eq!(paste_into(Some(1), &d, 0, &still, WAIT, TICK), Pasted::Superseded, "{allowed}");
            assert!(d.calls().iter().all(|c| c != "ctrl_v"), "{allowed}");
        }
    }

    #[test]
    fn a_short_send_releases_the_key_and_the_modifier() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL};
        let v = VIRTUAL_KEY(0x56);
        let keys = |batch: &[INPUT]| -> Vec<(u16, bool)> {
            // SAFETY: every INPUT here is a keyboard input.
            batch.iter().map(|i| unsafe { (i.Anonymous.ki.wVk.0, i.Anonymous.ki.dwFlags.contains(KEYEVENTF_KEYUP)) }).collect()
        };
        let mut sent = Vec::new();
        let r = sys::chord(VK_CONTROL, v, &mut |batch| {
            sent.push(keys(batch));
            if sent.len() == 1 { 1 } else { batch.len() as u32 }
        });
        assert!(r.is_err());
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1], [(0x56, true), (VK_CONTROL.0, true)]);

        let mut sent = Vec::new();
        let r = sys::chord(VK_CONTROL, v, &mut |batch| {
            sent.push(keys(batch));
            batch.len() as u32
        });
        assert!(r.is_ok());
        assert_eq!(sent, [vec![(VK_CONTROL.0, false), (0x56, false), (0x56, true), (VK_CONTROL.0, true)]]);
    }
}
