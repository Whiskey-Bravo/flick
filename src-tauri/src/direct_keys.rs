//! Direct hotkeys: a global key combination that opens one item without the bar.
//! The bar hotkey always wins: a combo equal to it (in any spelling) is never
//! registered, and one combo that fails leaves the others working.
// Managed and registered in lib.rs by the wiring task.
#![allow(dead_code)]

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::config::{combo_allowed, normalize_combo, DirectHotkeyDef, ItemRef, BAR_COMBO, DUPLICATE_COMBO, WEAK_COMBO};
use crate::launch::CANCELLED;
use crate::provider::{Outcome, ResultItem, Then};
use crate::AppState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterError {
    Invalid,
    Taken,
}

/// Where combos are registered. `keys` is always the normalized combo
/// (`normalize_combo`). Never call on the main thread.
pub trait KeyRegistry: Send + Sync {
    fn register(&self, keys: &str) -> Result<(), RegisterError>;
    fn unregister(&self, keys: &str);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyState {
    Ok,
    Taken,
    NotFound,
    Weak,
    Clash,
    Invalid,
    Duplicate,
}

impl KeyState {
    pub fn message(self) -> &'static str {
        match self {
            KeyState::Ok => "",
            KeyState::Taken => "Taken by another program",
            KeyState::NotFound => "Not found",
            KeyState::Weak => WEAK_COMBO,
            KeyState::Clash => BAR_COMBO,
            KeyState::Invalid => "Not a valid key combination",
            KeyState::Duplicate => DUPLICATE_COMBO,
        }
    }
}

/// One line of the Settings status; `keys` is exactly as written in the config.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct KeyStatusRow {
    pub keys: String,
    pub state: KeyState,
    pub message: String,
}

/// Held across registry calls: serialises apply / release / pause.
#[derive(Default)]
struct Work {
    bar: String,
    defs: Vec<DirectHotkeyDef>,
    /// Normalized combos registered now.
    registered: Vec<String>,
}

/// What presses and the status read. Only ever held briefly, never across a
/// registry call, so a press handled on the main thread never waits for a
/// thread that is itself waiting for the main thread.
#[derive(Default)]
struct View {
    rows: Vec<(DirectHotkeyDef, KeyState)>,
    /// False after `release_all` (nothing is registered then).
    live: bool,
}

pub struct DirectKeys {
    registry: Arc<dyn KeyRegistry>,
    work: Mutex<Work>,
    view: Mutex<View>,
}

impl DirectKeys {
    pub fn new(registry: Arc<dyn KeyRegistry>) -> Self {
        Self { registry, work: Mutex::new(Work::default()), view: Mutex::new(View::default()) }
    }

    /// Releases every combo registered before, then registers `defs` in order,
    /// skipping the bar's combo, weak combos and repeats. Never on the main thread.
    pub fn apply(&self, bar: &str, defs: &[DirectHotkeyDef]) {
        let mut work = self.work.lock().unwrap_or_else(|e| e.into_inner());
        self.release(&mut work);
        let bar_combo = normalize_combo(bar);
        let mut seen: Vec<String> = Vec::new();
        let mut rows = Vec::with_capacity(defs.len());
        for def in defs {
            let combo = normalize_combo(&def.keys);
            let state = if combo == bar_combo {
                KeyState::Clash
            } else if !combo_allowed(&def.keys) {
                KeyState::Weak
            } else if seen.contains(&combo) {
                KeyState::Duplicate
            } else {
                match self.registry.register(&combo) {
                    Ok(()) => {
                        work.registered.push(combo.clone());
                        KeyState::Ok
                    }
                    Err(RegisterError::Invalid) => KeyState::Invalid,
                    Err(RegisterError::Taken) => KeyState::Taken,
                }
            };
            seen.push(combo);
            rows.push((def.clone(), state));
        }
        work.bar = bar.to_string();
        work.defs = defs.to_vec();
        *self.view.lock().unwrap_or_else(|e| e.into_inner()) = View { rows, live: true };
    }

    fn release(&self, work: &mut Work) {
        self.view.lock().unwrap_or_else(|e| e.into_inner()).live = false;
        for combo in work.registered.drain(..) {
            self.registry.unregister(&combo);
        }
    }

    /// Unregisters everything. Never on the main thread.
    pub fn release_all(&self) {
        let mut work = self.work.lock().unwrap_or_else(|e| e.into_inner());
        self.release(&mut work);
    }

