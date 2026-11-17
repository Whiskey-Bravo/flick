//! The `bar` window: created once by Tauri at startup, then only shown and hidden.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow, WindowEvent};
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUERY_USER_NOTIFICATION_STATE, QUNS_RUNNING_D3D_FULL_SCREEN,
};

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

/// `None` when the state cannot be read; that never blocks showing.
fn notification_state() -> Option<QUERY_USER_NOTIFICATION_STATE> {
    // SAFETY: no pointers or preconditions; the wrapper owns the out-parameter.
    unsafe { SHQueryUserNotificationState() }.ok()
}

/// Only exclusive full-screen blocks the hotkey. Borderless full-screen and
/// presentation mode (`QUNS_BUSY`, `QUNS_PRESENTATION_MODE`) do not.
fn hotkey_may_show(state: Option<QUERY_USER_NOTIFICATION_STATE>) -> bool {
    state != Some(QUNS_RUNNING_D3D_FULL_SCREEN)
}

/// Centre horizontally, top edge at 25% of the monitor under the mouse cursor;
/// show; focus; emit `bar:shown`, then `config:error` with the stored error (or null).
pub fn show(app: &AppHandle) {
    let Some(w) = bar(app) else { return };
    SHOWS.fetch_add(1, Ordering::SeqCst);
    set_weather_hidden(app, false);
    if let Some(pos) = placement(app, &w) {
        let _ = w.set_position(pos);
    }
    reveal_window(&w);
    let _ = app.emit_to(LABEL, "bar:shown", ());
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
