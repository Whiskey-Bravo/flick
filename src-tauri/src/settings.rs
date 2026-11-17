//! Commands of the Settings window. Rust is the only writer of `config.toml`: every
//! write reads the file, edits it in place with `config_edit`, replaces it atomically
//! and reloads straight away. A file that does not parse is never overwritten.
//!
//! The helpers up to `check_city` take a path (or a fetcher) and closures, so they are
//! tested without an `AppHandle`; the `#[tauri::command]` wrappers below only supply
//! the real path, `hotkey::apply` and `reload_config`.

use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::DialogExt;

use crate::answers::weather;
use crate::config::{self, Config, FieldError, ShortcutDef};
use crate::config_edit::{self, Setting};
use crate::net::{Fetch, FetchError, WinHttp};
use crate::{apply_hotkey, reload_config, AppState, WslState};

/// Label of the Settings window; events for it are harmless while it is closed.
pub const WINDOW: &str = "settings";

/// Serialises read-edit-write of the file between commands.
static WRITING: Mutex<()> = Mutex::new(());

/// Why a shortcut could not be saved: a rule of one field, or the file itself.
#[derive(Serialize, Debug, PartialEq)]
#[serde(untagged)]
pub enum SaveError {
    Field(FieldError),
    File(String),
}

/// The file's text; a missing file is empty (it is created by the first write).
fn read_text(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("config.toml: could not read: {e}")),
    }
}

/// The parsed file, or its error text ("config.toml: line 3: ...").
fn read_config(path: &Path) -> Result<(String, Config), String> {
    let text = read_text(path)?;
    let config = config::parse(&text).map_err(|e| format!("config.toml: {e}"))?;
    Ok((text, config))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    config_edit::write_atomic(path, text).map_err(|e| format!("config.toml: could not save: {e}"))
}

/// Edits one setting in the file at `path`.
pub fn write_setting(path: &Path, setting: &Setting) -> Result<(), String> {
    let (text, _) = read_config(path)?;
    write(path, &config_edit::set(&text, setting)?)
}

/// Writes the hotkey only once `register` has accepted it. `applied` is the hotkey
/// that is live: asking for it again does nothing. A broken file is refused before
/// anything is registered.
pub fn set_hotkey(
    path: &Path,
    applied: Option<&str>,
    wanted: &str,
    register: impl Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    if applied == Some(wanted) {
        return Ok(());
    }
    read_config(path)?;
    register(wanted)?;
    write_setting(path, &Setting::Hotkey(wanted.to_string()))
}

/// Adds a shortcut (`original` is `None`) or replaces the one named `original`.
pub fn save_shortcut(path: &Path, original: Option<&str>, def: &ShortcutDef) -> Result<(), SaveError> {
    let (text, config) = read_config(path).map_err(SaveError::File)?;
    config::check_shortcut_in(&config, original, def).map_err(SaveError::Field)?;
    let edited = config_edit::upsert_shortcut(&text, original, def).map_err(SaveError::File)?;
    write(path, &edited).map_err(SaveError::File)
}

pub fn remove_shortcut(path: &Path, name: &str) -> Result<(), String> {
    let (text, _) = read_config(path)?;
    write(path, &config_edit::delete_shortcut(&text, name)?)
}

/// One geocoding request for `city`: `"<name>, <country>"` of the best match.
/// Only asked while online answers are on.
pub fn check_city(online: bool, fetch: &dyn Fetch, city: &str) -> Result<String, String> {
    if !online {
        return Err("Online answers are off".into());
    }
    let city = city.trim();
    if city.is_empty() {
        return Err("Type a city".into());
    }
    match fetch.get(&weather::geocode_url(city)) {
        Ok(body) => match weather::geocode_label(&body) {
            Ok(Some(label)) => Ok(label),
            Ok(None) => Err("City not found".into()),
            Err(()) => Err("The weather service gave an unexpected answer".into()),
        },
        Err(FetchError::Offline) => Err("Could not reach the weather service".into()),
        Err(_) => Err("The weather service did not answer".into()),
    }
}

// ---- commands ----------------------------------------------------------------

#[derive(Serialize)]
pub struct SettingsState {
    /// The last good config.
    config: Config,
    /// Error of the file as it is now, if it does not parse or validate.
    file_error: Option<String>,
    /// Usable WSL distros; `None` when WSL is absent or not detected yet.
    distros: Option<Vec<String>>,
    start_at_login: bool,
    version: String,
}

