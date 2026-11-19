//! Gives the bar's WebView2 memory back to Windows while the bar is hidden:
//! after `LOW_AFTER` hidden, its `MemoryUsageTargetLevel` goes to Low; the
//! next show puts it back to Normal first. Only the bar's WebView; the
//! Settings window is left alone. Any failure (older runtime, no interface)
//! is silent.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

/// How long the bar has to stay hidden before its WebView is told to use less.
pub const LOW_AFTER: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Normal,
    Low,
}

/// What the level should be, decided without touching any WebView. Pure.
pub struct Tracker {
    generation: u64,
    hidden_since: Option<Instant>,
    level: Level,
}

impl Tracker {
    /// The bar starts hidden: counted as hidden since `now`.
    pub fn new(now: Instant) -> Tracker {
        Tracker { generation: 0, hidden_since: Some(now), level: Level::Normal }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_hidden(&self) -> bool {
        self.hidden_since.is_some()
    }

    /// The level the WebView should have now.
    pub fn level(&self) -> Level {
        self.level
    }

    /// The bar was hidden. Already hidden: nothing changes and the current
    /// generation comes back. Otherwise a new generation starts.
    pub fn on_hide(&mut self, now: Instant) -> u64 {
        if self.hidden_since.is_none() {
            self.generation += 1;
            self.hidden_since = Some(now);
        }
        self.generation
    }

    /// The bar is being shown: `Some(Normal)` only when the level was Low.
    /// Pending checks are void.
    pub fn on_show(&mut self) -> Option<Level> {
        self.generation += 1;
        self.hidden_since = None;
        if self.level == Level::Low {
            self.level = Level::Normal;
            Some(Level::Normal)
        } else {
            None
        }
    }

    /// A delayed check: `Some(Low)` when `generation` is still current, the bar
    /// is still hidden, `LOW_AFTER` has passed and the level is not Low yet.
    pub fn on_timer(&mut self, generation: u64, now: Instant) -> Option<Level> {
        let since = self.hidden_since?;
        if generation != self.generation
            || self.level == Level::Low
            || now.saturating_duration_since(since) < LOW_AFTER
        {
            return None;
        }
        self.level = Level::Low;
        Some(Level::Low)
    }
}

/// The tracker in app state. The lock is only ever held to decide, never
/// across a WebView or window call.
pub struct State(Mutex<Tracker>);

impl State {
    fn lock(&self) -> std::sync::MutexGuard<'_, Tracker> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Registers the state and starts the first delayed check: the bar starts hidden.
pub fn start(app: &AppHandle) {
    let tracker = Tracker::new(Instant::now());
    let generation = tracker.generation();
    app.manage(State(Mutex::new(tracker)));
    spawn_check(app, generation);
}

/// The bar was hidden (`window::hide`, main thread). A hide while already
/// hidden starts no new check.
pub fn hidden(app: &AppHandle) {
    let Some(state) = app.try_state::<State>() else { return };
    let new_generation = {
        let mut t = state.lock();
        let fresh = !t.is_hidden();
        let generation = t.on_hide(Instant::now());
        fresh.then_some(generation)
    };
    if let Some(generation) = new_generation {
        spawn_check(app, generation);
    }
}

/// The bar is about to be shown (`window::show_as`, before the window shows).
/// One short lock; a WebView call only when the level was Low.
pub fn shown(app: &AppHandle) {
    let Some(state) = app.try_state::<State>() else { return };
    let change = state.lock().on_show();
    if change.is_some() {
        apply(app, "webview.normal");
    }
}

fn spawn_check(app: &AppHandle, generation: u64) {
    let app = app.clone();
    let _ = std::thread::Builder::new().name("flick-webview-memory".into()).spawn(move || {
        std::thread::sleep(LOW_AFTER);
        let Some(state) = app.try_state::<State>() else { return };
        let change = state.lock().on_timer(generation, Instant::now());
        if change.is_some() {
            apply(&app, "webview.low");
        }
    });
}

/// Sets the bar WebView to the level the tracker wants *at that moment* (read
/// on the main thread, under a short lock released before the call), so a
/// late-running request can never undo a newer one. No lock is held across
/// the WebView call, and nothing here waits for the main thread.
fn apply(app: &AppHandle, mark: &'static str) {
    let Some(w) = app.get_webview_window(crate::window::LABEL) else { return };
    let handle = app.clone();
    let _ = w.with_webview(move |webview| {
        let level = match handle.try_state::<State>() {
            Some(state) => state.lock().level(),
            None => return,
        };
        set_level(&webview, level);
        crate::timing::mark(mark, 0);
    });
}

fn set_level(webview: &tauri::webview::PlatformWebview, level: Level) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
    };
    let target = match level {
        Level::Low => COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW,
        Level::Normal => COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
    };
    // SAFETY: `memory_interface`'s conditions hold: this runs inside
    // `with_webview`, on the main thread, with the bar's own controller.
    unsafe {
        if let Some(view) = memory_interface(webview) {
            let _ = view.SetMemoryUsageTargetLevel(target);
        }
    }
}

