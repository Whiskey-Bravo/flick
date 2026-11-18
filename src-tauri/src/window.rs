//! The `bar` window: created once by Tauri at startup, then only shown and hidden.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow, WindowEvent};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsHungAppWindow, SetForegroundWindow,
};
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUERY_USER_NOTIFICATION_STATE, QUNS_RUNNING_D3D_FULL_SCREEN,
};

use crate::provider::ResultItem;

pub const LABEL: &str = "bar";

/// Focus-loss events arriving this soon after `show` belong to the show/focus
/// sequence itself and are ignored, so they cannot hide the bar straight away.
const BLUR_GRACE: Duration = Duration::from_millis(250);

static LAST_SHOW: Mutex<Option<Instant>> = Mutex::new(None);

/// How many times the bar has been shown (`show`, not `reveal`): each show
/// starts a new session.
static SHOWS: AtomicU64 = AtomicU64::new(0);

/// The current show count; see [`SHOWS`].
pub fn shows() -> u64 {
    SHOWS.load(Ordering::SeqCst)
}

fn bar(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL)
}

/// Hotkey path. Showing is skipped while an exclusive full-screen (D3D) app is
/// running: taking the foreground from it could minimise it.
pub fn toggle(app: &AppHandle) {
    let Some(w) = bar(app) else { return };
    if w.is_visible().unwrap_or(false) {
        hide(app);
    } else if hotkey_may_show(notification_state()) {
        show(app);
    }
}

/// Win-key path ([`toggle`] for a lone Win press). The press was held back
/// from Windows, so it gave Flick no right to take the foreground; if
/// Windows keeps another window in front, the bar's thread joins that
/// window's input queue for a moment and takes it from there.
/// Main thread only (the bar's window thread, for `SetFocus`).
pub fn toggle_from_win_key(app: &AppHandle) {
    let Some(w) = bar(app) else { return };
    if w.is_visible().unwrap_or(false) {
        hide(app);
        return;
    }
    if !hotkey_may_show(notification_state()) {
        return;
    }
    show(app);
    if let Ok(h) = w.hwnd() {
        if take_foreground(HWND(h.0 as _)) {
            // `SetFocus` reached only the frame: focus the page's input the
            // way the normal path does.
            let _ = w.set_focus();
        }
    }
}

/// Makes `hwnd` (a window of this thread) the foreground window with focus.
/// True when it had to join the front window's input queue to do so.
fn take_foreground(hwnd: HWND) -> bool {
    // SAFETY: plain calls on window handles and thread ids; every result is
    // checked, and the input queues are detached again before returning.
    unsafe {
        if GetForegroundWindow() == hwnd {
            return false;
        }
        let _ = SetForegroundWindow(hwnd);
        let front = GetForegroundWindow();
        if front == hwnd {
            return false;
        }
        let front_thread = GetWindowThreadProcessId(front, None);
        let me = GetCurrentThreadId();
        // A hung window's thread could stall this one while attached.
        let hung = !front.is_invalid() && IsHungAppWindow(front).as_bool();
        if !may_attach(front_thread, me, hung) {
            return false;
        }
        if AttachThreadInput(me, front_thread, true).as_bool() {
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(hwnd));
            let _ = AttachThreadInput(me, front_thread, false);
            return true;
        }
        false
    }
}

/// Whether the bar's thread (`me`) may join the input queue of the front
/// window's thread: not when there is none, it is this thread, or the front
/// window is hung.
fn may_attach(front_thread: u32, me: u32, hung: bool) -> bool {
    front_thread != 0 && front_thread != me && !hung
}

/// `None` when the state cannot be read; that never blocks showing.
pub(crate) fn notification_state() -> Option<QUERY_USER_NOTIFICATION_STATE> {
    // SAFETY: no pointers or preconditions; the wrapper owns the out-parameter.
    unsafe { SHQueryUserNotificationState() }.ok()
}

