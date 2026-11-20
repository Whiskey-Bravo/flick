//! Keeps an auto-hidden taskbar up while the bar is open (the `show_taskbar`
//! setting).
//!
//! First the taskbar on the bar's monitor is asked to show itself, with the
//! message Explorer's own auto-hide code answers (`SHOW_AUTOHIDE`, not
//! documented; other launchers use it too). Closing the bar asks it to hide
//! again. If the taskbar is not up shortly after, a Windows update may have
//! changed that, and the documented way takes over: Windows shows a hidden
//! taskbar while one of its buttons asks for attention, so an invisible
//! window that never takes the focus gets a taskbar button, asks for
//! attention until the taskbar is up, and then stops asking. The button turns
//! into a plain one and the taskbar stays up for as long as the button
//! exists; closing the bar removes the window, and the taskbar hides again.
//! No Windows setting is changed.
//!
//! Clicking that button gives its window the focus, the bar loses it and
//! hides as it does for any click elsewhere, and the window goes with it.
//!
//! Everything here runs on the main thread. Tests cover only the pure parts:
//! they never create a window or ask the taskbar anything.

use std::cell::Cell;
use std::time::Instant;

use tauri::{AppHandle, Manager};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::Shell::{SHAppBarMessage, ABM_GETSTATE, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowExW, FindWindowW, FlashWindowEx, GetWindowRect, KillTimer,
    PostMessageW, RegisterClassW, SendMessageW, SetLayeredWindowAttributes, SetTimer, ShowWindow, FLASHWINFO, FLASHW_STOP,
    FLASHW_TRAY, ICON_BIG, ICON_SMALL, LWA_ALPHA, SW_SHOWNOACTIVATE, WM_GETICON, WM_SETICON, WM_TIMER, WNDCLASSW,
    WS_EX_APPWINDOW, WS_EX_LAYERED, WS_POPUP, WS_SYSMENU,
};

/// `ABM_GETSTATE`'s auto-hide bit.
const AUTOHIDE: usize = 1;
const TIMER: usize = 1;
/// How often the taskbar is looked at while the bar is open.
const TICK_MS: u32 = 120;
/// How long the button keeps asking once the taskbar is up, so the taskbar
/// has finished rising before the button turns plain.
const SETTLE_MS: u64 = 250;
/// The longest the button asks for attention when the taskbar doesn't come.
const GIVE_UP_MS: u64 = 3_000;
/// How many times the taskbar is raised again if it hides while the bar is
/// still open, so the button can never flash on and on.
const MAX_RAISES: u32 = 3;
/// Asks an auto-hidden taskbar to show itself (wParam 1) or hide again
/// (wParam 0); lParam is the monitor. Not documented: Explorer's own message.
const SHOW_AUTOHIDE: u32 = 0x05D1;
/// How long the taskbar has to come up after the message before the button
/// takes over.
const MESSAGE_WAIT_MS: u64 = 400;

/// What one timer tick does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tick {
    /// The taskbar is still hidden: ask for attention (again).
    Ask,
    /// Nothing to do yet.
    Wait,
    /// The taskbar has been up long enough: stop asking, the button turns plain.
    Settle,
    /// The taskbar never came: stop asking for good.
    GiveUp,
}

/// Decides a tick while the button is asking. `up_for_ms`: how long the
/// taskbar has been up, if it is; `asking_for_ms`: how long this try has run.
fn tick(up_for_ms: Option<u64>, asking_for_ms: u64) -> Tick {
    match up_for_ms {
        Some(ms) if ms >= SETTLE_MS => Tick::Settle,
        Some(_) => Tick::Wait,
        None if asking_for_ms >= GIVE_UP_MS => Tick::GiveUp,
        None => Tick::Ask,
    }
}

/// What a tick does after the taskbar was asked to show itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Message {
    /// The taskbar is up: hold.
    Up,
    /// Not yet: look again.
    Wait,
    /// It did not come: the button takes over.
    Fallback,
}

fn after_message(up: bool, waited_ms: u64) -> Message {
    if up {
        Message::Up
    } else if waited_ms >= MESSAGE_WAIT_MS {
        Message::Fallback
    } else {
        Message::Wait
    }
}

/// The taskbar on `monitor` among `taskbars` (taskbar, its monitor), the
/// main one first; `None` when that monitor has none of its own.
fn taskbar_for(monitor: isize, taskbars: &[(isize, isize)]) -> Option<isize> {
    taskbars.iter().find(|(_, m)| *m == monitor).map(|(t, _)| *t)
}

