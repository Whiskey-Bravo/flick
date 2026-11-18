//! Tray icon and its menu: Open, Settings, Edit config, Reload, Start at login, Quit.

use std::sync::Mutex;

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;

use crate::{launch, settings_window, window};

/// The "Start at login" menu item, in managed state.
struct LoginItem(CheckMenuItem<tauri::Wry>);

/// The tray menu and the "Update to x.y" item in it while one is shown.
struct UpdateItem {
    menu: Menu<tauri::Wry>,
    item: Mutex<Option<MenuItem<tauri::Wry>>>,
}

const TRAY_ID: &str = "flick";
const DEFAULT_TOOLTIP: &str = "Flick";
const IGNORED_TOOLTIP: &str = "last press ignored, a full-screen app was in front";
const IGNORED_ITEM: &str = "Last press ignored: a full-screen app was in front";
/// The dot on the icon while the hotkey does not work: #F5A524.
const DOT: [u8; 4] = [0xF5, 0xA5, 0x24, 0xFF];

/// Builds the tray; `error` is the bar hotkey's error at start. `on_reload`
/// runs on the main thread when `Reload` is picked and must not block it.
pub fn build(
    app: &AppHandle,
    error: Option<&str>,
    on_reload: impl Fn(&AppHandle) + Send + Sync + 'static,
) -> tauri::Result<()> {
    let start_at_login = app.autolaunch().is_enabled().unwrap_or(false);
    let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let edit = MenuItem::with_id(app, "edit_config", "Edit config", true, None::<&str>)?;
    let reload = MenuItem::with_id(app, "reload", "Reload", true, None::<&str>)?;
    let login =
        CheckMenuItem::with_id(app, "start_at_login", "Start at login", true, start_at_login, None::<&str>)?;
    // Kept so the Settings window can move the check mark.
    app.manage(LoginItem(login.clone()));
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &settings, &edit, &reload, &login, &separator, &quit])?;

    app.manage(UpdateItem { menu: menu.clone(), item: Mutex::new(None) });

    let base = app.default_window_icon();
    let plain = base.map(|icon| Image::new_owned(icon.rgba().to_vec(), icon.width(), icon.height()));
    let dotted =
        base.map(|icon| Image::new_owned(with_dot(icon.rgba(), icon.width(), icon.height()), icon.width(), icon.height()));
    let first_icon = if error.is_some() { dotted.clone() } else { plain.clone() };
    app.manage(TrayState {
        menu: menu.clone(),
        plain,
        dotted,
        now: Mutex::new(Now { error: error.map(str::to_string), ignored: false, item: None }),
    });

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip(tooltip_text(error, false))
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "open" => window::show(app),
            "settings" => settings_window::open_soon(app),
            "edit_config" => {
                // ShellExecute can block (it waits for the shell); keep it off the main thread.
                std::thread::spawn(|| {
                    let _ = launch::open_config();
                });
            }
            "reload" => on_reload(app),
            "update" => {
                // Installing blocks for as long as the download takes.
                let app = app.clone();
                std::thread::spawn(move || app.state::<crate::AppState>().updates.install());
            }
            "start_at_login" => set_start_at_login(app, &login),
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = first_icon {
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

/// The tray tooltip: what is wrong with the hotkey, and whether the last press was ignored.
pub fn tooltip_text(error: Option<&str>, ignored: bool) -> String {
    match (error, ignored) {
        (None, false) => DEFAULT_TOOLTIP.to_string(),
        (Some(e), false) => format!("{DEFAULT_TOOLTIP} \u{2014} {e}"),
        (None, true) => format!("{DEFAULT_TOOLTIP}: {IGNORED_TOOLTIP}"),
        (Some(e), true) => format!("{DEFAULT_TOOLTIP} \u{2014} {e}; {IGNORED_TOOLTIP}"),
    }
}

/// A copy of the RGBA icon with a filled amber circle (40 % of the width
/// across) touching the bottom-right corner; every pixel outside it is unchanged.
pub fn with_dot(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let radius = width as f32 * 0.2;
    let (cx, cy) = (width as f32 - radius, height as f32 - radius);
    let mut out = rgba.to_vec();
    for y in 0..height {
        for x in 0..width {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            if dx * dx + dy * dy <= radius * radius {
                let i = ((y * width + x) * 4) as usize;
                if let Some(pixel) = out.get_mut(i..i + 4) {
                    pixel.copy_from_slice(&DOT);
                }
            }
        }
    }
    out
}

/// Tray state that changes at run time, in managed state.
struct TrayState {
    menu: Menu<tauri::Wry>,
    plain: Option<Image<'static>>,
    dotted: Option<Image<'static>>,
    now: Mutex<Now>,
}

struct Now {
    error: Option<String>,
    ignored: bool,
    /// The "Last press ignored" menu item while one is shown.
    item: Option<MenuItem<tauri::Wry>>,
}

/// Puts the tooltip and the icon in line with `now`. No-op until the tray exists.
fn refresh(app: &AppHandle, state: &TrayState, now: &Now) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let _ = tray.set_tooltip(Some(tooltip_text(now.error.as_deref(), now.ignored)));
    let icon = if now.error.is_some() { state.dotted.clone() } else { state.plain.clone() };
    if let Some(icon) = icon {
        let _ = tray.set_icon(Some(icon));
    }
}