#[tauri::command(async)]
pub fn settings_state(app: AppHandle, state: State<AppState>) -> SettingsState {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let distros = match &*state.detected.lock().unwrap_or_else(|e| e.into_inner()) {
        WslState::Present { distros, .. } => Some(distros.clone()),
        WslState::Unknown | WslState::NoWsl => None,
    };
    SettingsState {
        config,
        file_error: read_config(&config::config_path()).err(),
        distros,
        start_at_login: app.autolaunch().is_enabled().unwrap_or(false),
        version: app.package_info().version.to_string(),
    }
}

/// Applies the new file now rather than waiting for the watcher, which then finds
/// nothing new. `reload_config` also emits `settings:changed`.
#[tauri::command(async)]
pub fn settings_set(setting: Setting, app: AppHandle, state: State<AppState>) -> Result<(), String> {
    let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let path = config::config_path();
    let result = match setting {
        Setting::Hotkey(wanted) => {
            let applied = state.hotkey.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let r = set_hotkey(&path, applied.as_deref(), &wanted, |h| apply_hotkey(&app, h).map_or(Ok(()), Err));
            if r.is_err() {
                // The old hotkey is registered again; bring the record and the
                // tooltip back in line with it.
                reload_config(&app);
            }
            r
        }
        other => write_setting(&path, &other),
    };
    if result.is_ok() {
        reload_config(&app);
    }
    result
}

#[tauri::command(async)]
pub fn settings_save_shortcut(original: Option<String>, def: ShortcutDef, app: AppHandle) -> Result<(), SaveError> {
    let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    save_shortcut(&config::config_path(), original.as_deref(), &def)?;
    reload_config(&app);
    Ok(())
}

/// Against the last good config, in memory only.
#[tauri::command(async)]
pub fn settings_check_shortcut(
    original: Option<String>,
    def: ShortcutDef,
    state: State<AppState>,
) -> Option<FieldError> {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    config::check_shortcut_in(&config, original.as_deref(), &def).err()
}

#[tauri::command(async)]
pub fn settings_delete_shortcut(name: String, app: AppHandle) -> Result<(), String> {
    let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    remove_shortcut(&config::config_path(), &name)?;
    reload_config(&app);
    Ok(())
}

/// Returns the state Windows ended up with, and moves the tray's check mark to it.
#[tauri::command(async)]
pub fn settings_set_start_at_login(on: bool, app: AppHandle) -> bool {
    let manager = app.autolaunch();
    let _ = if on { manager.enable() } else { manager.disable() };
    let actual = manager.is_enabled().unwrap_or(false);
    crate::tray::set_login_checked(&app, actual);
    actual
}

#[tauri::command(async)]
pub fn settings_clear_history(state: State<AppState>) {
    state.history.clear();
}

/// A folder picker over the Settings window; `None` when cancelled.
#[tauri::command(async)]
pub fn settings_pick_folder(app: AppHandle) -> Option<String> {
    let mut dialog = app.dialog().file();
    if let Some(window) = app.get_webview_window(WINDOW) {
        dialog = dialog.set_parent(&window);
    }
    let path = dialog.blocking_pick_folder()?.into_path().ok()?;
    Some(path.to_string_lossy().into_owned())
}

#[derive(Serialize)]
pub struct PinRow {
    provider: String,
    id: String,
    title: String,
    subtitle: String,
    icon: String,
    /// False when the provider can no longer rebuild the item; `title` is then the stored id.
    found: bool,
}

#[tauri::command(async)]
pub fn settings_pins(state: State<AppState>) -> Vec<PinRow> {
    state
        .engine
        .pin_list()
        .into_iter()
        .map(|(provider, id)| match state.engine.item(&provider, &id) {
            Some(item) => PinRow { provider, id, title: item.title, subtitle: item.subtitle, icon: item.icon, found: true },
            None => PinRow { provider, title: id.clone(), id, subtitle: String::new(), icon: String::new(), found: false },
        })
        .collect()
}

#[tauri::command(async)]
pub fn settings_move_pin(provider: String, id: String, up: bool, state: State<AppState>) {
    state.engine.move_pin(&provider, &id, up);
}

