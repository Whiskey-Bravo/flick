//! The Win32 side of the Win key: a low-level keyboard hook on its own thread.
//! It runs in the helper process (winkey_helper.rs), never in Flick itself.
//!
//! The hook thread installs `WH_KEYBOARD_LL`, turns each event into an [`Ev`]
//! and asks [`Press`] (winkey_logic.rs) what to do. The callback never blocks:
//! everything slow (installing or removing the mouse hook, opening Flick) is
//! posted to the thread's own message loop and done after the callback returns;
//! opening Flick is handed on again to a worker thread, so the hook thread only
//! ever runs the loop and the callbacks.
//! A Win-down Flick may take is held back from Windows (swallowed). If another
//! key, click or wheel follows, the hook sends the Win-down (tagged as
//! Flick's own) from inside that event's callback, which Windows handles
//! before the event itself, so the combination works as usual. If Win comes
//! up alone, its up is swallowed too: Windows never saw the press, so Start
//! can't open, and Flick opens. Nothing is ever left down: the only key held
//! back is a Win-down Windows hasn't seen. (Masking a lone press with a dummy
//! key instead left Start opening now and then on Windows 11.)
//!
//! The watchdog's signal is keyboard-only raw input (`RIDEV_INPUTSINK` on a
//! message-only window on the hook thread): raw input keeps arriving after
//! Windows silently drops a hook, and the mouse never counts.
//!
//! Never started by tests: they cover only the pure helpers.

use std::cell::{Cell, OnceCell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::SystemInformation::{GetTickCount, GetTickCount64};
use windows::Win32::System::Threading::{
    GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    VK_CONTROL, VK_LBUTTON, VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_SHIFT, VK_XBUTTON1, VK_XBUTTON2,
};
use windows::Win32::UI::Input::{RegisterRawInputDevices, RAWINPUTDEVICE, RIDEV_INPUTSINK, RIDEV_REMOVE};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    KillTimer, PeekMessageW, PostThreadMessageW, RegisterClassW, SetTimer, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, HC_ACTION, HHOOK, HWND_MESSAGE, KBDLLHOOKSTRUCT,
    LLKHF_INJECTED, LLKHF_UP, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_INPUT, WM_LBUTTONDOWN, WM_MBUTTONDOWN,
    WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_TIMER, WM_XBUTTONDOWN, WNDCLASSW,
};

use crate::controls::key_input;
use crate::winkey_logic::{fresh_press, hold_is_stale, is_late, step, Ev, Held, Press, Step, Verdict, Watchdog};

/// Tag on the input Flick sends itself ("FLWK"); one value shared with controls.rs.
pub const FLICK_TAG: usize = crate::controls::FLICK_INPUT_TAG;

const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

/// Thread message: install (wParam 1) or remove (0) the mouse hook. lParam is
/// the press generation it was posted for; older ones are ignored.
const MSG_MOUSE: u32 = WM_APP + 1;
/// Thread message: a lone press was taken; wake the `on_lone` worker.
const MSG_LONE: u32 = WM_APP + 2;

/// How often the watchdog looks at the hook.
const WATCHDOG_MS: u32 = 5_000;
/// The wait before a second try when installing the keyboard hook fails.
const RETRY_MS: u32 = 3_000;

/// Shown when the keyboard hook can't be installed.
pub const UNAVAILABLE: &str = "Windows wouldn't let Flick watch the Win key.";

/// The running Win-key hook. Dropping it removes both hooks and ends its
/// threads.
pub struct WinKeyHook {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
    /// Runs `on_lone`; ends when the hook thread drops its sender.
    worker: Option<JoinHandle<()>>,
}