/// The hotkey's error (the tooltip says it, the icon gets a dot), or `None`
/// when it registered. No-op until the tray exists.
pub fn set_error(app: &AppHandle, error: Option<&str>) {
    let Some(state) = app.try_state::<TrayState>() else { return };
    let mut now = state.now.lock().unwrap_or_else(|e| e.into_inner());
    if now.error.as_deref() == error {
        return;
    }
    now.error = error.map(str::to_string);
    refresh(app, &state, &now);
}

/// Whether the last press of the hotkey was ignored because a full-screen app
/// was in front: the tooltip says so and the menu gets a line at the top. Called
/// on every show, so it does nothing when the value did not change. No-op until
/// the tray exists.
pub fn set_ignored(app: &AppHandle, ignored: bool) {
    let Some(state) = app.try_state::<TrayState>() else { return };
    let mut now = state.now.lock().unwrap_or_else(|e| e.into_inner());
    if now.ignored == ignored {
        return;
    }
    now.ignored = ignored;
    if ignored {
        if let Ok(item) = MenuItem::with_id(app, "ignored", IGNORED_ITEM, false, None::<&str>) {
            if state.menu.insert(&item, 0).is_ok() {
                now.item = Some(item);
            }
        }
    } else if let Some(item) = now.item.take() {
        let _ = state.menu.remove(&item);
    }
    refresh(app, &state, &now);
}

/// Puts "Update to <version>" at the top of the menu, or takes it out on `None`.
/// No-op until the tray exists.
pub fn set_update(app: &AppHandle, version: Option<&str>) {
    let Some(holder) = app.try_state::<UpdateItem>() else { return };
    let mut current = holder.item.lock().unwrap_or_else(|e| e.into_inner());
    match version {
        Some(v) => {
            let text = format!("Update to {v}");
            if let Some(item) = current.as_ref() {
                let _ = item.set_text(text);
            } else if let Ok(item) = MenuItem::with_id(app, "update", text, true, None::<&str>) {
                if holder.menu.insert(&item, 0).is_ok() {
                    *current = Some(item);
                }
            }
        }
        None => {
            if let Some(item) = current.take() {
                let _ = holder.menu.remove(&item);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_says_what_is_wrong() {
        assert_eq!(tooltip_text(None, false), "Flick");
        assert_eq!(
            tooltip_text(Some("hotkey \"X\" is not a valid shortcut"), false),
            "Flick \u{2014} hotkey \"X\" is not a valid shortcut"
        );
        assert_eq!(
            tooltip_text(None, true),
            "Flick: last press ignored, a full-screen app was in front"
        );
        assert_eq!(
            tooltip_text(Some("hotkey \"X\" is in use"), true),
            "Flick \u{2014} hotkey \"X\" is in use; last press ignored, a full-screen app was in front"
        );
    }

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * width + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    #[test]
    fn the_dot_sits_in_the_bottom_right_corner() {
        let icon = vec![0u8; 32 * 32 * 4];
        let dotted = with_dot(&icon, 32, 32);
        assert_eq!(dotted.len(), icon.len());
        assert_eq!(pixel(&dotted, 32, 25, 25), [0xF5, 0xA5, 0x24, 0xFF]);
        assert_eq!(pixel(&dotted, 32, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&dotted, 32, 10, 25), [0, 0, 0, 0]);
        // Touches the corner: the pixel under the circle's far edge is amber, the very corner is not.
        assert_eq!(pixel(&dotted, 32, 31, 31), [0, 0, 0, 0]);
    }

    #[test]
    fn only_the_dot_changes() {
        let icon: Vec<u8> = (0..32 * 32 * 4).map(|i| (i % 251) as u8).collect();
        let dotted = with_dot(&icon, 32, 32);
        let changed = (0..32 * 32).filter(|p| icon[p * 4..p * 4 + 4] != dotted[p * 4..p * 4 + 4]).count();
        // A circle of radius 6.4 covers about 129 pixels.
        assert!((100..160).contains(&changed), "{changed}");
        assert_eq!(&icon[..4], &dotted[..4]);
    }

    #[test]
    fn the_dot_scales_with_the_icon() {
        let icon = vec![0u8; 16 * 16 * 4];
        let dotted = with_dot(&icon, 16, 16);
        assert_eq!(pixel(&dotted, 16, 12, 12), [0xF5, 0xA5, 0x24, 0xFF]);
        assert_eq!(pixel(&dotted, 16, 0, 0), [0, 0, 0, 0]);
    }
}
