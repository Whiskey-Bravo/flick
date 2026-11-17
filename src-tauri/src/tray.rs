//! Tray icon and its menu: Open, Edit config, Reload, Start at login, Quit.

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;

use crate::{launch, window};

/// The "Start at login" menu item, in managed state.
struct LoginItem(CheckMenuItem<tauri::Wry>);

const TRAY_ID: &str = "flick";
pub const DEFAULT_TOOLTIP: &str = "Flick";

/// Builds the tray. `on_reload` runs on the main thread when `Reload` is picked
/// and must not block it.
pub fn build(
    app: &AppHandle,
    tooltip: &str,
    on_reload: impl Fn(&AppHandle) + Send + Sync + 'static,
) -> tauri::Result<()> {
    let start_at_login = app.autolaunch().is_enabled().unwrap_or(false);
    let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
    let edit = MenuItem::with_id(app, "edit_config", "Edit config", true, None::<&str>)?;
    let reload = MenuItem::with_id(app, "reload", "Reload", true, None::<&str>)?;
    let login =
        CheckMenuItem::with_id(app, "start_at_login", "Start at login", true, start_at_login, None::<&str>)?;
    // Kept so the Settings window can move the check mark.
    app.manage(LoginItem(login.clone()));
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &edit, &reload, &login, &separator, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip(tooltip)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "open" => window::show(app),
            "edit_config" => {
                // ShellExecute can block (it waits for the shell); keep it off the main thread.
                std::thread::spawn(|| {
                    let _ = launch::open_config();
                });
            }
            "reload" => on_reload(app),
            "start_at_login" => set_start_at_login(app, &login),
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

/// The menu toggles the check mark on click; make autostart follow it, and put
/// the mark back to the real state if that fails.
fn set_start_at_login(app: &AppHandle, item: &CheckMenuItem<tauri::Wry>) {
    let manager = app.autolaunch();
    let wanted = item.is_checked().unwrap_or(false);
    let _ = if wanted { manager.enable() } else { manager.disable() };
    let actual = manager.is_enabled().unwrap_or(false);
    if actual != wanted {
        let _ = item.set_checked(actual);
    }
    let _ = app.emit_to(crate::settings::WINDOW, "settings:changed", ());
}

/// Puts the tray's "Start at login" check mark on `on`. No-op until the tray exists.
pub fn set_login_checked(app: &AppHandle, on: bool) {
    if let Some(item) = app.try_state::<LoginItem>() {
        let _ = item.0.set_checked(on);
    }
}

/// No-op until the tray exists.
pub fn set_tooltip(app: &AppHandle, tooltip: &str) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(tooltip));
    }
}