impl WinKeyHook {
    /// Starts the hook thread and installs the keyboard hook. When installing
    /// fails it tries once more 3 s later (so this can block for 3 s) and then
    /// gives up with a reason to show.
    ///
    /// `may_take` runs inside the hook callback on every lone press: it must
    /// return at once (atomics only; no locks the UI thread may hold, no
    /// waiting). `on_lone` runs on a worker thread of its own after a taken
    /// press, never on the hook thread; it must not wait on the thread that
    /// drops the hook. `health` gets `false` once reinstalling the keyboard
    /// hook has failed twice in a row and `true` after the next successful
    /// install; it disconnects when the hook thread ends.
    pub fn start(
        may_take: Box<dyn Fn() -> bool + Send + Sync>,
        on_lone: Box<dyn Fn() + Send + Sync>,
        health: mpsc::Sender<bool>,
    ) -> Result<WinKeyHook, String> {
        let (lone_tx, lone_rx) = mpsc::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("flick-winkey-lone".into())
            .spawn(move || {
                for () in lone_rx {
                    let _ = catch_unwind(AssertUnwindSafe(|| on_lone()));
                }
            })
            .map_err(|_| UNAVAILABLE.to_string())?;
        let (ready_tx, ready_rx) = mpsc::channel();
        // On failure here `lone_tx` is dropped with the closure, ending the worker.
        let thread = match std::thread::Builder::new()
            .name("flick-winkey".into())
            .spawn(move || run(may_take, lone_tx, health, ready_tx))
        {
            Ok(thread) => thread,
            Err(_) => {
                let _ = worker.join();
                return Err(UNAVAILABLE.to_string());
            }
        };
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(WinKeyHook {
                thread_id,
                thread: Some(thread),
                worker: Some(worker),
            }),
            failed => {
                // The hook thread has ended (or is ending) and drops `lone_tx`.
                let _ = thread.join();
                let _ = worker.join();
                Err(failed.ok().and_then(Result::err).unwrap_or_else(|| UNAVAILABLE.to_string()))
            }
        }
    }
}

impl Drop for WinKeyHook {
    fn drop(&mut self) {
        let Some(thread) = self.thread.take() else { return };
        let worker = self.worker.take();
        // Retried briefly in case the queue is full; it fails for good only if
        // the thread has already ended.
        let mut posted = false;
        for _ in 0..20 {
            // SAFETY: posting a message to a thread id; harmless if it has ended.
            if unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }.is_ok() {
                posted = true;
                break;
            }
            if thread.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Never join a thread that may not have been told to stop: leaking it
        // beats hanging.
        if !posted && !thread.is_finished() {
            return;
        }
        let me = std::thread::current().id();
        if thread.thread().id() == me {
            return;
        }
        let _ = thread.join();
        // The hook thread has dropped its sender, so the worker ends after any
        // `on_lone` in progress. Dropped from inside `on_lone`: don't wait on it.
        if let Some(worker) = worker {
            if worker.thread().id() != me {
                let _ = worker.join();
            }
        }
    }
}

/// One hook thread's state. The callbacks and the loop run on the same
/// thread, so plain thread-locals do; the callback only `try_borrow`s.
struct Hooked {
    press: Press,
    /// Bumped whenever the keyboard hook is (re)installed, so mouse-hook
    /// messages posted for an older press are ignored.
    generation: usize,
    /// The Win key whose down is held back from Windows, if any, and
    /// `GetTickCount64` at its last event (the down or an auto-repeat).
    held: Option<(u16, u64)>,
    /// The last Win key seen going down (not Flick's own) and when, to tell
    /// an auto-repeat from a fresh press (see [`fresh_press`]).
    last_down: Option<(u16, u64)>,
}

type MayTake = Box<dyn Fn() -> bool + Send + Sync>;

thread_local! {
    static MAY_TAKE: OnceCell<MayTake> = const { OnceCell::new() };
    static HOOKED: RefCell<Hooked> = RefCell::new(Hooked { press: Press::new(), generation: 0, held: None, last_down: None });
    /// `GetTickCount64` at the keyboard hook's last event, for the watchdog.
    static LAST_EVENT_MS: Cell<u64> = const { Cell::new(0) };
    /// `GetTickCount64` at the last raw keyboard input (`WM_INPUT`); 0 if none.
    static LAST_RAW_KEY_MS: Cell<u64> = const { Cell::new(0) };
}

