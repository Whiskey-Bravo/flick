//! Global hotkey that toggles the bar. Knows nothing about providers, so a
//! Win-key hook can replace this module later.

use std::sync::Mutex;

use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

use crate::window;

/// The shortcut currently registered by this module, if any.
static REGISTERED: Mutex<Option<Shortcut>> = Mutex::new(None);

/// The shortcut released while the Settings window captures a new combination.
static PAUSED: Mutex<Option<Shortcut>> = Mutex::new(None);

/// What to register again when capture ends: the paused shortcut, unless a
/// new one was applied meanwhile.
fn resume_target<T>(registered: &Option<T>, paused: Option<T>) -> Option<T> {
    if registered.is_some() { None } else { paused }
}

/// Releases the hotkey (`paused`) so the Settings window can see its keys, or
/// registers it again. Never call on the main thread (see `apply`).
pub fn set_paused(app: &AppHandle, paused: bool) {
    let gs = app.global_shortcut();
    let mut registered = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
    let mut parked = PAUSED.lock().unwrap_or_else(|e| e.into_inner());
    if paused {
        if let Some(current) = registered.take() {
            let _ = gs.unregister(current);
            *parked = Some(current);
        }
    } else if let Some(old) = resume_target(&registered, parked.take()) {
        if gs.on_shortcut(old, on_hotkey).is_ok() {
            *registered = Some(old);
        }
    }
}

/// Unregisters the previous hotkey, then registers `accelerator` (e.g. "Alt+Space").
/// Fails when the accelerator is invalid or taken by another program; the error
/// is the text shown in the bar (the tray tooltip adds "Flick — "). On failure the previous hotkey is registered
/// again, so a typo in the config never leaves the user without one.
///
/// Never call this on the main thread while another thread may be inside it:
/// the plugin blocks on the main thread while the lock is held.
pub fn apply(app: &AppHandle, accelerator: &str) -> Result<(), String> {
    let gs = app.global_shortcut();
    let mut registered = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
    let previous = registered.take();
    if let Some(old) = previous {
        let _ = gs.unregister(old);
    }
    let attempt = parse(accelerator).and_then(|shortcut| {
        gs.on_shortcut(shortcut, on_hotkey).map(|_| shortcut).map_err(|_| in_use(accelerator, powertoys_run_running()))
    });
    match attempt {
        Ok(shortcut) => {
            *registered = Some(shortcut);
            Ok(())
        }
        Err(e) => {
            if let Some(old) = previous {
                if gs.on_shortcut(old, on_hotkey).is_ok() {
                    *registered = Some(old);
                }
            }
            Err(e)
        }
    }
}

fn parse(accelerator: &str) -> Result<Shortcut, String> {
    accelerator
        .trim()
        .parse::<Shortcut>()
        .map_err(|_| format!("hotkey \"{accelerator}\" is not a valid shortcut"))
}

fn in_use(accelerator: &str, powertoys_run: bool) -> String {
    if powertoys_run {
        format!(
            "hotkey \"{accelerator}\" is taken, probably by PowerToys Run: turn PowerToys Run off in PowerToys, or set hotkey in Flick's config"
        )
    } else {
        format!("hotkey \"{accelerator}\" is in use by another program")
    }
}

/// True when a process named PowerToys.PowerLauncher.exe is running. Only
/// called after a registration failure.
fn powertoys_run_running() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };

    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return false;
    };
    let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut found = false;
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while more {
        let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
        if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case("PowerToys.PowerLauncher.exe") {
            found = true;
            break;
        }
        more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }
    let _ = unsafe { CloseHandle(snapshot) };
    found
}

fn on_hotkey(app: &AppHandle, _shortcut: &Shortcut, event: ShortcutEvent) {
    if event.state == ShortcutState::Pressed {
        window::toggle(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typo_reads_not_a_valid_shortcut() {
        assert_eq!(parse("Alt+Spcae"), Err("hotkey \"Alt+Spcae\" is not a valid shortcut".to_string()));
        assert!(parse(" Alt+Space ").is_ok());
    }

    #[test]
    fn taken_hotkey_names_powertoys_when_it_runs() {
        assert_eq!(in_use("Alt+Space", false), "hotkey \"Alt+Space\" is in use by another program");
        assert_eq!(
            in_use("Alt+Space", true),
            "hotkey \"Alt+Space\" is taken, probably by PowerToys Run: turn PowerToys Run off in PowerToys, or set hotkey in Flick's config"
        );
    }

    #[test]
    fn settings_hotkey_strings_parse() {
        for s in ["Alt+Space", "Ctrl+Shift+K", "F5", "Ctrl+Alt+Super+F12", "Alt+F1", "Ctrl+Alt+Shift+Super+F24"] {
            assert!(parse(s).is_ok(), "{s} should parse");
        }
    }
}