/// Whether the taskbar counts as hidden: less than half of it is on its
/// monitor. Rectangles are left, top, right, bottom.
fn taskbar_hidden(taskbar: (i32, i32, i32, i32), monitor: (i32, i32, i32, i32)) -> bool {
    let width = (taskbar.2.min(monitor.2) - taskbar.0.max(monitor.0)).max(0) as i64;
    let height = (taskbar.3.min(monitor.3) - taskbar.1.max(monitor.1)).max(0) as i64;
    let whole = (taskbar.2 - taskbar.0).max(0) as i64 * (taskbar.3 - taskbar.1).max(0) as i64;
    width * height * 2 < whole
}

/// Where the helper window stands.
#[derive(Clone, Copy)]
enum Phase {
    /// The taskbar was asked to show itself at `since`.
    Messaged { since: Instant },
    /// Asking for attention since `since`; the taskbar came up at `up`.
    Asking { since: Instant, up: Option<Instant> },
    /// The button is plain (or was never needed) and the taskbar should be up.
    Holding,
    /// Nothing more to do until the bar closes.
    Done,
}

#[derive(Clone, Copy)]
struct Helper {
    hwnd: isize,
    phase: Phase,
    raises: u32,
    /// The taskbar on the bar's monitor (0: none found) and that monitor.
    taskbar: isize,
    monitor: isize,
    /// The taskbar was asked to show itself, so it is asked to hide when the
    /// bar closes.
    messaged: bool,
}

thread_local! {
    /// The helper window while the bar is open. Main thread only.
    static HELPER: Cell<Option<Helper>> = const { Cell::new(None) };
}

/// The bar was shown: bring the taskbar up if the setting is on and the
/// taskbar hides by itself. Safe to call when it is already up.
pub fn raise(app: &AppHandle) {
    // The main thread's copy: the config's own lock may be held by a reload
    // that is waiting for this very thread.
    let on = app.try_state::<crate::AppState>().is_some_and(|s| s.mirror.show_taskbar());
    if !on {
        return;
    }
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        if HELPER.with(|h| h.get()).is_some() || !auto_hides() {
            return;
        }
        let Some(bar) = bar_hwnd(&app) else { return };
        // SAFETY: a lookup on the bar's live window.
        let monitor = unsafe { MonitorFromWindow(bar, MONITOR_DEFAULTTONEAREST) }.0 as isize;
        let taskbar = taskbar_for(monitor, &taskbars()).unwrap_or(0);
        let Some(hwnd) = create(bar) else { return };
        let mut helper = Helper {
            hwnd: hwnd.0 as isize,
            phase: Phase::Messaged { since: Instant::now() },
            raises: 1,
            taskbar,
            monitor,
            messaged: false,
        };
        if taskbar != 0 && message(&helper, true) {
            helper.messaged = true;
        } else {
            show_button(hwnd);
            helper.phase = Phase::Asking { since: Instant::now(), up: None };
        }
        HELPER.with(|h| h.set(Some(helper)));
        // SAFETY: a timer on the window this thread just made.
        unsafe { SetTimer(Some(hwnd), TIMER, TICK_MS, None) };
    });
}

/// The bar was hidden: let the taskbar hide again and remove the button.
pub fn lower(app: &AppHandle) {
    let _ = app.run_on_main_thread(|| {
        if let Some(helper) = HELPER.with(|h| h.take()) {
            if helper.messaged {
                message(&helper, false);
            }
            let hwnd = HWND(helper.hwnd as _);
            // SAFETY: the window and timer this thread made.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER);
                let _ = DestroyWindow(hwnd);
            }
        }
    });
}

/// Whether the taskbar is set to hide automatically.
fn auto_hides() -> bool {
    let mut data = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, ..Default::default() };
    // SAFETY: a state query with a correctly sized structure.
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut data) & AUTOHIDE != 0 }
}

/// The bar window's handle.
fn bar_hwnd(app: &AppHandle) -> Option<HWND> {
    let h = app.get_webview_window(crate::window::LABEL)?.hwnd().ok()?;
    Some(HWND(h.0 as _))
}