/// The hook thread: installs the keyboard hook, then runs the message loop
/// until `WM_QUIT`, then removes both hooks.
fn run(
    may_take: MayTake,
    lone: mpsc::Sender<()>,
    health: mpsc::Sender<bool>,
    ready: mpsc::Sender<Result<u32, String>>,
) {
    // SAFETY: plain Win32 calls on this thread. PeekMessageW creates the
    // message queue so PostThreadMessageW works from now on.
    let thread_id = unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        GetCurrentThreadId()
    };
    MAY_TAKE.with(|c| {
        let _ = c.set(may_take);
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
    // Without it the watchdog has no signal and never reinstalls.
    let raw_window = raw_keyboard_window();

    let mut mouse: Option<HHOOK> = None;
    let mut watchdog = Watchdog::new();
    let mut failures = 0u32;
    // What `health` was last told; it hears changes only.
    let mut told_healthy = true;
    let mut tell = |healthy: bool| {
        if healthy != told_healthy {
            told_healthy = healthy;
            let _ = health.send(healthy);
        }
    };
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
            // SAFETY: a message just retrieved for a window on this thread.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            continue;
        }
        match msg.message {
            MSG_MOUSE => {
                let current = HOOKED.with(|h| h.borrow().generation);
                if msg.lParam.0 as usize == current {
                    set_mouse(&mut mouse, msg.wParam.0 != 0);
                }
            }
            MSG_LONE => {
                let _ = lone.send(());
            }
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
                        let last_event = LAST_EVENT_MS.with(|t| t.get());
                        let last_raw = LAST_RAW_KEY_MS.with(|t| t.get());
                        // Never reinstall in the middle of a press.
                        !win_held() && !holding(now_ms()) && watchdog.should_reinstall(now_ms(), last_event, last_raw)
                    }
                } else {
                    false
                };
                if attempt {
                    if reinstall(&mut keyboard, &mut mouse) {
                        failures = 0;
                        tell(true);
                    } else {
                        failures += 1;
                        if failures == 1 {
                            // SAFETY: a thread timer (no window).
                            retry_timer = unsafe { SetTimer(None, 0, RETRY_MS, None) };
                        } else {
                            tell(false);
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
    if let Some(hwnd) = raw_window {
        let remove = RAWINPUTDEVICE {
            usUsagePage: KEYBOARD_PAGE,
            usUsage: KEYBOARD_USAGE,
            dwFlags: RIDEV_REMOVE,
            hwndTarget: HWND::default(),
        };
        // SAFETY: one valid device entry; then destroying the window this
        // thread made.
        unsafe {
            let _ = RegisterRawInputDevices(&[remove], RAW_DEVICE_SIZE);
            let _ = DestroyWindow(hwnd);
        }
    }
    // `lone` drops here, so the worker ends.
}

/// Raw input's generic desktop page and its keyboard usage.
const KEYBOARD_PAGE: u16 = 1;
const KEYBOARD_USAGE: u16 = 6;
const RAW_DEVICE_SIZE: u32 = std::mem::size_of::<RAWINPUTDEVICE>() as u32;

/// A message-only window on this thread that receives raw keyboard input
/// whatever window is in front. `None` if Windows refuses either step. The
/// helper process has no other raw input, so nothing is taken over.
fn raw_keyboard_window() -> Option<HWND> {
    let class = w!("FlickWinKeyRawInput");
    let wc = WNDCLASSW { lpfnWndProc: Some(raw_proc), hInstance: instance(), lpszClassName: class, ..Default::default() };
    // SAFETY: a valid class description; registering twice (a second start)
    // fails harmlessly and the existing class is used.
    let hwnd = unsafe {
        RegisterClassW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            None,
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance()),
            None,
        )
    }
    .ok()?;
    let device = RAWINPUTDEVICE {
        usUsagePage: KEYBOARD_PAGE,
        usUsage: KEYBOARD_USAGE,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };
    // SAFETY: one valid device entry for a window this thread owns.
    match unsafe { RegisterRawInputDevices(&[device], RAW_DEVICE_SIZE) } {
        Ok(()) => Some(hwnd),
        Err(_) => {
            // SAFETY: the window just made on this thread.
            let _ = unsafe { DestroyWindow(hwnd) };
            None
        }
    }
}