    /// Releases every combo while Settings captures keys (`true`), or applies the
    /// remembered set again (`false`). Never on the main thread.
    pub fn pause(&self, paused: bool) {
        if paused {
            self.release_all();
        } else {
            let (bar, defs) = {
                let work = self.work.lock().unwrap_or_else(|e| e.into_inner());
                (work.bar.clone(), work.defs.clone())
            };
            self.apply(&bar, &defs);
        }
    }

    /// The item a registered combo opens, for any spelling of it.
    pub fn item_for(&self, keys: &str) -> Option<ItemRef> {
        let combo = normalize_combo(keys);
        let view = self.view.lock().unwrap_or_else(|e| e.into_inner());
        if !view.live {
            return None;
        }
        view.rows
            .iter()
            .find(|(def, state)| *state == KeyState::Ok && normalize_combo(&def.keys) == combo)
            .map(|(def, _)| def.item.clone())
    }

    /// One row per def, in config order; a registered combo whose item is not
    /// `found` reads `NotFound`.
    pub fn status(&self, found: &dyn Fn(&ItemRef) -> bool) -> Vec<KeyStatusRow> {
        let rows = self.view.lock().unwrap_or_else(|e| e.into_inner()).rows.clone();
        rows.into_iter()
            .map(|(def, state)| {
                let state = if state == KeyState::Ok && !found(&def.item) { KeyState::NotFound } else { state };
                KeyStatusRow { keys: def.keys, state, message: state.message().to_string() }
            })
            .collect()
    }
}

/// The Shortcut for a combo, built from its normalized spelling (modifiers first,
/// Win as `super`), which is the only form global-hotkey reads for every spelling
/// the config accepts.
fn shortcut(keys: &str) -> Result<Shortcut, RegisterError> {
    Shortcut::from_str(&normalize_combo(keys)).map_err(|_| RegisterError::Invalid)
}

/// The real registry: the global-shortcut plugin. Never called on the main thread.
pub struct PluginKeys {
    app: AppHandle,
    on_press: fn(&AppHandle, &str),
}

impl PluginKeys {
    pub fn new(app: AppHandle, on_press: fn(&AppHandle, &str)) -> Self {
        Self { app, on_press }
    }
}

impl KeyRegistry for PluginKeys {
    fn register(&self, keys: &str) -> Result<(), RegisterError> {
        let shortcut = shortcut(keys)?;
        let on_press = self.on_press;
        let keys = keys.to_string();
        self.app
            .global_shortcut()
            .on_shortcut(shortcut, move |app, _, e| {
                if e.state == ShortcutState::Pressed {
                    on_press(app, &keys)
                }
            })
            .map_err(|_| RegisterError::Taken)
    }

    fn unregister(&self, keys: &str) {
        if let Ok(shortcut) = shortcut(keys) {
            let _ = self.app.global_shortcut().unregister(shortcut);
        }
    }
}

/// What a press acts on. `open` may block; never called on the main thread.
pub trait PressTarget: Send + Sync {
    fn item(&self, provider: &str, id: &str) -> Option<ResultItem>;
    fn open(&self, provider: &str, id: &str) -> Result<Outcome, String>;
    fn show_armed(&self, item: ResultItem);
    fn follow_up(&self, then: Then);
    fn failed(&self, message: &str);
}

pub const REPEAT_GATE: Duration = Duration::from_millis(500);

/// Drops repeats of one combo while it is held (each repeat restarts the window).
pub struct PressGate {
    window: Duration,
    last: Mutex<HashMap<String, Instant>>,
}

impl PressGate {
    pub fn new(window: Duration) -> Self {
        Self { window, last: Mutex::new(HashMap::new()) }
    }

    /// False for the same combo (any spelling) within `window` of its last press.
    pub fn allow(&self, keys: &str, now: Instant) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let previous = last.insert(normalize_combo(keys), now);
        previous.is_none_or(|t| now.saturating_duration_since(t) >= self.window)
    }
}

/// Opens `item` for a direct hotkey: a confirm item shows the bar armed; a gone
/// item does nothing; a failure is reported once; a cancel is silent.
pub fn press(target: &dyn PressTarget, item: &ItemRef) {
    let Some(row) = target.item(&item.provider, &item.id) else {
        return;
    };
    if row.confirm {
        target.show_armed(row);
        return;
    }
    match target.open(&item.provider, &item.id) {
        Ok(Outcome::HideThen { then }) => target.follow_up(then),
        Ok(_) => {}
        Err(e) if e == CANCELLED => {}
        Err(e) => target.failed(&e),
    }
}