/// `ICoreWebView2_19` of the controller's WebView, or `None` when the runtime
/// is too old for it. The `windows-core` that `webview2-com` is built on is
/// not the one this crate uses and is not re-exported, so its `cast` is out of
/// reach: this is the same `QueryInterface` call, made by hand.
///
/// # Safety
/// Main thread, with a live controller.
unsafe fn memory_interface(
    webview: &tauri::webview::PlatformWebview,
) -> Option<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_19> {
    use std::ffi::c_void;
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_19;
    // IID of ICoreWebView2_19, 6921f954-79b0-437f-a997-c85811897c68, as a GUID in memory.
    const IID: [u8; 16] = [
        0x54, 0xf9, 0x21, 0x69, 0xb0, 0x79, 0x7f, 0x43, 0xa9, 0x97, 0xc8, 0x58, 0x11, 0x89, 0x7c, 0x68,
    ];
    let core = webview.controller().CoreWebView2().ok()?;
    // A COM interface is a pointer to a pointer to its vtable, which starts
    // with IUnknown's QueryInterface, AddRef, Release.
    let raw: *mut c_void = std::mem::transmute_copy(&core);
    if raw.is_null() {
        return None;
    }
    type QueryInterface = unsafe extern "system" fn(*mut c_void, *const [u8; 16], *mut *mut c_void) -> i32;
    let vtable = *(raw as *const *const QueryInterface);
    let query = *vtable;
    let mut found: *mut c_void = std::ptr::null_mut();
    if query(raw, &IID, &mut found) < 0 || found.is_null() {
        return None;
    }
    // The call added a reference; the wrapper's drop releases it.
    Some(std::mem::transmute::<*mut c_void, ICoreWebView2_19>(found))
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn low_comes_after_twenty_seconds_hidden_and_only_once() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        t.on_show();
        let g = t.on_hide(t0);
        assert_eq!(t.on_timer(g, t0 + 19 * S), None);
        assert_eq!(t.on_timer(g, t0 + 20 * S), Some(Level::Low));
        assert_eq!(t.on_timer(g, t0 + 25 * S), None, "already low");
    }

    #[test]
    fn a_show_before_the_time_cancels_and_costs_nothing() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        t.on_show();
        let g = t.on_hide(t0);
        assert_eq!(t.on_show(), None, "still Normal");
        assert_eq!(t.on_timer(g, t0 + 20 * S), None);
    }

    #[test]
    fn a_show_after_low_goes_back_to_normal_once() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        t.on_show();
        let g = t.on_hide(t0);
        assert_eq!(t.on_timer(g, t0 + 20 * S), Some(Level::Low));
        assert_eq!(t.level(), Level::Low);
        assert_eq!(t.on_show(), Some(Level::Normal));
        assert_eq!(t.level(), Level::Normal);
        assert_eq!(t.on_show(), None);
    }

    #[test]
    fn an_old_check_does_not_count_for_a_later_hide() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        t.on_show();
        let first = t.on_hide(t0);
        t.on_show();
        let second = t.on_hide(t0 + 10 * S);
        assert_ne!(first, second);
        assert_eq!(t.on_timer(first, t0 + 20 * S), None);
        assert_eq!(t.on_timer(second, t0 + 30 * S), Some(Level::Low));
    }

    #[test]
    fn a_fresh_tracker_counts_as_hidden_since_its_creation() {
        let start = Instant::now();
        let mut t = Tracker::new(start);
        let g = t.generation();
        assert_eq!(t.on_timer(g, start + 19 * S), None);
        assert_eq!(t.on_timer(g, start + 20 * S), Some(Level::Low));
    }

    #[test]
    fn hiding_again_while_hidden_keeps_the_generation_and_the_start() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        let g = t.generation();
        assert!(t.is_hidden());
        assert_eq!(t.on_hide(t0 + 15 * S), g);
        assert_eq!(t.on_timer(g, t0 + 20 * S), Some(Level::Low), "counted from the first hide");
    }

    #[test]
    fn a_timer_while_shown_does_nothing() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        let g = t.generation();
        t.on_show();
        assert_eq!(t.on_timer(g, t0 + 60 * S), None);
        assert_eq!(t.on_timer(t.generation(), t0 + 60 * S), None);
    }
}