unsafe extern "system" fn raw_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_INPUT {
        LAST_RAW_KEY_MS.with(|t| t.set(now_ms()));
    }
    // SAFETY: default handling (for WM_INPUT it frees the raw input).
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
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
        h.held = None;
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
            // Never swallowed: a mouse event at most replays the held Win first.
            let _ = feed(ev, false, false, 0);
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
    // The other Win going down while one is held back: both Win keys together
    // mean nothing to Windows, and the held one's up may have gone where the
    // hook can't see. Forget it without replaying, so it can't be left down.
    if is_win(vk) && !up && holding_key(now_ms()).is_some_and(|win| win != vk as u16) {
        forget_hold();
    }
    let mut held = if is_win(vk) && !up && k.dwExtraInfo != FLICK_TAG { held_now(vk) } else { Held::default() };
    if is_win(vk) && !up && k.dwExtraInfo != FLICK_TAG {
        let now = now_ms();
        let last = HOOKED.with(|h| h.try_borrow_mut().ok().and_then(|mut h| h.last_down.replace((vk as u16, now))));
        let last_same = last.and_then(|(win, at)| (win == vk as u16).then_some(at));
        if held.win_already_down && fresh_press(now, last_same) {
            // Windows still counts this Win key as down from a press whose up
            // the hook never saw (Win+L, an administrator window). Release it
            // first (it lands before this event), then treat this as the new
            // press it is.
            // SAFETY: a valid slice of one initialised INPUT.
            unsafe { SendInput(&[tagged_win_up(vk as u16)], std::mem::size_of::<INPUT>() as i32) };
            held.win_already_down = false;
        }
    }
    let Some(ev) = key_event(vk, up, injected, k.dwExtraInfo, held) else { return false };
    let button_held = ev == Ev::WinUp && any_button_down();
    // SAFETY: no arguments.
    let late = is_win(vk) && !up && is_late(unsafe { GetTickCount() }, k.time);
    feed(ev, button_held, late, vk as u16)
}

/// Feeds the state machine, acts on its verdict and on the [`Step`]; true
/// swallows the event. `late`: the event reached the hook too late to hold
/// back (Windows may already have timed the hook out and passed it). `vk` is
/// the event's key (used only to remember which Win key is held).
fn feed(ev: Ev, button_held: bool, late: bool, vk: u16) -> bool {
    // Busy only if a callback re-entered while the loop held it: let it pass.
    let now = now_ms();
    let Some((verdict, generation, step, replay)) = HOOKED.with(|h| {
        h.try_borrow_mut().ok().map(|mut h| {
            drop_stale(&mut h, now);
            let verdict = h.press.on(ev, button_held);
            let held = h.held.is_some();
            let take = match ev {
                Ev::WinDown { .. } => verdict == Verdict::ArmMouse && may_take(),
                Ev::WinUp => held && verdict == Verdict::Lone && may_take(),
                _ => false,
            };
            let step = step(held, ev, verdict, take, late);
            let replay = h.held.map(|(win, _)| win);
            match step {
                Step::Hold => h.held = Some((vk, now)),
                // An auto-repeat: the key is still down.
                Step::Swallow => h.held = h.held.map(|(win, _)| (win, now)),
                Step::ReplayWin | Step::Open | Step::Drop => h.held = None,
                Step::Pass => {}
            }
            (verdict, h.generation, step, replay)
        })
    }) else {
        return false;
    };
    match verdict {
        Verdict::Pass => {}
        Verdict::ArmMouse => post(MSG_MOUSE, 1, generation),
        Verdict::DisarmMouse | Verdict::Lone => post(MSG_MOUSE, 0, generation),
    }
    match step {
        Step::Pass => false,
        Step::Hold | Step::Swallow | Step::Drop => true,
        Step::ReplayWin => {
            if let Some(win) = replay {
                // Windows handles it before the event this callback is on.
                // SAFETY: a valid slice of one initialised INPUT.
                unsafe { SendInput(&[tagged_win_down(win)], std::mem::size_of::<INPUT>() as i32) };
            }
            false
        }
        Step::Open => {
            post(MSG_LONE, 0, 0);
            true
        }
    }
}