/// The Settings Hotkeys status, one row per config entry.
#[tauri::command(async)]
pub fn settings_hotkey_status(state: State<AppState>, keys: State<Arc<DirectKeys>>) -> Vec<KeyStatusRow> {
    let engine = &state.engine;
    keys.status(&|r: &ItemRef| engine.item(&r.provider, &r.id).is_some() || !engine.ready(&r.provider))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri_plugin_global_shortcut::{Code, Modifiers};

    #[derive(Default)]
    struct FakeRegistry {
        calls: Mutex<Vec<String>>,
        taken: Vec<&'static str>,
        invalid: Vec<&'static str>,
    }

    impl FakeRegistry {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl KeyRegistry for FakeRegistry {
        fn register(&self, keys: &str) -> Result<(), RegisterError> {
            if self.invalid.contains(&keys) {
                return Err(RegisterError::Invalid);
            }
            if self.taken.contains(&keys) {
                return Err(RegisterError::Taken);
            }
            self.calls.lock().unwrap().push(format!("register {keys}"));
            Ok(())
        }
        fn unregister(&self, keys: &str) {
            self.calls.lock().unwrap().push(format!("unregister {keys}"));
        }
    }

    fn item(id: &str) -> ItemRef {
        ItemRef { provider: "apps".into(), id: id.into(), title: id.into() }
    }

    fn def(keys: &str, id: &str) -> DirectHotkeyDef {
        DirectHotkeyDef { keys: keys.into(), item: item(id) }
    }

    fn keys_with(reg: FakeRegistry) -> (Arc<FakeRegistry>, DirectKeys) {
        let reg = Arc::new(reg);
        (reg.clone(), DirectKeys::new(reg))
    }

    fn states(keys: &DirectKeys) -> Vec<(String, KeyState)> {
        keys.status(&|_| true).into_iter().map(|r| (r.keys, r.state)).collect()
    }

    #[test]
    fn a_combo_equal_to_the_bar_hotkey_is_never_registered() {
        let (reg, keys) = keys_with(FakeRegistry::default());
        keys.apply("Alt+Space", &[def("alt+space", "a"), def("Space+Alt", "b"), def("Ctrl+Alt+T", "c")]);
        assert_eq!(
            states(&keys),
            [
                ("alt+space".to_string(), KeyState::Clash),
                ("Space+Alt".to_string(), KeyState::Clash),
                ("Ctrl+Alt+T".to_string(), KeyState::Ok)
            ]
        );
        assert_eq!(reg.calls(), ["register ctrl+alt+t"]);
        assert_eq!(keys.status(&|_| true)[0].message, BAR_COMBO);
        assert_eq!(keys.item_for("Alt+Space"), None);
        // The bar hotkey changed to a direct one's combo: that one is released and refused.
        keys.apply("Alt+Ctrl+T", &[def("Ctrl+Alt+T", "c"), def("Ctrl+Alt+Space", "a")]);
        assert_eq!(reg.calls(), ["register ctrl+alt+t", "unregister ctrl+alt+t", "register ctrl+alt+space"]);
        assert_eq!(states(&keys)[0].1, KeyState::Clash);
    }

    #[test]
    fn one_taken_combo_leaves_the_others_working() {
        let (reg, keys) = keys_with(FakeRegistry { taken: vec!["ctrl+alt+x"], ..Default::default() });
        keys.apply("Alt+Space", &[def("Ctrl+Alt+T", "t"), def("Ctrl+Alt+X", "x"), def("Super+N", "n")]);
        let rows = keys.status(&|_| true);
        assert_eq!(rows[1], KeyStatusRow { keys: "Ctrl+Alt+X".into(), state: KeyState::Taken, message: "Taken by another program".into() });
        assert_eq!((rows[0].state, rows[2].state), (KeyState::Ok, KeyState::Ok));
        assert_eq!(reg.calls(), ["register ctrl+alt+t", "register super+n"]);
        assert_eq!(keys.item_for("Win+N"), Some(item("n")));
        assert_eq!(keys.item_for("Ctrl+Alt+X"), None);
    }

    #[test]
    fn weak_invalid_and_duplicate_combos_are_refused() {
        let (reg, keys) = keys_with(FakeRegistry { invalid: vec!["ctrl+alt+nope"], ..Default::default() });
        keys.apply("Alt+Space", &[def("Ctrl+T", "a"), def("Ctrl+Alt+Nope", "b"), def("Ctrl+Alt+T", "c"), def("Alt+Ctrl+T", "d")]);
        let rows = keys.status(&|_| true);
        let got: Vec<(KeyState, &str)> = rows.iter().map(|r| (r.state, r.message.as_str())).collect();
        assert_eq!(
            got,
            [
                (KeyState::Weak, WEAK_COMBO),
                (KeyState::Invalid, "Not a valid key combination"),
                (KeyState::Ok, ""),
                (KeyState::Duplicate, DUPLICATE_COMBO)
            ]
        );
        assert_eq!(reg.calls(), ["register ctrl+alt+t"]);
        assert_eq!(keys.item_for("Ctrl+Alt+T"), Some(item("c")));
    }

    #[test]
    fn apply_releases_the_previous_set_first() {
        let (reg, keys) = keys_with(FakeRegistry::default());
        keys.apply("Alt+Space", &[def("Ctrl+Alt+T", "t"), def("Win+N", "n")]);
        keys.apply("Alt+Space", &[def("Ctrl+Alt+T", "t"), def("Ctrl+Alt+E", "e")]);
        assert_eq!(
            reg.calls(),
            [
                "register ctrl+alt+t",
                "register super+n",
                "unregister ctrl+alt+t",
                "unregister super+n",
                "register ctrl+alt+t",
                "register ctrl+alt+e"
            ]
        );
        assert_eq!(keys.item_for("Win+N"), None);
    }

    #[test]
    fn pause_releases_everything_and_resume_restores() {
        let (reg, keys) = keys_with(FakeRegistry::default());
        keys.apply("Alt+Space", &[def("Ctrl+Alt+T", "t"), def("Alt+Space", "x")]);
        keys.pause(true);
        assert_eq!(reg.calls(), ["register ctrl+alt+t", "unregister ctrl+alt+t"]);
        assert_eq!(keys.item_for("Ctrl+Alt+T"), None);
        keys.pause(false);
        assert_eq!(reg.calls(), ["register ctrl+alt+t", "unregister ctrl+alt+t", "register ctrl+alt+t"]);
        assert_eq!(keys.item_for("Ctrl+Alt+T"), Some(item("t")));
        assert_eq!(states(&keys)[1].1, KeyState::Clash);
        keys.release_all();
        assert_eq!(reg.calls().last().map(String::as_str), Some("unregister ctrl+alt+t"));
        keys.release_all();
        assert_eq!(reg.calls().len(), 4);
    }

    #[test]
    fn status_says_not_found_for_a_missing_item() {
        let (_, keys) = keys_with(FakeRegistry::default());
        keys.apply("Alt+Space", &[def("Ctrl+Alt+T", "gone"), def("Ctrl+T", "gone"), def("Ctrl+Alt+E", "here")]);
        let rows = keys.status(&|r| r.id != "gone");
        assert_eq!(rows[0], KeyStatusRow { keys: "Ctrl+Alt+T".into(), state: KeyState::NotFound, message: "Not found".into() });
        assert_eq!((rows[1].state, rows[2].state), (KeyState::Weak, KeyState::Ok));
    }

    #[test]
    fn status_keys_are_exactly_as_written_in_the_config() {
        let (_, keys) = keys_with(FakeRegistry::default());
        keys.apply("Alt+Space", &[def("ctrl+alt+esc", "a"), def("N+Win", "b"), def(" Alt + Ctrl + KeyT ", "c")]);
        let written: Vec<String> = keys.status(&|_| true).into_iter().map(|r| r.keys).collect();
        assert_eq!(written, ["ctrl+alt+esc", "N+Win", " Alt + Ctrl + KeyT "]);
        let json = serde_json::to_value(&keys.status(&|_| false)[0]).unwrap();
        assert_eq!(json, serde_json::json!({ "keys": "ctrl+alt+esc", "state": "not_found", "message": "Not found" }));
    }

    #[test]
    fn item_for_matches_any_spelling() {
        let (_, keys) = keys_with(FakeRegistry::default());
        keys.apply("Alt+Space", &[def("Ctrl+Alt+Esc", "a"), def("Win+N", "n")]);
        for spelling in ["ctrl+alt+escape", "Alt+Control+Escape", "Escape+Alt+Ctrl", "CmdOrCtrl+Alt+Esc"] {
            assert_eq!(keys.item_for(spelling), Some(item("a")), "{spelling}");
        }
        for spelling in ["super+n", "N+Win", "meta+KeyN", "Cmd+n"] {
            assert_eq!(keys.item_for(spelling), Some(item("n")), "{spelling}");
        }
        assert_eq!(keys.item_for("Ctrl+Alt+N"), None);
    }

    #[test]
    fn every_accepted_spelling_parses_as_the_right_shortcut() {
        let cases = [
            ("Win+N", Modifiers::SUPER, Code::KeyN),
            ("N+Win", Modifiers::SUPER, Code::KeyN),
            ("Meta+Shift+n", Modifiers::SUPER | Modifiers::SHIFT, Code::KeyN),
            ("Esc+Ctrl+Alt", Modifiers::CONTROL | Modifiers::ALT, Code::Escape),
            ("KeyT+Alt+Ctrl", Modifiers::CONTROL | Modifiers::ALT, Code::KeyT),
            ("CmdOrCtrl+Alt+T", Modifiers::CONTROL | Modifiers::ALT, Code::KeyT),
            ("Command+Digit1", Modifiers::SUPER, Code::Digit1),
            ("ctrl+alt+num0", Modifiers::CONTROL | Modifiers::ALT, Code::Numpad0),
            ("Ctrl+Alt+-", Modifiers::CONTROL | Modifiers::ALT, Code::Minus),
            ("Ctrl+Alt+ArrowUp", Modifiers::CONTROL | Modifiers::ALT, Code::ArrowUp),
        ];
        for (keys, mods, code) in cases {
            assert_eq!(shortcut(keys), Ok(Shortcut::new(Some(mods), code)), "{keys}");
        }
        assert_eq!(shortcut("Ctrl+Alt+Nope"), Err(RegisterError::Invalid));
    }

    #[derive(Default)]
    struct FakeTarget {
        item: Option<ResultItem>,
        open: Mutex<Option<Result<Outcome, String>>>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeTarget {
        fn with(item: Option<ResultItem>, open: Result<Outcome, String>) -> Self {
            Self { item, open: Mutex::new(Some(open)), ..Default::default() }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl PressTarget for FakeTarget {
        fn item(&self, provider: &str, id: &str) -> Option<ResultItem> {
            self.calls.lock().unwrap().push(format!("item {provider} {id}"));
            self.item.clone()
        }
        fn open(&self, provider: &str, id: &str) -> Result<Outcome, String> {
            self.calls.lock().unwrap().push(format!("open {provider} {id}"));
            self.open.lock().unwrap().take().expect("opened twice")
        }
        fn show_armed(&self, item: ResultItem) {
            self.calls.lock().unwrap().push(format!("armed {}", item.id));
        }
        fn follow_up(&self, then: Then) {
            self.calls.lock().unwrap().push(format!("follow_up {then:?}"));
        }
        fn failed(&self, message: &str) {
            self.calls.lock().unwrap().push(format!("failed {message}"));
        }
    }

    fn row(id: &str, confirm: bool) -> ResultItem {
        ResultItem { id: id.into(), provider: "apps", title: id.into(), confirm, ..Default::default() }
    }

    #[test]
    fn a_missing_item_does_nothing_visible() {
        let t = FakeTarget::with(None, Ok(Outcome::Hide));
        press(&t, &item("x"));
        assert_eq!(t.calls(), ["item apps x"]);
    }

    #[test]
    fn a_confirm_item_shows_the_bar_armed() {
        let t = FakeTarget::with(Some(row("x", true)), Ok(Outcome::Hide));
        press(&t, &item("x"));
        assert_eq!(t.calls(), ["item apps x", "armed x"]);
    }

    #[test]
    fn a_plain_item_opens_without_the_bar() {
        let t = FakeTarget::with(Some(row("x", false)), Ok(Outcome::Hide));
        press(&t, &item("x"));
        assert_eq!(t.calls(), ["item apps x", "open apps x"]);
    }

    #[test]
    fn a_follow_up_runs_after_open() {
        let t = FakeTarget::with(Some(row("x", false)), Ok(Outcome::HideThen { then: Then::Snip }));
        press(&t, &item("x"));
        assert_eq!(t.calls(), ["item apps x", "open apps x", "follow_up Snip"]);
    }

    #[test]
    fn an_error_is_reported_once() {
        let t = FakeTarget::with(Some(row("x", false)), Err("file not found".into()));
        press(&t, &item("x"));
        assert_eq!(t.calls(), ["item apps x", "open apps x", "failed file not found"]);
    }

    #[test]
    fn cancelled_is_silent() {
        let t = FakeTarget::with(Some(row("x", false)), Err(CANCELLED.into()));
        press(&t, &item("x"));
        assert_eq!(t.calls(), ["item apps x", "open apps x"]);
    }

    #[test]
    fn a_held_combo_opens_once() {
        let gate = PressGate::new(REPEAT_GATE);
        let t0 = Instant::now();
        assert!(gate.allow("Ctrl+Alt+T", t0));
        assert!(!gate.allow("alt+ctrl+t", t0 + Duration::from_millis(30)));
        assert!(gate.allow("Win+N", t0 + Duration::from_millis(40)));
        assert!(!gate.allow("Ctrl+Alt+T", t0 + Duration::from_millis(300)));
        assert!(gate.allow("Ctrl+Alt+T", t0 + Duration::from_millis(900)));
    }
}
