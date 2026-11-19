//! Direct hotkeys: a global key combination that opens one item without the bar.
//! The bar hotkey always wins: a combo equal to it (in any spelling) is never
//! registered, and one combo that fails leaves the others working.

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
/// (`normalize_combo`). Never call on the main thread, except once during
/// setup, before the config watcher, the tray and Settings exist.
pub trait KeyRegistry: Send + Sync {
    /// Registers `keys`; a combo held by anyone (another program, or the bar
    /// hotkey) is `Taken` and is left alone.
    fn register(&self, keys: &str) -> Result<(), RegisterError>;
    /// Releases a combo this module registered; false when it is still held.
    fn unregister(&self, keys: &str) -> bool;
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
    /// Combos we registered whose release failed: still ours, retried on every
    /// release. The only combos this module ever frees besides `registered`.
    stale: Vec<String>,
    /// Released while Settings captures keys; `apply` then only remembers.
    paused: bool,
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
    /// skipping the bar's combo, weak combos and repeats. `bar` must be the bar
    /// shortcut actually registered (after any fallback), not just the one asked
    /// for. While paused it only remembers `bar` and `defs` for the resume and
    /// shows them in the status without registering (`item_for` stays None).
    /// Never on the main thread, except once during setup, before the config
    /// watcher, the tray and Settings exist: the plugin then runs inline there
    /// and no other thread can hold `work`.
    pub fn apply(&self, bar: &str, defs: &[DirectHotkeyDef]) {
        let mut work = self.work.lock().unwrap_or_else(|e| e.into_inner());
        if work.paused {
            work.bar = bar.to_string();
            work.defs = defs.to_vec();
            let rows = defs
                .iter()
                .zip(refused_states(bar, defs))
                .map(|(def, refused)| (def.clone(), refused.unwrap_or(KeyState::Ok)))
                .collect();
            *self.view.lock().unwrap_or_else(|e| e.into_inner()) = View { rows, live: false };
            return;
        }
        self.apply_locked(&mut work, bar, defs);
    }

    /// `apply` under the caller's `work` guard, so no other apply can slip in
    /// between reading the remembered set and registering it.
    fn apply_locked(&self, work: &mut Work, bar: &str, defs: &[DirectHotkeyDef]) {
        self.release(work);
        let mut rows = Vec::with_capacity(defs.len());
        for (def, refused) in defs.iter().zip(refused_states(bar, defs)) {
            let state = refused.unwrap_or_else(|| {
                let combo = normalize_combo(&def.keys);
                match self.registry.register(&combo) {
                    Ok(()) => {
                        work.registered.push(combo);
                        KeyState::Ok
                    }
                    Err(RegisterError::Invalid) => KeyState::Invalid,
                    Err(RegisterError::Taken) => KeyState::Taken,
                }
            });
            rows.push((def.clone(), state));
        }
        work.bar = bar.to_string();
        work.defs = defs.to_vec();
        *self.view.lock().unwrap_or_else(|e| e.into_inner()) = View { rows, live: true };
    }

    fn release(&self, work: &mut Work) {
        self.view.lock().unwrap_or_else(|e| e.into_inner()).live = false;
        let ours: Vec<String> = work.stale.drain(..).chain(work.registered.drain(..)).collect();
        for combo in ours {
            if !self.registry.unregister(&combo) && !work.stale.contains(&combo) {
                work.stale.push(combo);
            }
        }
    }

    /// Unregisters everything. Never on the main thread.
    pub fn release_all(&self) {
        let mut work = self.work.lock().unwrap_or_else(|e| e.into_inner());
        self.release(&mut work);
    }

