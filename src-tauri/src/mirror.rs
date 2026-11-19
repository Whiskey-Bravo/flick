//! What the main thread reads of the config without taking its lock.
//!
//! The main thread must never lock `AppState.config`: a reload holds that lock
//! while it waits for the main thread (the global-shortcut plugin runs its
//! work there), so the two would wait for each other for good. The few
//! settings the main thread needs are copied here by the one function that
//! stores a config, and read from the copy. Each part is an atomic or sits
//! behind a lock that is only ever held to copy a value in or out, never
//! across a call.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::config::Config;
use crate::theme::{self, Appearance};

pub(crate) struct ConfigMirror {
    show_taskbar: AtomicBool,
    appearance: Mutex<Appearance>,
}

/// The theme and animation level of `config`.
fn appearance_of(config: &Config) -> Appearance {
    Appearance {
        tab_completion: config.tab_completion,
        ..theme::resolve(&config.theme, &config.custom_theme, &config.animations)
    }
}

impl ConfigMirror {
    /// The copy of the config loaded at start.
    pub(crate) fn new(config: &Config) -> Self {
        Self { show_taskbar: AtomicBool::new(config.show_taskbar), appearance: Mutex::new(appearance_of(config)) }
    }

    /// Puts `new` into `slot` (the config, which the caller has locked) and
    /// into the copy; returns what `slot` held. The only place a config is stored.
    pub(crate) fn store(&self, slot: &mut Config, new: Config) -> Config {
        self.show_taskbar.store(new.show_taskbar, Ordering::SeqCst);
        *self.appearance.lock().unwrap_or_else(|e| e.into_inner()) = appearance_of(&new);
        std::mem::replace(slot, new)
    }

    /// The `show_taskbar` setting.
    pub(crate) fn show_taskbar(&self) -> bool {
        self.show_taskbar.load(Ordering::SeqCst)
    }

    /// The theme and animation level in force.
    pub(crate) fn appearance(&self) -> Appearance {
        self.appearance.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(show_taskbar: bool, theme: &str) -> Config {
        Config { show_taskbar, theme: theme.to_string(), ..Config::default() }
    }

    #[test]
    fn the_copy_starts_from_the_loaded_config() {
        assert!(ConfigMirror::new(&config(true, "dark")).show_taskbar());
        assert!(!ConfigMirror::new(&config(false, "dark")).show_taskbar());
        assert_eq!(ConfigMirror::new(&config(false, "light")).appearance().theme, "light");
    }

    #[test]
    fn storing_a_config_flips_show_taskbar_both_ways() {
        let mut slot = config(false, "dark");
        let mirror = ConfigMirror::new(&slot);
        let old = mirror.store(&mut slot, config(true, "dark"));
        assert!(!old.show_taskbar);
        assert!(slot.show_taskbar && mirror.show_taskbar());
        let old = mirror.store(&mut slot, config(false, "dark"));
        assert!(old.show_taskbar);
        assert!(!slot.show_taskbar && !mirror.show_taskbar());
    }

    #[test]
    fn storing_a_config_moves_the_appearance_and_tab_completion() {
        let mut slot = config(false, "dark");
        let mirror = ConfigMirror::new(&slot);
        let mut next = config(false, "ocean");
        next.tab_completion = !slot.tab_completion;
        mirror.store(&mut slot, next.clone());
        assert_eq!(mirror.appearance(), appearance_of(&next));
        assert_eq!(mirror.appearance().theme, "ocean");
        assert_eq!(mirror.appearance().tab_completion, next.tab_completion);
    }
}
