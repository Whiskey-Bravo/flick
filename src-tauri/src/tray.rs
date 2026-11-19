//! Tray icon and its menu: Open, Settings, Edit config, Reload, Start at login, Quit.

use std::sync::Mutex;

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};

use crate::{launch, settings_window, window};

/// The "Start at login" menu item, in managed state.
struct LoginItem(CheckMenuItem<tauri::Wry>);

/// The tray menu and the "Update to x.y" item in it while one is shown.
struct UpdateItem {
    menu: Menu<tauri::Wry>,
    item: Mutex<Option<MenuItem<tauri::Wry>>>,
    /// The version last asked for; `item` follows it on the main thread.
    wanted: Mutex<Option<String>>,
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
    let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let edit = MenuItem::with_id(app, "edit_config", "Edit config", true, None::<&str>)?;
    let reload = MenuItem::with_id(app, "reload", "Reload", true, None::<&str>)?;
    // Task Scheduler and the registry block, so the real state is read in the
    // background after the tray exists (`lib.rs`), which then ticks the item.
    let login =
        CheckMenuItem::with_id(app, "start_at_login", "Start at login", true, false, None::<&str>)?;
    // Kept so the Settings window can move the check mark.
    app.manage(LoginItem(login.clone()));
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &settings, &edit, &reload, &login, &separator, &quit])?;

    app.manage(UpdateItem { menu: menu.clone(), item: Mutex::new(None), wanted: Mutex::new(None) });

    let base = app.default_window_icon();
    let plain = base.map(|icon| Image::new_owned(icon.rgba().to_vec(), icon.width(), icon.height()));
    let dotted =
        base.map(|icon| Image::new_owned(with_dot(icon.rgba(), icon.width(), icon.height()), icon.width(), icon.height()));
    let first_icon = if error.is_some() { dotted.clone() } else { plain.clone() };
    app.manage(TrayState {
        menu: menu.clone(),
        plain,
        dotted,
        want: Mutex::new(Want { error: error.map(str::to_string), ignored: false }),
        applied: Mutex::new(Applied {
            shown: Shown { dotted: error.is_some(), line: false, tooltip: tooltip_text(error, false) },
            item: None,
        }),
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

/// The menu toggles the check mark on click (main thread); make Start at login
/// follow it, off the main thread (Task Scheduler blocks), and put the mark
/// back to the real state afterwards.
fn set_start_at_login(app: &AppHandle, item: &CheckMenuItem<tauri::Wry>) {
    let wanted = item.is_checked().unwrap_or(false);
    let app = app.clone();
    std::thread::spawn(move || {
        // Debug builds only read: they would register the debug exe.
        let actual = match std::env::current_exe() {
            Ok(exe) if !cfg!(debug_assertions) => crate::login_task::set(&crate::login_task::WindowsLoginStart, wanted, &exe),
            Ok(_) => crate::login_task::is_enabled(&crate::login_task::WindowsLoginStart),
            Err(_) => crate::login_task::is_enabled(&crate::login_task::WindowsLoginStart),
        };
        set_login_checked(&app, actual);
        let _ = app.emit_to(crate::settings::WINDOW, "settings:changed", ());
    });
}

/// Takes the icon out of the notification area, or puts it back. Used while
/// the updater starts the installer, which may still fail. Waits for the main
/// thread, so call it with no lock held. No-op until the tray exists.
pub fn set_visible(app: &AppHandle, visible: bool) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_visible(visible);
    }
}

/// Puts the tray's "Start at login" check mark on `on`, on the main thread.
/// No-op until the tray exists.
pub fn set_login_checked(app: &AppHandle, on: bool) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(item) = handle.try_state::<LoginItem>() {
            let _ = item.0.set_checked(on);
        }
    });
}

/// Windows shows at most this many UTF-16 units of a tray tooltip.
const TOOLTIP_MAX: usize = 127;

/// The tray tooltip: what is wrong with the hotkey, and whether the last press
/// was ignored. At most 127 UTF-16 units, ending in an ellipsis when cut.
pub fn tooltip_text(error: Option<&str>, ignored: bool) -> String {
    let text = match (error, ignored) {
        (None, false) => DEFAULT_TOOLTIP.to_string(),
        (Some(e), false) => format!("{DEFAULT_TOOLTIP} \u{2014} {e}"),
        (None, true) => format!("{DEFAULT_TOOLTIP}: {IGNORED_TOOLTIP}"),
        (Some(e), true) => format!("{DEFAULT_TOOLTIP}: {IGNORED_TOOLTIP}; {e}"),
    };
    cut_utf16(&text, TOOLTIP_MAX)
}