    /// Releases every combo while Settings captures keys (`true`), or applies the
    /// remembered set again (`false`). Never on the main thread.
    /// A resume when not paused does nothing.
    pub fn pause(&self, paused: bool) {
        let mut work = self.work.lock().unwrap_or_else(|e| e.into_inner());
        if paused {
            work.paused = true;
            self.release(&mut work);
        } else if work.paused {
            work.paused = false;
            let (bar, defs) = (work.bar.clone(), work.defs.clone());
            self.apply_locked(&mut work, &bar, &defs);
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

/// For each def, the state it gets without asking the registry (`Clash`, `Weak`,
/// `Duplicate`), or None when it is to be registered.
fn refused_states(bar: &str, defs: &[DirectHotkeyDef]) -> Vec<Option<KeyState>> {
    let bar_combo = normalize_combo(bar);
    let mut seen: Vec<String> = Vec::new();
    defs.iter()
        .map(|def| {
            let combo = normalize_combo(&def.keys);
            let state = if combo == bar_combo {
                Some(KeyState::Clash)
            } else if !combo_allowed(&def.keys) {
                Some(KeyState::Weak)
            } else if seen.contains(&combo) {
                Some(KeyState::Duplicate)
            } else {
                None
            };
            seen.push(combo);
            state
        })
        .collect()
}

/// The Shortcut for a combo, built from its normalized spelling (modifiers first,
/// Win as `super`), which is the only form global-hotkey reads for every spelling
/// the config accepts.
fn shortcut(keys: &str) -> Result<Shortcut, RegisterError> {
    Shortcut::from_str(&normalize_combo(keys)).map_err(|_| RegisterError::Invalid)
}

/// The real registry: the global-shortcut plugin. Never called on the main
/// thread, except during setup (see `DirectKeys::apply`).
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
        let gs = self.app.global_shortcut();
        // Held in the plugin's app-wide map, which the bar hotkey shares: never
        // touch it here. (`DirectKeys` frees its own leftovers before this.)
        if gs.is_registered(shortcut) {
            return Err(RegisterError::Taken);
        }
        let on_press = self.on_press;
        let keys = keys.to_string();
        gs.on_shortcut(shortcut, move |app, _, e| {
                if e.state == ShortcutState::Pressed {
                    on_press(app, &keys)
                }
            })
            .map_err(|_| RegisterError::Taken)
    }

    fn unregister(&self, keys: &str) -> bool {
        let Ok(shortcut) = shortcut(keys) else {
            return true;
        };
        match self.app.global_shortcut().unregister(shortcut) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("could not release direct hotkey {keys}: {}", error_kind(&e));
                false
            }
        }
    }
}

/// The error's kind only (its variant name), never any text it carries.
fn error_kind(e: &tauri_plugin_global_shortcut::Error) -> String {
    let debug = format!("{e:?}");
    debug.split(|c: char| !c.is_alphanumeric()).next().unwrap_or_default().to_string()
}

/// What a press acts on. `open` may block; never called on the main thread.
pub trait PressTarget: Send + Sync {
    fn item(&self, provider: &str, id: &str) -> Option<ResultItem>;
    /// False while the provider has not finished its first scan.
    fn ready(&self, provider: &str) -> bool;
    fn open(&self, provider: &str, id: &str) -> Result<Outcome, String>;
    fn show_armed(&self, item: ResultItem);
    /// The bar opens with `text` typed (an item that needs an argument).
    fn show_filled(&self, text: &str);
    /// The bar opens in the pick list `scope` (an item that asks to choose).
    fn show_step(&self, prompt: &str, scope: &str);
    fn follow_up(&self, then: Then);
    fn failed(&self, message: &str);
}

/// How long a press or a workspace waits for a provider's first scan (right
/// after login) before it gives up with [`STILL_STARTING`].
pub const STARTUP_WAIT: Duration = Duration::from_secs(3);

/// Said when an item's provider has not finished its first scan in time.
pub const STILL_STARTING: &str = "Flick is still starting";

/// Time between two looks at `ready` while waiting.
const READY_POLL: Duration = Duration::from_millis(50);