/// Every taskbar with its monitor: the main one first, then one for each
/// other monitor that shows its own (`Shell_SecondaryTrayWnd`).
fn taskbars() -> Vec<(isize, isize)> {
    // SAFETY: a lookup on a window just found.
    let with_monitor = |t: HWND| (t.0 as isize, unsafe { MonitorFromWindow(t, MONITOR_DEFAULTTONEAREST) }.0 as isize);
    let mut out = Vec::new();
    // SAFETY: plain window lookups; each handle found is only measured.
    unsafe {
        if let Ok(main) = FindWindowW(w!("Shell_TrayWnd"), None) {
            out.push(with_monitor(main));
        }
        let mut after = None;
        while let Ok(next) = FindWindowExW(None, after, w!("Shell_SecondaryTrayWnd"), None) {
            out.push(with_monitor(next));
            after = Some(next);
        }
    }
    out
}

/// Asks the bar's taskbar to show itself (`show`) or hide again.
fn message(helper: &Helper, show: bool) -> bool {
    // SAFETY: a posted message to Explorer's taskbar window; nothing waits on it.
    unsafe {
        PostMessageW(Some(HWND(helper.taskbar as _)), SHOW_AUTOHIDE, WPARAM(usize::from(show)), LPARAM(helper.monitor))
            .is_ok()
    }
}

/// Whether the bar's taskbar is hidden right now (the main one when none was
/// found for the bar's monitor); `None` when it can't be measured.
fn hidden_now(taskbar: isize) -> Option<bool> {
    // SAFETY: plain reads on window and monitor handles; every result is checked.
    unsafe {
        let tray = if taskbar != 0 { HWND(taskbar as _) } else { FindWindowW(w!("Shell_TrayWnd"), None).ok()? };
        let mut rect = RECT::default();
        GetWindowRect(tray, &mut rect).ok()?;
        let mut monitor = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromWindow(tray, MONITOR_DEFAULTTONEAREST), &mut monitor).as_bool() {
            return None;
        }
        let m = monitor.rcMonitor;
        Some(taskbar_hidden((rect.left, rect.top, rect.right, rect.bottom), (m.left, m.top, m.right, m.bottom)))
    }
}

/// This module's HINSTANCE (the linker's `__ImageBase`).
fn instance() -> HINSTANCE {
    extern "C" {
        static __ImageBase: u8;
    }
    // Only the address is taken, never the value.
    HINSTANCE(std::ptr::addr_of!(__ImageBase) as *mut std::ffi::c_void)
}

/// Makes the helper window: one transparent pixel at the bar's corner, so a
/// button for it lands on the bar's monitor, with Flick's icon. It stays
/// hidden, without a button, until `show_button`; it also carries the timer.
fn create(bar: HWND) -> Option<HWND> {
    let class = w!("FlickTaskbarButton");
    let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance(), lpszClassName: class, ..Default::default() };
    let mut at = RECT::default();
    // SAFETY: a read on the bar's live window; a valid class description
    // (registering it again fails harmlessly); the window belongs to this thread.
    unsafe {
        let _ = GetWindowRect(bar, &mut at);
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_APPWINDOW | WS_EX_LAYERED,
            class,
            w!("Flick"),
            WS_POPUP | WS_SYSMENU,
            at.left,
            at.top,
            1,
            1,
            None,
            None,
            Some(instance()),
            None,
        )
        .ok()?;
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
        // The bar's icons, so the button is not blank (a button shows its
        // window's icon only when the window has a system menu).
        for kind in [ICON_BIG, ICON_SMALL] {
            let icon = SendMessageW(bar, WM_GETICON, Some(WPARAM(kind as usize)), None);
            if icon.0 != 0 {
                SendMessageW(hwnd, WM_SETICON, Some(WPARAM(kind as usize)), Some(LPARAM(icon.0)));
            }
        }
        Some(hwnd)
    }
}

/// Shows the helper window, which gives it its taskbar button.
fn show_button(hwnd: HWND) {
    // SAFETY: a window of this thread, shown without taking the focus.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
}

/// Asks for attention (`true`) or stops asking.
fn attention(hwnd: HWND, ask: bool) {
    let info = FLASHWINFO {
        cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
        hwnd,
        dwFlags: if ask { FLASHW_TRAY } else { FLASHW_STOP },
        uCount: u32::from(ask),
        dwTimeout: 0,
    };
    // SAFETY: a correctly sized structure for a window of this thread.
    let _ = unsafe { FlashWindowEx(&info) };
}