/// Only exclusive full-screen blocks the hotkey. Borderless full-screen and
/// presentation mode (`QUNS_BUSY`, `QUNS_PRESENTATION_MODE`) do not.
pub(crate) fn hotkey_may_show(state: Option<QUERY_USER_NOTIFICATION_STATE>) -> bool {
    state != Some(QUNS_RUNNING_D3D_FULL_SCREEN)
}

/// What the page is told when the bar is shown.
enum Shown<'a> {
    /// `bar:shown`: a fresh, empty bar.
    Fresh,
    /// `bar:armed` with the item: it waits for the second Enter.
    Armed(&'a ResultItem),
    /// `bar:reopen` with `{ text, error }`: the text is put back with the error under it.
    Reopen { text: &'a str, error: &'a str },
    /// `bar:step` with `{ prompt, scope }`: the bar opens in that pick list.
    Step { prompt: &'a str, scope: &'a str },
}

/// Centre horizontally, top edge at 25% of the monitor under the mouse cursor;
/// show; focus; emit `bar:shown`, then `config:error` with the stored error (or null).
pub fn show(app: &AppHandle) {
    show_as(app, Shown::Fresh);
}

/// Like [`show`], but the bar opens with `item` armed (a direct hotkey on an
/// item that asks for a second Enter): `bar:armed` instead of `bar:shown`.
pub fn show_armed(app: &AppHandle, item: &ResultItem) {
    show_as(app, Shown::Armed(item));
}

/// Like [`show`], but the bar opens with `text` and `error` under it (a
/// workspace whose items did not all open): `bar:reopen` instead of `bar:shown`.
/// An empty `error` shows none (a direct hotkey on an item that needs typing).
pub fn reopen(app: &AppHandle, text: &str, error: &str) {
    show_as(app, Shown::Reopen { text, error });
}

/// Like [`show`], but the bar opens in the pick list `scope` under `prompt`,
/// as after Enter on its row (a direct hotkey on an item that asks to choose).
pub fn show_step(app: &AppHandle, prompt: &str, scope: &str) {
    show_as(app, Shown::Step { prompt, scope });
}

fn show_as(app: &AppHandle, shown: Shown) {
    // Every open starts fresh: a `v` or `qr` read that raced the last hide
    // is dropped too.
    if let Some(state) = app.try_state::<crate::AppState>() {
        state.providers.on_hide();
    }
    let Some(w) = bar(app) else { return };
    // Before the bar takes focus: `v`'s Enter pastes into this window. When
    // the bar is already up (tray Open, a second launch) the foreground is
    // the bar itself, so the window remembered when it opened stays.
    if !w.is_visible().unwrap_or(false) {
        crate::paste::remember_foreground();
    }
    SHOWS.fetch_add(1, Ordering::SeqCst);
    set_weather_hidden(app, false);
    if let Some(pos) = placement(app, &w) {
        let _ = w.set_position(pos);
    }
    reveal_window(&w);
    let _ = match shown {
        Shown::Fresh => app.emit_to(LABEL, "bar:shown", ()),
        Shown::Armed(item) => app.emit_to(LABEL, "bar:armed", item),
        Shown::Reopen { text, error } => {
            app.emit_to(LABEL, "bar:reopen", serde_json::json!({ "text": text, "error": error }))
        }
        Shown::Step { prompt, scope } => {
            app.emit_to(LABEL, "bar:step", serde_json::json!({ "prompt": prompt, "scope": scope }))
        }
    };
    // Re-send the stored error so one raised before the page listened is not lost.
    if let Some(state) = app.try_state::<crate::AppState>() {
        // Open windows and the audio state are read off this thread.
        state.providers.refresh_on_show();
        let _ = app.emit_to(LABEL, "config:error", state.config_error());
    }
}

/// Show and focus where it was, without `bar:shown`, so the page keeps its
/// state (used to bring back a failed activation's error).
pub fn reveal(app: &AppHandle) {
    set_weather_hidden(app, false);
    if let Some(w) = bar(app) {
        reveal_window(&w);
    }
}

fn reveal_window(w: &WebviewWindow) {
    *LAST_SHOW.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    let _ = w.show();
    let _ = w.set_focus();
}