#[tauri::command(async)]
pub fn settings_check_city(city: String, state: State<AppState>) -> Result<String, String> {
    let online = state.config.lock().unwrap_or_else(|e| e.into_inner()).online;
    check_city(online, &WinHttp, &city)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Field;
    use crate::net::FakeFetch;
    use std::cell::Cell;

    const FILE: &str = "# my config\nhotkey = \"Alt+Space\"\n\n[[shortcut]]\nname = \"a\"\nrun = \"notepad\"\n\n[[shortcut]]\nname = \"b\"\nrun = \"calc\"\n";

    fn file(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    fn def(name: &str) -> ShortcutDef {
        ShortcutDef {
            name: name.into(),
            aliases: vec![],
            about: String::new(),
            run: Some("notepad".into()),
            wsl: None,
            open: None,
            terminal: None,
            admin: false,
            pick: None,
            choices: vec![],
        }
    }

    #[test]
    fn broken_file_is_never_overwritten() {
        let (_d, path) = file("hotkey = ");
        assert!(write_setting(&path, &Setting::History(false)).is_err());
        assert!(matches!(save_shortcut(&path, None, &def("x")), Err(SaveError::File(_))));
        assert!(remove_shortcut(&path, "a").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hotkey = ");
    }

    #[test]
    fn hotkey_is_written_only_after_it_registers() {
        let (_d, path) = file(FILE);
        let err = set_hotkey(&path, Some("Alt+Space"), "Ctrl+Space", |_| Err("in use".into()));
        assert_eq!(err, Err("in use".to_string()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FILE);

        let calls = Cell::new(0);
        set_hotkey(&path, Some("Alt+Space"), "Alt+Space", |_| {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(calls.get(), 0, "already applied: nothing registered");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FILE);

        set_hotkey(&path, Some("Alt+Space"), "Ctrl+Space", |_| Ok(())).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("hotkey = \"Ctrl+Space\""), "{after}");
        assert!(after.starts_with("# my config\n"), "comments kept");
    }

    #[test]
    fn broken_file_registers_no_hotkey() {
        let (_d, path) = file("hotkey = ");
        let calls = Cell::new(0);
        let r = set_hotkey(&path, None, "Ctrl+Space", |_| {
            calls.set(calls.get() + 1);
            Ok(())
        });
        assert!(r.is_err());
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn save_shortcut_reports_field_errors_and_does_not_write() {
        let (_d, path) = file(FILE);
        // Rename "a" onto the existing "b".
        let r = save_shortcut(&path, Some("a"), &def("b"));
        assert!(matches!(&r, Err(SaveError::Field(e)) if e.field == Field::Name), "{r:?}");
        // A new shortcut may not take the built-in `help`.
        assert!(matches!(save_shortcut(&path, None, &def("HELP")), Err(SaveError::Field(_))));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FILE);
    }

    #[test]
    fn shortcuts_are_added_replaced_and_deleted_in_place() {
        let (_d, path) = file(FILE);
        save_shortcut(&path, None, &def("c")).unwrap();
        save_shortcut(&path, Some("a"), &def("a2")).unwrap();
        remove_shortcut(&path, "b").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let config = config::parse(&text).unwrap();
        let names: Vec<_> = config.shortcuts.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a2", "c"]);
        assert!(text.starts_with("# my config\n"));
    }

    #[test]
    fn settings_are_written_to_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        write_setting(&path, &Setting::History(false)).unwrap();
        assert!(!config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap().history);
    }

    const LONDON_GEO: &[u8] = br#"{"results":[{"name":"London","latitude":51.5,"longitude":-0.12,"country":"United Kingdom"}]}"#;

    #[test]
    fn city_check_finds_misses_and_goes_offline() {
        let found = FakeFetch::offline().respond(&weather::geocode_url("London"), Ok(LONDON_GEO.to_vec()));
        assert_eq!(check_city(true, &found, " London "), Ok("London, United Kingdom".to_string()));
        assert_eq!(found.urls(), vec![weather::geocode_url("London")], "one request");

        let none = FakeFetch::new(Ok(br#"{"generationtime_ms":0.3}"#.to_vec()));
        assert_eq!(check_city(true, &none, "Nowhereville"), Err("City not found".to_string()));

        let offline = FakeFetch::offline();
        assert_eq!(check_city(true, &offline, "Paris"), Err("Could not reach the weather service".to_string()));
    }

    #[test]
    fn city_check_stays_off_the_network_when_offline_mode_or_blank() {
        let fetch = FakeFetch::new(Ok(LONDON_GEO.to_vec()));
        assert_eq!(check_city(false, &fetch, "London"), Err("Online answers are off".to_string()));
        assert!(check_city(true, &fetch, "  ").is_err());
        assert!(fetch.urls().is_empty());
    }
}

/// Opens config.toml like the tray's "Edit config" (the file-error banner's button).
#[tauri::command(async)]
pub fn settings_open_file() -> Result<(), String> {
    crate::launch::open_config()
}
