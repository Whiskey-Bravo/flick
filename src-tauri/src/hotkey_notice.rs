//! Says once, in a notification, that the bar's hotkey could not be used.

use std::sync::Mutex;

/// The hotkey error already announced, so a reload that fails the same way
/// stays quiet.
#[derive(Default)]
pub struct HotkeyNotice {
    announced: Mutex<Option<String>>,
}

impl HotkeyNotice {
    /// `Some((title, body))` when `error` is new since the last call; `None`
    /// for a repeat, and for no error (which forgets the last one, so the
    /// same error later is announced again).
    pub fn update(&self, accelerator: &str, error: Option<&str>) -> Option<(String, String)> {
        let mut announced = self.announced.lock().unwrap_or_else(|e| e.into_inner());
        let Some(error) = error else {
            *announced = None;
            return None;
        };
        if announced.as_deref() == Some(error) {
            return None;
        }
        *announced = Some(error.to_string());
        let mut chars = error.chars();
        let body: String = chars.next().into_iter().flat_map(char::to_uppercase).chain(chars).collect();
        Some((format!("Flick can't use {accelerator}"), body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ERR: &str = "hotkey \"Alt+Space\" is in use by another program";

    #[test]
    fn the_first_error_is_announced_with_a_capital() {
        let notice = HotkeyNotice::default();
        assert_eq!(
            notice.update("Alt+Space", Some(ERR)),
            Some((
                "Flick can't use Alt+Space".to_string(),
                "Hotkey \"Alt+Space\" is in use by another program".to_string()
            ))
        );
    }

    #[test]
    fn the_same_error_again_is_quiet() {
        let notice = HotkeyNotice::default();
        assert!(notice.update("Alt+Space", Some(ERR)).is_some());
        assert_eq!(notice.update("Alt+Space", Some(ERR)), None);
    }

    #[test]
    fn no_error_forgets_the_last_one() {
        let notice = HotkeyNotice::default();
        assert!(notice.update("Alt+Space", Some(ERR)).is_some());
        assert_eq!(notice.update("Alt+Space", None), None);
        assert!(notice.update("Alt+Space", Some(ERR)).is_some());
    }

    #[test]
    fn a_different_error_is_announced() {
        let notice = HotkeyNotice::default();
        assert!(notice.update("Alt+Space", Some(ERR)).is_some());
        let other = notice.update("Ctrl+Q", Some("hotkey \"Ctrl+Q\" is not a valid shortcut")).unwrap();
        assert_eq!(other.0, "Flick can't use Ctrl+Q");
        assert_eq!(other.1, "Hotkey \"Ctrl+Q\" is not a valid shortcut");
    }
}
