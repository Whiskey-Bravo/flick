//! Global hotkey that toggles the bar. Knows nothing about providers, so a
//! Win-key hook can replace this module later.

use std::sync::Mutex;

use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

use crate::window;

/// The shortcut currently registered by this module, if any.
static REGISTERED: Mutex<Option<Shortcut>> = Mutex::new(None);

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
        gs.on_shortcut(shortcut, on_hotkey).map(|_| shortcut).map_err(|_| in_use(accelerator))
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

fn in_use(accelerator: &str) -> String {
    format!("hotkey \"{accelerator}\" is in use by another program")
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
    fn taken_hotkey_reads_in_use() {
        assert_eq!(in_use("Alt+Space"), "hotkey \"Alt+Space\" is in use by another program");
    }
}