/// Waits until `ready` says true or `deadline` passes (looking every 50 ms);
/// true when it got ready. Blocks: never on the main thread.
pub fn wait_ready(ready: impl Fn() -> bool, deadline: Instant) -> bool {
    loop {
        if ready() {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep(READY_POLL.min(deadline - now));
    }
}

pub const REPEAT_GATE: Duration = Duration::from_millis(500);

/// Drops repeats of one combo while it is held (each repeat restarts the window).
/// Presses of one combo less than `REPEAT_GATE` apart are dropped on purpose,
/// even deliberate ones: a held key must open its item once, not again and again.
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

/// Opens `item` for a direct hotkey: a confirm item shows the bar armed; an
/// item that needs typing shows the bar filled or in its pick list; a gone item
/// does nothing; a failure is reported once; a cancel is silent. Right after
/// login the item's provider may not have scanned yet: the press waits up to
/// [`STARTUP_WAIT`] for it, then says [`STILL_STARTING`]. Never on the main thread.
pub fn press(target: &dyn PressTarget, item: &ItemRef) {
    press_waiting(target, item, STARTUP_WAIT);
}

fn press_waiting(target: &dyn PressTarget, item: &ItemRef, wait: Duration) {
    let mut row = target.item(&item.provider, &item.id);
    if row.is_none() && !target.ready(&item.provider) {
        if !wait_ready(|| target.ready(&item.provider), Instant::now() + wait) {
            target.failed(STILL_STARTING);
            return;
        }
        row = target.item(&item.provider, &item.id);
    }
    let Some(row) = row else {
        return;
    };
    if row.confirm {
        target.show_armed(row);
        return;
    }
    match target.open(&item.provider, &item.id) {
        Ok(Outcome::HideThen { then }) => target.follow_up(then),
        Ok(Outcome::Fill { text }) => target.show_filled(&text),
        Ok(Outcome::Step { prompt, scope }) => target.show_step(&prompt, &scope),
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

    /// Models the plugin's one app-wide map: `taken` is held by someone else
    /// (another program or the bar), `held` by us; `stuck` fails one release each.
    #[derive(Default)]
    struct FakeRegistry {
        calls: Mutex<Vec<String>>,
        taken: Vec<&'static str>,
        invalid: Vec<&'static str>,
        held: Mutex<Vec<String>>,
        stuck: Mutex<Vec<&'static str>>,
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
            let mut held = self.held.lock().unwrap();
            if self.taken.contains(&keys) || held.iter().any(|k| k == keys) {
                return Err(RegisterError::Taken);
            }
            held.push(keys.to_string());
            self.calls.lock().unwrap().push(format!("register {keys}"));
            Ok(())
        }
        fn unregister(&self, keys: &str) -> bool {
            self.calls.lock().unwrap().push(format!("unregister {keys}"));
            let mut stuck = self.stuck.lock().unwrap();
            if let Some(i) = stuck.iter().position(|k| *k == keys) {
                stuck.remove(i);
                return false;
            }
            self.held.lock().unwrap().retain(|k| k != keys);
            true
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
    fn apply_while_paused_registers_nothing_and_resume_registers_the_new_set() {
        let (reg, keys) = keys_with(FakeRegistry::default());
        keys.apply("Alt+Space", &[def("Ctrl+Alt+T", "t")]);
        keys.pause(true);
        keys.apply("Ctrl+Alt+E", &[def("Ctrl+Alt+E", "e"), def("Win+N", "n")]);
        assert_eq!(reg.calls(), ["register ctrl+alt+t", "unregister ctrl+alt+t"]);
        assert_eq!(keys.item_for("Ctrl+Alt+T"), None);
        assert_eq!(keys.item_for("Win+N"), None);
        assert_eq!(states(&keys), [("Ctrl+Alt+E".to_string(), KeyState::Clash), ("Win+N".to_string(), KeyState::Ok)]);
        keys.pause(false);
        assert_eq!(reg.calls(), ["register ctrl+alt+t", "unregister ctrl+alt+t", "register super+n"]);
        assert_eq!(states(&keys), [("Ctrl+Alt+E".to_string(), KeyState::Clash), ("Win+N".to_string(), KeyState::Ok)]);
        assert_eq!(keys.item_for("Win+N"), Some(item("n")));
        keys.pause(false);
        assert_eq!(reg.calls().len(), 3, "a second resume neither releases nor registers again");
        assert_eq!(keys.item_for("Win+N"), Some(item("n")));
    }

    #[test]
    fn a_combo_held_by_someone_else_is_taken_and_left_alone() {
        // e.g. the bar hotkey really registered is not the `bar` passed in.
        let (reg, keys) = keys_with(FakeRegistry { taken: vec!["ctrl+alt+b"], ..Default::default() });
        keys.apply("Alt+Space", &[def("Ctrl+Alt+B", "b"), def("Ctrl+Alt+T", "t")]);
        keys.apply("Alt+Space", &[def("Ctrl+Alt+B", "b")]);
        keys.release_all();
        assert_eq!(states(&keys), [("Ctrl+Alt+B".to_string(), KeyState::Taken)]);
        assert!(!reg.calls().iter().any(|c| c.ends_with("ctrl+alt+b")), "{:?}", reg.calls());
    }

    #[test]
    fn a_combo_whose_release_failed_is_freed_and_registered_again() {
        let (reg, keys) = keys_with(FakeRegistry::default());
        keys.apply("Alt+Space", &[def("Ctrl+Alt+T", "t")]);
        reg.stuck.lock().unwrap().push("ctrl+alt+t");
        keys.release_all();
        keys.apply("Alt+Space", &[def("Ctrl+Alt+T", "t")]);
        assert_eq!(
            reg.calls(),
            ["register ctrl+alt+t", "unregister ctrl+alt+t", "unregister ctrl+alt+t", "register ctrl+alt+t"]
        );
        assert_eq!(states(&keys)[0].1, KeyState::Ok);
        assert_eq!(keys.item_for("Ctrl+Alt+T"), Some(item("t")));
        keys.apply("Alt+Space", &[]);
        assert_eq!(reg.calls().len(), 5, "freed once, not retried again");
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
        /// Answers of `ready`, one per call; the last one stays. Empty: ready.
        ready: Mutex<Vec<bool>>,
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
            let ready = self.ready.lock().unwrap();
            if ready.first() == Some(&false) {
                return None;
            }
            self.item.clone()
        }
        fn ready(&self, _provider: &str) -> bool {
            let mut ready = self.ready.lock().unwrap();
            let now = ready.first().copied().unwrap_or(true);
            if ready.len() > 1 {
                ready.remove(0);
            }
            now
        }
        fn open(&self, provider: &str, id: &str) -> Result<Outcome, String> {
            self.calls.lock().unwrap().push(format!("open {provider} {id}"));
            self.open.lock().unwrap().take().expect("opened twice")
        }
        fn show_armed(&self, item: ResultItem) {
            self.calls.lock().unwrap().push(format!("armed {}", item.id));
        }
        fn show_filled(&self, text: &str) {
            self.calls.lock().unwrap().push(format!("filled {text}"));
        }
        fn show_step(&self, prompt: &str, scope: &str) {
            self.calls.lock().unwrap().push(format!("step {prompt} {scope}"));
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
    fn an_item_that_needs_typing_shows_the_bar_filled_or_in_its_list() {
        let fill = Outcome::Fill { text: "search ".into() };
        let t = FakeTarget::with(Some(row("need:search", false)), Ok(fill));
        press(&t, &item("need:search"));
        assert_eq!(t.calls(), ["item apps need:search", "open apps need:search", "filled search "]);

        let step = Outcome::Step { prompt: "proj".into(), scope: "shortcut:proj".into() };
        let t = FakeTarget::with(Some(row("step:proj", false)), Ok(step));
        press(&t, &item("step:proj"));
        assert_eq!(t.calls(), ["item apps step:proj", "open apps step:proj", "step proj shortcut:proj"]);
    }

    #[test]
    fn a_press_before_the_first_scan_waits_for_it() {
        let t = FakeTarget::with(Some(row("x", false)), Ok(Outcome::Hide));
        *t.ready.lock().unwrap() = vec![false, false, false, true];
        press_waiting(&t, &item("x"), Duration::from_secs(5));
        assert_eq!(t.calls(), ["item apps x", "item apps x", "open apps x"]);
    }

    #[test]
    fn a_press_says_still_starting_when_the_scan_takes_too_long() {
        let t = FakeTarget::with(Some(row("x", false)), Ok(Outcome::Hide));
        *t.ready.lock().unwrap() = vec![false];
        let start = Instant::now();
        press_waiting(&t, &item("x"), Duration::from_millis(120));
        assert!(start.elapsed() >= Duration::from_millis(120));
        assert_eq!(t.calls(), ["item apps x", "failed Flick is still starting"]);
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
