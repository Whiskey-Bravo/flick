//! The Settings window: one instance, built on demand, destroyed when closed.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::settings::WINDOW;

/// Focuses the Settings window, or builds it. Callable from any thread except the
/// main one while it is blocked waiting (menu handlers hand this to a thread).
pub fn open(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(WINDOW) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::App("settings.html".into()))
        .title("Flick Settings")
        .inner_size(820.0, 560.0)
        .min_inner_size(640.0, 440.0)
        .decorations(true)
        .resizable(true)
        .skip_taskbar(false)
        .center()
        .build();
    match built {
        Ok(w) => {
            // A pause for hotkey capture must never outlive the window.
            let handle = app.clone();
            w.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Destroyed) {
                    let app = handle.clone();
                    std::thread::spawn(move || crate::hotkey::set_paused(&app, false));
                }
            });
            let _ = w.set_focus();
        }
        Err(e) => eprintln!("could not open the Settings window: {e}"),
    }
}

/// Opens the window off the calling thread (for the tray's menu handler, which runs
/// on the main thread, where building a window would wait on itself).
pub fn open_soon(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || open(&app));
}
