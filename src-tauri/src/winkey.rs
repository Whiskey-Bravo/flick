//! The Win32 side of the Win key: a low-level keyboard hook on its own thread.
//!
//! The hook thread installs `WH_KEYBOARD_LL`, turns each event into an [`Ev`]
//! and asks [`Press`] (winkey_logic.rs) what to do. The callback never blocks:
//! everything slow (installing or removing the mouse hook, opening Flick) is
//! posted to the thread's own message loop and done after the callback returns.
//! The only event ever swallowed is the Win-up of a lone press Flick takes; it
//! is re-sent at once as `[mask down, mask up, Win up]`, tagged as Flick's own.
//!
//! Never started by tests: they cover only the pure helpers.

// started by lib.rs when the setting is on (Task 5)
#![allow(dead_code)]

use std::cell::{Cell, OnceCell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{
    GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetLastInputInfo, SendInput, INPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    LASTINPUTINFO,
    VIRTUAL_KEY, VK_CONTROL, VK_LBUTTON, VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_SHIFT, VK_XBUTTON1,
    VK_XBUTTON2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, KillTimer, PeekMessageW, PostThreadMessageW, SetTimer,
    SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
    LLKHF_UP, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP,
    WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN,
    WM_TIMER, WM_XBUTTONDOWN,
};

use crate::controls::key_input;
use crate::winkey_logic::{Ev, Held, Press, Verdict, Watchdog};

/// Tag on the input Flick sends itself ("FLWK"); one value shared with controls.rs.
pub const FLICK_TAG: usize = crate::controls::FLICK_INPUT_TAG;

/// The unassigned key sent between the swallowed Win down and its re-sent up,
/// so Windows sees a Win combination and doesn't open Start.
const MASK_VK: u16 = 0xE8;

const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

/// Thread message: install (wParam 1) or remove (0) the mouse hook. lParam is
/// the press generation it was posted for; older ones are ignored.
const MSG_MOUSE: u32 = WM_APP + 1;
/// Thread message: a lone press was taken; call `on_lone`.
const MSG_LONE: u32 = WM_APP + 2;

/// How often the watchdog looks at the hook.
const WATCHDOG_MS: u32 = 5_000;
/// The wait before a second try when installing the keyboard hook fails.
const RETRY_MS: u32 = 3_000;

/// Shown when the keyboard hook can't be installed.
const UNAVAILABLE: &str = "Windows wouldn't let Flick watch the Win key.";

/// The running Win-key hook. Stopping it (or dropping it) removes both hooks
/// and ends its thread.
pub struct WinKeyHook {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
    healthy: Arc<AtomicBool>,
}

impl WinKeyHook {
    /// Starts the hook thread and installs the keyboard hook. When installing
    /// fails it tries once more 3 s later (so this can block for 3 s) and then
    /// gives up with a reason to show.
    ///
    /// `may_take` runs inside the hook callback on every lone press: it must
    /// return at once (atomics only; no locks the UI thread may hold, no
    /// waiting). `on_lone` runs on the hook thread after a taken press; it must
    /// not wait on the thread that calls [`WinKeyHook::stop`].
    pub fn start(
        may_take: Box<dyn Fn() -> bool + Send + Sync>,
        on_lone: Box<dyn Fn() + Send + Sync>,
    ) -> Result<WinKeyHook, String> {
        let healthy = Arc::new(AtomicBool::new(true));
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_healthy = healthy.clone();
        let thread = std::thread::Builder::new()
            .name("flick-winkey".into())
            .spawn(move || run(Callbacks { may_take, on_lone }, thread_healthy, ready_tx))
            .map_err(|_| UNAVAILABLE.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(WinKeyHook { thread_id, thread: Some(thread), healthy }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(UNAVAILABLE.to_string())
            }
        }
    }

    /// Removes both hooks and ends the thread (the same as dropping it).
    pub fn stop(self) {}

    /// False once reinstalling the keyboard hook has failed twice in a row;
    /// true again after the next successful install.
    pub fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }
}

impl Drop for WinKeyHook {
    fn drop(&mut self) {
        let Some(thread) = self.thread.take() else { return };
        // SAFETY: posting a message to a thread id; harmless if it has ended.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        // Dropped from inside `on_lone`: the loop ends after it returns.
        if thread.thread().id() != std::thread::current().id() {
            let _ = thread.join();
        }
    }
}