/// Tells the weather store whether the bar is hidden (memory only).
fn set_weather_hidden(app: &AppHandle, hidden: bool) {
    if let Some(state) = app.try_state::<crate::AppState>() {
        state.providers.weather.set_hidden(hidden);
    }
}

pub fn hide(app: &AppHandle) {
    // A weather city still waiting for the typing pause is forgotten, so a
    // hidden bar never sends a request.
    set_weather_hidden(app, true);
    // Clipboard history and clipboard text are held only while the bar is open.
    if let Some(state) = app.try_state::<crate::AppState>() {
        state.providers.on_hide();
    }
    let _ = app.emit_to(LABEL, "bar:hidden", ());
    if let Some(w) = bar(app) {
        let _ = w.hide();
    }
}

/// Hide on focus loss; turn close requests (Alt+F4) into hides so the window
/// is never destroyed.
pub fn install_handlers(app: &AppHandle) {
    let Some(w) = bar(app) else { return };
    let handle = app.clone();
    w.on_window_event(move |event| match event {
        WindowEvent::Focused(false) if !within_blur_grace() => hide(&handle),
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            hide(&handle);
        }
        _ => {}
    });
}

fn within_blur_grace() -> bool {
    LAST_SHOW
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|t| t.elapsed() < BLUR_GRACE)
}

fn placement(app: &AppHandle, w: &WebviewWindow) -> Option<PhysicalPosition<i32>> {
    let cursor = app.cursor_position().ok();
    let monitor = cursor
        .and_then(|c| app.monitor_from_point(c.x, c.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten())?;
    // The window's logical width, scaled for the target monitor (they can differ in DPI).
    let logical_width = w.outer_size().ok()?.width as f64 / w.scale_factor().ok()?;
    let width = (logical_width * monitor.scale_factor()).round() as u32;
    let (x, y) = bar_position(
        (monitor.position().x, monitor.position().y),
        (monitor.size().width, monitor.size().height),
        width,
    );
    Some(PhysicalPosition::new(x, y))
}

/// Top-left corner for a window `width` wide on a monitor at `origin` with `size`
/// (all physical pixels): horizontally centred, top edge at 25% of the height.
fn bar_position(origin: (i32, i32), size: (u32, u32), width: u32) -> (i32, i32) {
    let x = origin.0 + (size.0 as i32 - width as i32) / 2;
    let y = origin.1 + (size.1 / 4) as i32;
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_attaches_to_a_hung_missing_or_own_thread() {
        assert!(may_attach(20, 10, false));
        assert!(!may_attach(20, 10, true), "the front window is hung");
        assert!(!may_attach(0, 10, false), "no front window");
        assert!(!may_attach(10, 10, false), "the bar's own thread");
    }

    #[test]
    fn only_exclusive_full_screen_blocks_hotkey_show() {
        use windows::Win32::UI::Shell::{
            QUNS_ACCEPTS_NOTIFICATIONS, QUNS_APP, QUNS_BUSY, QUNS_NOT_PRESENT,
            QUNS_PRESENTATION_MODE, QUNS_QUIET_TIME,
        };
        assert!(!hotkey_may_show(Some(QUNS_RUNNING_D3D_FULL_SCREEN)));
        for state in [
            QUNS_NOT_PRESENT,
            QUNS_BUSY,
            QUNS_PRESENTATION_MODE,
            QUNS_ACCEPTS_NOTIFICATIONS,
            QUNS_QUIET_TIME,
            QUNS_APP,
        ] {
            assert!(hotkey_may_show(Some(state)), "{state:?}");
        }
        assert!(hotkey_may_show(None));
    }

    #[test]
    fn centred_at_quarter_height_on_primary() {
        assert_eq!(bar_position((0, 0), (1920, 1080), 680), (620, 270));
    }

    #[test]
    fn offset_monitor_is_respected() {
        // A second monitor to the left of the primary one.
        assert_eq!(bar_position((-2560, 0), (2560, 1440), 1020), (-2560 + 770, 360));
    }

    #[test]
    fn window_wider_than_monitor_still_centres() {
        assert_eq!(bar_position((0, 0), (600, 800), 680), (-40, 200));
    }
}