/// Forgets a hold that has gone stale (see [`hold_is_stale`]) without
/// replaying it, and starts the press afresh. With auto-repeat switched off
/// (FilterKeys, some remote desktops) a Win held alone for over 1.5 s goes
/// stale too: a key pressed after that arrives without Win. Better than a Win
/// left down.
fn drop_stale(h: &mut Hooked, now: u64) {
    if h.held.is_some_and(|(_, last)| hold_is_stale(now, last)) {
        reset_hold(h);
    }
}

fn reset_hold(h: &mut Hooked) {
    h.held = None;
    h.press = Press::new();
    post(MSG_MOUSE, 0, h.generation);
}

/// Forgets the held-back Win without replaying it.
fn forget_hold() {
    HOOKED.with(|h| {
        if let Ok(mut h) = h.try_borrow_mut() {
            reset_hold(&mut h);
        }
    });
}

/// The Win key held back from Windows right now (a stale hold is dropped first).
fn holding_key(now: u64) -> Option<u16> {
    HOOKED.with(|h| {
        h.try_borrow_mut().ok().and_then(|mut h| {
            drop_stale(&mut h, now);
            h.held.map(|(win, _)| win)
        })
    })
}

/// Whether a Win-down is held back from Windows right now; busy counts as yes.
fn holding(now: u64) -> bool {
    HOOKED.with(|h| {
        h.try_borrow_mut().map_or(true, |mut h| {
            drop_stale(&mut h, now);
            h.held.is_some()
        })
    })
}

fn may_take() -> bool {
    MAY_TAKE.with(|c| c.get().is_some_and(|f| catch_unwind(AssertUnwindSafe(|| f())).unwrap_or(false)))
}

/// Posts a message to this (the hook) thread's own loop.
fn post(message: u32, wparam: usize, lparam: usize) {
    // SAFETY: a plain post to the current thread's queue.
    let _ = unsafe {
        PostThreadMessageW(GetCurrentThreadId(), message, WPARAM(wparam), LPARAM(lparam as isize))
    };
}

/// The held-back Win-down, tagged as Flick's own. Both Win keys are extended
/// keys, like the real event.
fn tagged_win_down(win: u16) -> INPUT {
    let mut i = key_input(VIRTUAL_KEY(win), false);
    // A down: no other flags.
    i.Anonymous.ki.dwFlags = KEYEVENTF_EXTENDEDKEY;
    i.Anonymous.ki.dwExtraInfo = FLICK_TAG;
    i
}

/// A Win-up tagged as Flick's own, to clear a key Windows wrongly counts as down.
fn tagged_win_up(win: u16) -> INPUT {
    let mut i = key_input(VIRTUAL_KEY(win), true);
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
    fn the_replayed_win_down_is_a_tagged_extended_down() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT_KEYBOARD, KEYEVENTF_KEYUP};
        let i = tagged_win_down(VK_RWIN as u16);
        assert_eq!(i.r#type, INPUT_KEYBOARD);
        // SAFETY: a keyboard INPUT.
        let k = unsafe { i.Anonymous.ki };
        assert_eq!(k.wVk.0, 0x5C);
        assert_eq!(k.dwExtraInfo, FLICK_TAG);
        assert!(k.dwFlags.contains(KEYEVENTF_EXTENDEDKEY));
        assert!(!k.dwFlags.contains(KEYEVENTF_KEYUP));
    }
}