fn on_timer(hwnd: HWND) {
    let Some(mut helper) = HELPER.with(|h| h.get()) else { return };
    if helper.hwnd != hwnd.0 as isize {
        return;
    }
    let hidden = hidden_now(helper.taskbar).unwrap_or(false);
    let now = Instant::now();
    let ms = |from: Instant| now.duration_since(from).as_millis() as u64;
    match helper.phase {
        Phase::Messaged { since } => {
            helper.phase = match after_message(!hidden, ms(since)) {
                Message::Up => Phase::Holding,
                Message::Wait => Phase::Messaged { since },
                Message::Fallback => {
                    show_button(hwnd);
                    Phase::Asking { since: now, up: None }
                }
            };
        }
        Phase::Asking { since, up } => {
            let up = if hidden { None } else { up.or(Some(now)) };
            helper.phase = match tick(up.map(ms), ms(since)) {
                Tick::Ask => {
                    attention(hwnd, true);
                    Phase::Asking { since, up }
                }
                Tick::Wait => Phase::Asking { since, up },
                Tick::Settle => {
                    attention(hwnd, false);
                    Phase::Holding
                }
                Tick::GiveUp => {
                    attention(hwnd, false);
                    Phase::Done
                }
            };
        }
        // It hid although the bar is still open: raise it again, a few times at most.
        Phase::Holding if hidden => {
            helper.phase = if helper.raises < MAX_RAISES {
                helper.raises += 1;
                if helper.messaged && message(&helper, true) {
                    Phase::Messaged { since: now }
                } else {
                    show_button(hwnd);
                    Phase::Asking { since: now, up: None }
                }
            } else {
                Phase::Done
            };
        }
        Phase::Holding => {}
        Phase::Done => {
            // SAFETY: the timer this thread set on its own window.
            let _ = unsafe { KillTimer(Some(hwnd), TIMER) };
        }
    }
    HELPER.with(|h| h.set(Some(helper)));
}

unsafe extern "system" fn proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_TIMER && wparam.0 == TIMER {
        on_timer(hwnd);
        return LRESULT(0);
    }
    // SAFETY: default handling for a window of this thread.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_asks_until_the_taskbar_is_up_then_settles() {
        assert_eq!(tick(None, 0), Tick::Ask);
        assert_eq!(tick(None, 2_999), Tick::Ask);
        // Up, but only just: keep the highlight while it rises.
        assert_eq!(tick(Some(0), 400), Tick::Wait);
        assert_eq!(tick(Some(249), 400), Tick::Wait);
        assert_eq!(tick(Some(250), 400), Tick::Settle);
    }

    #[test]
    fn after_the_message_the_taskbar_is_waited_for_then_the_button_takes_over() {
        assert_eq!(after_message(true, 0), Message::Up);
        assert_eq!(after_message(true, 390), Message::Up);
        assert_eq!(after_message(false, 0), Message::Wait);
        assert_eq!(after_message(false, MESSAGE_WAIT_MS - 1), Message::Wait);
        assert_eq!(after_message(false, MESSAGE_WAIT_MS), Message::Fallback);
    }

    #[test]
    fn the_taskbar_on_the_bars_monitor_is_the_one_used() {
        // (taskbar, its monitor): the main one first, then the others.
        let bars = [(10, 1), (20, 2), (30, 3)];
        assert_eq!(taskbar_for(1, &bars), Some(10));
        assert_eq!(taskbar_for(3, &bars), Some(30));
        // A monitor without its own taskbar (only on the main one).
        assert_eq!(taskbar_for(4, &bars), None);
        assert_eq!(taskbar_for(1, &[]), None);
    }

    #[test]
    fn a_taskbar_that_never_comes_is_given_up_on() {
        assert_eq!(tick(None, 3_000), Tick::GiveUp);
        // Up at the last moment still counts.
        assert_eq!(tick(Some(10), 3_500), Tick::Wait);
    }

    #[test]
    fn a_taskbar_mostly_off_its_monitor_is_hidden() {
        let monitor = (0, 0, 1920, 1200);
        // Bottom edge: two pixels showing, then fully up.
        assert!(taskbar_hidden((0, 1198, 1920, 1258), monitor));
        assert!(!taskbar_hidden((0, 1140, 1920, 1200), monitor));
        // Half way up while it slides: not hidden yet.
        assert!(!taskbar_hidden((0, 1170, 1920, 1230), monitor));
        // Left and top edges.
        assert!(taskbar_hidden((-58, 0, 2, 1200), monitor));
        assert!(!taskbar_hidden((0, 0, 60, 1200), monitor));
        assert!(taskbar_hidden((0, -58, 1920, 2), monitor));
        // A second monitor to the left.
        assert!(!taskbar_hidden((-1920, 1140, 0, 1200), (-1920, 0, 0, 1200)));
        // Nothing to measure.
        assert!(!taskbar_hidden((0, 0, 0, 0), monitor));
    }
}