/// `text` as is when it fits in `max` UTF-16 units, else cut (between
/// characters) to end in `…` within `max`.
fn cut_utf16(text: &str, max: usize) -> String {
    if text.encode_utf16().count() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 1; // the ellipsis
    for c in text.chars() {
        used += c.len_utf16();
        if used > max {
            break;
        }
        out.push(c);
    }
    out.push('\u{2026}');
    out
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

/// What the user should see, as the hotkey and the full-screen check last said.
#[derive(Clone, Default)]
struct Want {
    error: Option<String>,
    ignored: bool,
}

/// What the tray shows now.
#[derive(Clone, PartialEq, Debug)]
struct Shown {
    /// The dotted icon is on.
    dotted: bool,
    /// The "Last press ignored" menu line is in.
    line: bool,
    tooltip: String,
}

/// What to change on the tray to get from `Shown` to `Want`; `None` leaves it.
#[derive(PartialEq, Debug, Default)]
struct Changes {
    /// Swap the icon: dotted or plain.
    icon: Option<bool>,
    /// Put the menu line in (`true`) or take it out.
    line: Option<bool>,
    tooltip: Option<String>,
}

fn plan(shown: &Shown, want: &Want) -> Changes {
    let dotted = want.error.is_some();
    let tooltip = tooltip_text(want.error.as_deref(), want.ignored);
    Changes {
        icon: (shown.dotted != dotted).then_some(dotted),
        line: (shown.line != want.ignored).then_some(want.ignored),
        tooltip: (shown.tooltip != tooltip).then_some(tooltip),
    }
}

/// Tray state that changes at run time, in managed state. `want` is changed
/// from any thread and only ever held for a field write; `applied` is used by
/// [`apply`] alone, on the main thread. No lock is held across a tray call.
struct TrayState {
    menu: Menu<tauri::Wry>,
    plain: Option<Image<'static>>,
    dotted: Option<Image<'static>>,
    want: Mutex<Want>,
    applied: Mutex<Applied>,
}

struct Applied {
    shown: Shown,
    /// The "Last press ignored" menu item while one is in.
    item: Option<MenuItem<tauri::Wry>>,
}

/// Brings the tray in line with `want`. Main thread only. No-op until the tray exists.
fn apply(app: &AppHandle) {
    let Some(state) = app.try_state::<TrayState>() else { return };
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let want = state.want.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let (mut shown, mut item) = {
        let applied = state.applied.lock().unwrap_or_else(|e| e.into_inner());
        (applied.shown.clone(), applied.item.clone())
    };
    let changes = plan(&shown, &want);
    match changes.line {
        Some(true) => {
            if let Ok(new) = MenuItem::with_id(app, "ignored", IGNORED_ITEM, false, None::<&str>) {
                if state.menu.insert(&new, 0).is_ok() {
                    item = Some(new);
                    shown.line = true;
                }
            }
        }
        Some(false) => {
            if let Some(old) = item.take() {
                let _ = state.menu.remove(&old);
            }
            shown.line = false;
        }
        None => {}
    }
    if let Some(dotted) = changes.icon {
        let icon = if dotted { state.dotted.clone() } else { state.plain.clone() };
        if let Some(icon) = icon {
            let _ = tray.set_icon(Some(icon));
        }
        shown.dotted = dotted;
    }
    if let Some(text) = changes.tooltip {
        let _ = tray.set_tooltip(Some(text.as_str()));
        shown.tooltip = text;
    }
    let mut applied = state.applied.lock().unwrap_or_else(|e| e.into_inner());
    applied.shown = shown;
    applied.item = item;
}

/// Hands the tray work to the main thread without waiting for it.
fn schedule(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || apply(&handle));
}

/// The hotkey's error (the tooltip says it, the icon gets a dot), or `None`
/// when it registered. No-op until the tray exists.
pub fn set_error(app: &AppHandle, error: Option<&str>) {
    let Some(state) = app.try_state::<TrayState>() else { return };
    {
        let mut want = state.want.lock().unwrap_or_else(|e| e.into_inner());
        if want.error.as_deref() == error {
            return;
        }
        want.error = error.map(str::to_string);
    }
    schedule(app);
}

/// Whether the last press of the hotkey was ignored because a full-screen app
/// was in front: the tooltip says so and the menu gets a line at the top. Called
/// on every show, so it does nothing when the value did not change. No-op until
/// the tray exists.
pub fn set_ignored(app: &AppHandle, ignored: bool) {
    let Some(state) = app.try_state::<TrayState>() else { return };
    {
        let mut want = state.want.lock().unwrap_or_else(|e| e.into_inner());
        if want.ignored == ignored {
            return;
        }
        want.ignored = ignored;
    }
    schedule(app);
}

/// Puts "Update to <version>" at the top of the menu, or takes it out on `None`.
/// The menu work runs on the main thread, never waited for and never under a
/// lock. No-op until the tray exists.
pub fn set_update(app: &AppHandle, version: Option<&str>) {
    let Some(holder) = app.try_state::<UpdateItem>() else { return };
    *holder.wanted.lock().unwrap_or_else(|e| e.into_inner()) = version.map(str::to_string);
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || apply_update(&handle));
}