struct Callbacks {
    may_take: Box<dyn Fn() -> bool + Send + Sync>,
    on_lone: Box<dyn Fn() + Send + Sync>,
}

/// One hook thread's state. The callbacks and the loop run on the same
/// thread, so plain thread-locals do; the callback only `try_borrow`s.
struct Hooked {
    press: Press,
    /// Bumped whenever the keyboard hook is (re)installed, so mouse-hook
    /// messages posted for an older press are ignored.
    generation: usize,
}

thread_local! {
    static CALLBACKS: OnceCell<Callbacks> = const { OnceCell::new() };
    static HOOKED: RefCell<Hooked> = RefCell::new(Hooked { press: Press::new(), generation: 0 });
    /// `GetTickCount64` at the keyboard hook's last event, for the watchdog.
    static LAST_EVENT_MS: Cell<u64> = const { Cell::new(0) };
}

/// The hook thread: installs the keyboard hook, then runs the message loop
/// until `WM_QUIT`, then removes both hooks.
fn run(callbacks: Callbacks, healthy: Arc<AtomicBool>, ready: mpsc::Sender<Result<u32, String>>) {
    // SAFETY: plain Win32 calls on this thread. PeekMessageW creates the
    // message queue so PostThreadMessageW works from now on.
    let thread_id = unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        GetCurrentThreadId()
    };
    CALLBACKS.with(|c| {
        let _ = c.set(callbacks);
    });
    let mut keyboard = match install(true).or_else(|_| {
        std::thread::sleep(Duration::from_millis(RETRY_MS as u64));
        install(true)
    }) {
        Ok(hook) => hook,
        Err(()) => {
            let _ = ready.send(Err(UNAVAILABLE.to_string()));
            return;
        }
    };
    LAST_EVENT_MS.with(|t| t.set(now_ms()));
    let _ = ready.send(Ok(thread_id));
    drop(ready);

    let mut mouse: Option<HHOOK> = None;
    let mut watchdog = Watchdog::new();
    let mut failures = 0u32;
    let mut retry_timer = 0usize;
    // SAFETY: a thread timer (no window); WM_TIMER comes to this queue.
    let tick_timer = unsafe { SetTimer(None, 0, WATCHDOG_MS, None) };

    let mut msg = MSG::default();
    loop {
        // SAFETY: a valid MSG out-pointer; 0 is WM_QUIT, -1 an error: both end it.
        match unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 {
            0 | -1 => break,
            _ => {}
        }
        if !msg.hwnd.0.is_null() {
            continue;
        }
        match msg.message {
            MSG_MOUSE => {
                let current = HOOKED.with(|h| h.borrow().generation);
                if msg.lParam.0 as usize == current {
                    set_mouse(&mut mouse, msg.wParam.0 != 0);
                }
            }
            MSG_LONE => CALLBACKS.with(|c| {
                if let Some(c) = c.get() {
                    let _ = catch_unwind(AssertUnwindSafe(|| (c.on_lone)()));
                }
            }),
            WM_TIMER => {
                let timer = msg.wParam.0;
                let attempt = if timer == retry_timer && retry_timer != 0 {
                    // SAFETY: killing a timer this thread made.
                    let _ = unsafe { KillTimer(None, retry_timer) };
                    retry_timer = 0;
                    true
                } else if timer == tick_timer && tick_timer != 0 {
                    if failures >= 2 {
                        // Unavailable: keep trying quietly, between presses.
                        !win_held()
                    } else {
                        let now = now_ms();
                        let last_event = LAST_EVENT_MS.with(|t| t.get());
                        // Never reinstall in the middle of a press.
                        !win_held() && watchdog.should_reinstall(now, last_event, last_input_ms(now))
                    }
                } else {
                    false
                };
                if attempt {
                    if reinstall(&mut keyboard, &mut mouse) {
                        failures = 0;
                        healthy.store(true, Ordering::Relaxed);
                    } else {
                        failures += 1;
                        if failures == 1 {
                            // SAFETY: a thread timer (no window).
                            retry_timer = unsafe { SetTimer(None, 0, RETRY_MS, None) };
                        } else {
                            healthy.store(false, Ordering::Relaxed);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // SAFETY: removing timers and hooks this thread made.
    unsafe {
        if tick_timer != 0 {
            let _ = KillTimer(None, tick_timer);
        }
        if retry_timer != 0 {
            let _ = KillTimer(None, retry_timer);
        }
    }
    set_mouse(&mut mouse, false);
    // SAFETY: a hook this thread installed.
    let _ = unsafe { UnhookWindowsHookEx(keyboard) };
}

/// Installs the keyboard (`true`) or mouse (`false`) low-level hook.
fn install(keyboard: bool) -> Result<HHOOK, ()> {
    // SAFETY: the hook procedures are `extern "system"` functions of this
    // module, valid for the life of the process.
    unsafe {
        if keyboard {
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(instance()), 0)
        } else {
            SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(instance()), 0)
        }
    }
    .map_err(|_| ())
}

/// Swaps in a fresh keyboard hook; on success the press starts over and the
/// mouse hook comes out. On failure the old hook stays.
fn reinstall(keyboard: &mut HHOOK, mouse: &mut Option<HHOOK>) -> bool {
    let Ok(new) = install(true) else { return false };
    // SAFETY: the old hook this thread installed.
    let _ = unsafe { UnhookWindowsHookEx(*keyboard) };
    *keyboard = new;
    HOOKED.with(|h| {
        let mut h = h.borrow_mut();
        h.press = Press::new();
        h.generation = h.generation.wrapping_add(1);
    });
    set_mouse(mouse, false);
    LAST_EVENT_MS.with(|t| t.set(now_ms()));
    true
}

/// Puts the mouse hook in or takes it out. Only ever called from the loop.
fn set_mouse(mouse: &mut Option<HHOOK>, want: bool) {
    match (mouse.is_some(), want) {
        (false, true) => *mouse = install(false).ok(),
        (true, false) => {
            if let Some(hook) = mouse.take() {
                // SAFETY: a hook this thread installed.
                let _ = unsafe { UnhookWindowsHookEx(hook) };
            }
        }
        _ => {}
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

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        LAST_EVENT_MS.with(|t| t.set(now_ms()));
        // SAFETY: for HC_ACTION, lparam points at a KBDLLHOOKSTRUCT.
        let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if on_key(k) {
            return LRESULT(1);
        }
    }
    // SAFETY: passing the event on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, lparam points at an MSLLHOOKSTRUCT.
        let m = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if let Some(ev) = mouse_event(wparam.0 as u32, m.dwExtraInfo) {
            // A mouse event is never lone, so this never swallows.
            feed(ev, false, 0);
        }
    }
    // SAFETY: passing the event on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Handles one key event; true swallows it.
fn on_key(k: &KBDLLHOOKSTRUCT) -> bool {
    let vk = k.vkCode;
    let up = k.flags.0 & LLKHF_UP.0 != 0;
    let injected = k.flags.0 & LLKHF_INJECTED.0 != 0;
    let held = if is_win(vk) && !up && k.dwExtraInfo != FLICK_TAG { held_now(vk) } else { Held::default() };
    let Some(ev) = key_event(vk, up, injected, k.dwExtraInfo, held) else { return false };
    let button_held = ev == Ev::WinUp && any_button_down();
    feed(ev, button_held, vk)
}

/// Feeds the state machine and acts on its verdict; true swallows the event.
/// `win_vk` is the Win key whose up this is (used only for `Lone`).
fn feed(ev: Ev, button_held: bool, win_vk: u32) -> bool {
    // Busy only if a callback re-entered while the loop held it: let it pass.
    let Some((verdict, generation)) = HOOKED.with(|h| {
        h.try_borrow_mut().ok().map(|mut h| (h.press.on(ev, button_held), h.generation))
    }) else {
        return false;
    };
    match verdict {
        Verdict::Pass => false,
        Verdict::ArmMouse => {
            post(MSG_MOUSE, 1, generation);
            false
        }
        Verdict::DisarmMouse => {
            post(MSG_MOUSE, 0, generation);
            false
        }
        Verdict::Lone => {
            post(MSG_MOUSE, 0, generation);
            if !may_take() {
                return false;
            }
            resend_win_up(win_vk as u16);
            post(MSG_LONE, 0, 0);
            true
        }
    }
}

fn may_take() -> bool {
    CALLBACKS.with(|c| {
        c.get().is_some_and(|c| catch_unwind(AssertUnwindSafe(|| (c.may_take)())).unwrap_or(false))
    })
}

/// Posts a message to this (the hook) thread's own loop.
fn post(message: u32, wparam: usize, lparam: usize) {
    // SAFETY: a plain post to the current thread's queue.
    let _ = unsafe {
        PostThreadMessageW(GetCurrentThreadId(), message, WPARAM(wparam), LPARAM(lparam as isize))
    };
}

/// Re-sends the swallowed Win-up behind the mask key. If Windows inserts
/// fewer than all three, sends a lone Win-up so Win is never left down.
fn resend_win_up(win_vk: u16) {
    let size = std::mem::size_of::<INPUT>() as i32;
    let batch = resend_batch(win_vk);
    // SAFETY: valid slices of fully initialised INPUTs.
    unsafe {
        if SendInput(&batch, size) < batch.len() as u32 {
            SendInput(&[tagged_win_up(win_vk)], size);
        }
    }
}

/// `[mask down, mask up, Win up]`, all tagged as Flick's own.
fn resend_batch(win_vk: u16) -> [INPUT; 3] {
    let mut mask = [key_input(VIRTUAL_KEY(MASK_VK), false), key_input(VIRTUAL_KEY(MASK_VK), true)];
    for i in &mut mask {
        i.Anonymous.ki.dwExtraInfo = FLICK_TAG;
    }
    [mask[0], mask[1], tagged_win_up(win_vk)]
}

fn tagged_win_up(win_vk: u16) -> INPUT {
    let mut i = key_input(VIRTUAL_KEY(win_vk), true);
    // Both Win keys are extended keys, like the real event.
    i.Anonymous.ki.dwFlags = KEYEVENTF_KEYUP | KEYEVENTF_EXTENDEDKEY;
    i.Anonymous.ki.dwExtraInfo = FLICK_TAG;
    i
}

fn is_win(vk: u32) -> bool {
    vk == VK_LWIN || vk == VK_RWIN
}

/// The event the state machine sees for one key, or `None` when it sees
/// nothing: Flick's own tagged input and key-ups other than Win.
fn key_event(vk: u32, up: bool, injected: bool, extra: usize, held: Held) -> Option<Ev> {
    if extra == FLICK_TAG {
        return None;
    }
    match (is_win(vk), up) {
        (true, true) => Some(Ev::WinUp),
        (true, false) if held.other_win => Some(Ev::OtherWinDown),
        (true, false) => Some(Ev::WinDown { physical: !injected, held }),
        (false, false) => Some(Ev::KeyDown { flick: false }),
        (false, true) => None,
    }
}

/// The event for one mouse message: button downs and wheels only.
fn mouse_event(message: u32, extra: usize) -> Option<Ev> {
    if extra == FLICK_TAG {
        return None;
    }
    match message {
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN => {
            Some(Ev::MouseDown { flick: false })
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => Some(Ev::Wheel { flick: false }),
        _ => None,
    }
}

fn key_down(vk: u16) -> bool {
    // SAFETY: a plain key-state read.
    unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
}

/// What's already down as the Win key `vk` goes down. The hook runs before
/// Windows updates the key state, so `win_already_down` means auto-repeat.
fn held_now(vk: u32) -> Held {
    let other = if vk == VK_LWIN { VK_RWIN } else { VK_LWIN };
    Held {
        ctrl: key_down(VK_CONTROL.0),
        shift: key_down(VK_SHIFT.0),
        alt: key_down(VK_MENU.0),
        other_win: key_down(other as u16),
        mouse_button: any_button_down(),
        win_already_down: key_down(vk as u16),
    }
}

fn any_button_down() -> bool {
    [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2].iter().any(|b| key_down(b.0))
}

fn win_held() -> bool {
    key_down(VK_LWIN as u16) || key_down(VK_RWIN as u16)
}

fn now_ms() -> u64 {
    // SAFETY: no arguments.
    unsafe { GetTickCount64() }
}

/// `GetLastInputInfo` on the `GetTickCount64` clock; 0 when it fails.
fn last_input_ms(now: u64) -> u64 {
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: a valid, sized out-struct.
    if unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        widen_tick(now, info.dwTime)
    } else {
        0
    }
}

/// Widens a 32-bit tick (`GetTickCount`) to the 64-bit clock `now` is on:
/// the high bits come from `now`, one wrap back if that would be in the future.
fn widen_tick(now: u64, low: u32) -> u64 {
    let wide = (now & !0xFFFF_FFFF) | low as u64;
    if wide <= now {
        wide
    } else if now >= 1 << 32 {
        wide - (1 << 32)
    } else {
        now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ev_from_flags_marks_flick_tag_and_injection() {
        let none = Held::default();
        let ctrl = Held { ctrl: true, ..none };
        assert_eq!(key_event(VK_LWIN, false, false, 0, ctrl), Some(Ev::WinDown { physical: true, held: ctrl }));
        assert_eq!(key_event(VK_RWIN, false, true, 7, none), Some(Ev::WinDown { physical: false, held: none }));
        assert_eq!(key_event(VK_LWIN, true, false, 0, none), Some(Ev::WinUp));
        assert_eq!(key_event(VK_RWIN, true, true, 7, none), Some(Ev::WinUp));
        assert_eq!(key_event(0x41, false, false, 0, none), Some(Ev::KeyDown { flick: false }));
        assert_eq!(key_event(0x41, false, true, 7, none), Some(Ev::KeyDown { flick: false }));
        assert_eq!(key_event(0x41, true, false, 0, none), None);
        // Flick's own injections never reach the state machine.
        for (vk, up) in [(VK_LWIN, false), (VK_LWIN, true), (0xE8, false), (0xE8, true), (0x41, false)] {
            assert_eq!(key_event(vk, up, true, FLICK_TAG, none), None, "{vk:#x} {up}");
        }
    }

    #[test]
    fn second_win_key_is_other_win_down() {
        let other = Held { other_win: true, ..Held::default() };
        assert_eq!(key_event(VK_RWIN, false, false, 0, other), Some(Ev::OtherWinDown));
        assert_eq!(key_event(VK_LWIN, false, true, 0, other), Some(Ev::OtherWinDown));
    }

    #[test]
    fn mouse_events_are_button_downs_and_wheels() {
        for m in [WM_LBUTTONDOWN, WM_RBUTTONDOWN, WM_MBUTTONDOWN, WM_XBUTTONDOWN] {
            assert_eq!(mouse_event(m, 0), Some(Ev::MouseDown { flick: false }));
            assert_eq!(mouse_event(m, FLICK_TAG), None);
        }
        for m in [WM_MOUSEWHEEL, WM_MOUSEHWHEEL] {
            assert_eq!(mouse_event(m, 0), Some(Ev::Wheel { flick: false }));
        }
        // Moves and button-ups.
        for m in [0x200, 0x202, 0x205, 0x208, 0x20C] {
            assert_eq!(mouse_event(m, 0), None, "{m:#x}");
        }
    }

    #[test]
    fn resend_batch_is_mask_then_win_up_all_tagged() {
        use windows::Win32::UI::Input::KeyboardAndMouse::INPUT_KEYBOARD;
        let batch = resend_batch(VK_RWIN as u16);
        // SAFETY: every entry is a keyboard INPUT.
        let keys: Vec<_> = batch.iter().map(|i| (i.r#type, unsafe { i.Anonymous.ki })).collect();
        assert!(keys.iter().all(|(t, k)| *t == INPUT_KEYBOARD && k.dwExtraInfo == FLICK_TAG));
        assert_eq!(keys.iter().map(|(_, k)| k.wVk.0).collect::<Vec<_>>(), [0xE8, 0xE8, 0x5C]);
        let up = |k: &windows::Win32::UI::Input::KeyboardAndMouse::KEYBDINPUT| k.dwFlags.contains(KEYEVENTF_KEYUP);
        assert_eq!(keys.iter().map(|(_, k)| up(k)).collect::<Vec<_>>(), [false, true, true]);
        assert!(keys[2].1.dwFlags.contains(KEYEVENTF_EXTENDEDKEY));
    }

    #[test]
    fn widen_tick_takes_high_bits_from_now() {
        let hi = 3u64 << 32;
        assert_eq!(widen_tick(hi + 5_000, 4_000), hi + 4_000);
        assert_eq!(widen_tick(hi + 5_000, 5_000), hi + 5_000);
        // The 32-bit tick wrapped just before now's did: one wrap back.
        assert_eq!(widen_tick(hi + 10, 0xFFFF_FF00), hi - 0x100);
        // Before the first wrap.
        assert_eq!(widen_tick(9_000, 8_000), 8_000);
        // A tick from the future with nothing to wrap back to: clamp to now.
        assert_eq!(widen_tick(9_000, 9_500), 9_000);
    }
}