/// Main thread only: puts the update line in line with the version last asked for.
fn apply_update(app: &AppHandle) {
    let Some(holder) = app.try_state::<UpdateItem>() else { return };
    let wanted = holder.wanted.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let current = holder.item.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let now = match (wanted, current) {
        (Some(v), Some(item)) => {
            let _ = item.set_text(format!("Update to {v}"));
            Some(item)
        }
        (Some(v), None) => MenuItem::with_id(app, "update", format!("Update to {v}"), true, None::<&str>)
            .ok()
            .filter(|item| holder.menu.insert(item, 0).is_ok()),
        (None, Some(item)) => {
            let _ = holder.menu.remove(&item);
            None
        }
        (None, None) => None,
    };
    *holder.item.lock().unwrap_or_else(|e| e.into_inner()) = now;
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
            "Flick: last press ignored, a full-screen app was in front; hotkey \"X\" is in use"
        );
    }

    #[test]
    fn a_long_tooltip_is_cut_to_127_units_with_an_ellipsis() {
        let long = "x".repeat(300);
        let cut = tooltip_text(Some(&long), true);
        assert_eq!(cut.encode_utf16().count(), 127);
        assert!(cut.ends_with('\u{2026}'));
        assert!(cut.starts_with("Flick: last press ignored"));
        // Exactly 127 units is left alone.
        let exact = "y".repeat(127 - "Flick \u{2014} ".encode_utf16().count());
        assert_eq!(tooltip_text(Some(&exact), false).encode_utf16().count(), 127);
        assert!(!tooltip_text(Some(&exact), false).ends_with('\u{2026}'));
    }

    #[test]
    fn a_cut_never_splits_a_surrogate_pair() {
        // Two units each: a cut by units would land inside one.
        for pad in 0..3 {
            let error = format!("{}{}", "x".repeat(pad), "\u{1F355}".repeat(100));
            let cut = tooltip_text(Some(&error), false);
            assert!(cut.encode_utf16().count() <= 127);
            assert!(cut.ends_with('\u{2026}'));
        }
    }

    fn shown(dotted: bool, line: bool, error: Option<&str>, ignored: bool) -> Shown {
        Shown { dotted, line, tooltip: tooltip_text(error, ignored) }
    }

    fn want(error: Option<&str>, ignored: bool) -> Want {
        Want { error: error.map(str::to_string), ignored }
    }

    #[test]
    fn nothing_changed_changes_nothing() {
        assert_eq!(plan(&shown(false, false, None, false), &want(None, false)), Changes::default());
        assert_eq!(plan(&shown(true, true, Some("e"), true), &want(Some("e"), true)), Changes::default());
    }

    #[test]
    fn only_ignored_changing_leaves_the_icon_alone() {
        let c = plan(&shown(false, false, None, false), &want(None, true));
        assert_eq!(c.icon, None);
        assert_eq!(c.line, Some(true));
        assert_eq!(c.tooltip, Some(tooltip_text(None, true)));
        let back = plan(&shown(false, true, None, true), &want(None, false));
        assert_eq!((back.icon, back.line), (None, Some(false)));
    }

    #[test]
    fn an_error_appearing_changes_icon_and_tooltip_not_the_menu() {
        let c = plan(&shown(false, false, None, false), &want(Some("e"), false));
        assert_eq!(c, Changes { icon: Some(true), line: None, tooltip: Some(tooltip_text(Some("e"), false)) });
        let gone = plan(&shown(true, false, Some("e"), false), &want(None, false));
        assert_eq!(gone, Changes { icon: Some(false), line: None, tooltip: Some("Flick".to_string()) });
    }

    #[test]
    fn error_and_ignored_flipping_together_change_everything() {
        let c = plan(&shown(false, false, None, false), &want(Some("e"), true));
        assert_eq!(c.icon, Some(true));
        assert_eq!(c.line, Some(true));
        assert_eq!(c.tooltip, Some(tooltip_text(Some("e"), true)));
    }

    #[test]
    fn a_new_error_text_changes_only_the_tooltip() {
        let c = plan(&shown(true, false, Some("a"), false), &want(Some("b"), false));
        assert_eq!((c.icon, c.line), (None, None));
        assert_eq!(c.tooltip, Some(tooltip_text(Some("b"), false)));
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
        // Touches the right and bottom edges; the square's very corner is outside the circle.
        assert_eq!(pixel(&dotted, 32, 31, 25), [0xF5, 0xA5, 0x24, 0xFF]);
        assert_eq!(pixel(&dotted, 32, 25, 31), [0xF5, 0xA5, 0x24, 0xFF]);
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
