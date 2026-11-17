//! Self-updates: what the last check found, kept in memory only. The source
//! (the updater plugin in the app, a fake in tests) does the network work.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::{settings, tray, window, AppState};

/// A newer release.
#[derive(Clone, Debug, PartialEq)]
pub struct Available {
    pub version: String,
    pub notes: String,
}

/// Where updates come from. Both calls may block for a long time.
pub trait UpdateSource: Send + Sync {
    /// `Ok(None)`: this is the latest version.
    fn check(&self) -> Result<Option<Available>, String>;
    /// Downloads and installs what the last `check` found, then restarts Flick.
    fn install(&self) -> Result<(), String>;
}

#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateState {
    Idle,
    Available { version: String, notes: String },
    Downloading { version: String },
    Failed { version: String, reason: String },
}

struct Inner {
    state: UpdateState,
    last_check: Option<jiff::Timestamp>,
    enabled: bool,
}

pub struct Updates {
    source: Arc<dyn UpdateSource>,
    on_change: Arc<dyn Fn() + Send + Sync>,
    inner: Mutex<Inner>,
}

impl Updates {
    /// `on_change` runs (with no lock held) whenever what the bar shows may differ.
    pub fn new(source: Arc<dyn UpdateSource>, on_change: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            source,
            on_change,
            inner: Mutex::new(Inner { state: UpdateState::Idle, last_check: None, enabled: true }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Asks the source. A failed check changes nothing and does not count as checked.
    pub fn check_now(&self) -> Result<(), String> {
        let found = self.source.check()?;
        {
            let mut inner = self.lock();
            inner.last_check = Some(jiff::Timestamp::now());
            // An install in progress is not interrupted by a check.
            if !matches!(inner.state, UpdateState::Downloading { .. }) {
                inner.state = match found {
                    Some(a) => UpdateState::Available { version: a.version, notes: a.notes },
                    None => UpdateState::Idle,
                };
            }
        }
        (self.on_change)();
        Ok(())
    }

    /// Installs what was found (also retries after a failure). Blocks until the
    /// source is done; on success the source restarts Flick.
    pub fn install(&self) {
        let version = {
            let mut inner = self.lock();
            let version = match &inner.state {
                UpdateState::Available { version, .. } | UpdateState::Failed { version, .. } => version.clone(),
                _ => return,
            };
            inner.state = UpdateState::Downloading { version: version.clone() };
            version
        };
        (self.on_change)();
        if let Err(reason) = self.source.install() {
            self.lock().state = UpdateState::Failed { version, reason };
            (self.on_change)();
        }
    }

    pub fn state(&self) -> UpdateState {
        self.lock().state.clone()
    }

    /// What the bar and tray show: nothing while `check_updates` is off.
    pub fn visible_state(&self) -> UpdateState {
        let inner = self.lock();
        if inner.enabled {
            inner.state.clone()
        } else {
            UpdateState::Idle
        }
    }

    pub fn last_check(&self) -> Option<jiff::Timestamp> {
        self.lock().last_check
    }

    /// The `check_updates` switch. Off hides what was found; it is kept.
    pub fn set_enabled(&self, check_updates: bool) {
        let changed = {
            let mut inner = self.lock();
            let changed = inner.enabled != check_updates;
            inner.enabled = check_updates;
            changed
        };
        if changed {
            (self.on_change)();
        }
    }
}

/// Automatic checks only run in release builds, with the switch on and the network allowed.
pub fn automatic_check_allowed(check_updates: bool, online: bool, debug: bool) -> bool {
    check_updates && online && !debug
}

/// The updater plugin as a source.
pub struct PluginSource {
    app: AppHandle,
    /// What the last check found, for `install`.
    found: Mutex<Option<Update>>,
}

impl PluginSource {
    pub fn new(app: AppHandle) -> Self {
        Self { app, found: Mutex::new(None) }
    }
}

impl UpdateSource for PluginSource {
    fn check(&self) -> Result<Option<Available>, String> {
        // Called from a worker thread, never from the main thread.
        let update = tauri::async_runtime::block_on(async {
            let updater = self.app.updater().map_err(|e| e.to_string())?;
            updater.check().await.map_err(|e| e.to_string())
        })?;
        let available =
            update.as_ref().map(|u| Available { version: u.version.clone(), notes: u.body.clone().unwrap_or_default() });
        *self.found.lock().unwrap_or_else(|e| e.into_inner()) = update;
        Ok(available)
    }

    fn install(&self) -> Result<(), String> {
        let found = self.found.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(update) = found else {
            return Err("no update to install".into());
        };
        tauri::async_runtime::block_on(update.download_and_install(|_, _| {}, || {}))
            .map_err(|e| e.to_string())?;
        self.app.restart()
    }
}

/// Wires the in-memory state to the bar, the Settings window and the tray.
pub fn on_change(app: AppHandle) -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(move || {
        let _ = app.emit_to(window::LABEL, "answers:ready", ());
        let _ = app.emit_to(settings::WINDOW, "settings:changed", ());
        if let Some(state) = app.try_state::<AppState>() {
            let label = match state.updates.visible_state() {
                UpdateState::Available { version, .. } | UpdateState::Failed { version, .. } => Some(version),
                _ => None,
            };
            tray::set_update(&app, label.as_deref());
        }
    })
}

const FIRST_CHECK_DELAY: std::time::Duration = std::time::Duration::from_secs(10);
const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// 10 s after start, then every 24 h: check when allowed. The config is read
/// afresh each time and not held while checking. Not started in debug builds.
pub fn start_scheduler(app: &AppHandle) {
    if cfg!(debug_assertions) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK_DELAY);
        loop {
            let state = app.state::<AppState>();
            let (check_updates, online) = {
                let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
                (config.check_updates, config.online)
            };
            if automatic_check_allowed(check_updates, online, cfg!(debug_assertions)) {
                let _ = state.updates.check_now();
            }
            std::thread::sleep(CHECK_INTERVAL);
        }
    });
}

#[derive(Serialize)]
pub struct UpdateStatus {
    state: UpdateState,
    /// RFC 3339.
    last_check: Option<String>,
    current: String,
}

#[tauri::command(async)]
pub fn settings_update_status(app: AppHandle, state: tauri::State<AppState>) -> UpdateStatus {
    UpdateStatus {
        state: state.updates.state(),
        last_check: state.updates.last_check().map(|t| t.to_string()),
        current: app.package_info().version.to_string(),
    }
}

/// Always runs, whatever `check_updates` and `online` say.
#[tauri::command]
pub async fn settings_check_now(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let updates = state.updates.clone();
    tauri::async_runtime::spawn_blocking(move || updates.check_now())
        .await
        .map_err(|e| e.to_string())?
}

/// Starts the install and returns; progress shows through `settings:changed`.
#[tauri::command]
pub async fn settings_install_update(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let updates = state.updates.clone();
    std::thread::spawn(move || updates.install());
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Scripted results; an empty script means "no update" / success.
    #[derive(Default)]
    pub(crate) struct FakeSource {
        checks: Mutex<VecDeque<Result<Option<Available>, String>>>,
        installs: Mutex<VecDeque<Result<(), String>>>,
    }

    impl FakeSource {
        pub(crate) fn check_returns(self, r: Result<Option<Available>, String>) -> Self {
            self.checks.lock().unwrap().push_back(r);
            self
        }
        pub(crate) fn install_returns(self, r: Result<(), String>) -> Self {
            self.installs.lock().unwrap().push_back(r);
            self
        }
    }

    impl UpdateSource for FakeSource {
        fn check(&self) -> Result<Option<Available>, String> {
            self.checks.lock().unwrap().pop_front().unwrap_or(Ok(None))
        }
        fn install(&self) -> Result<(), String> {
            self.installs.lock().unwrap().pop_front().unwrap_or(Ok(()))
        }
    }

    pub(crate) fn new_available(version: &str) -> Available {
        Available { version: version.into(), notes: "Fixes".into() }
    }

    fn updates(source: FakeSource) -> (Updates, Arc<AtomicUsize>) {
        let changes = Arc::new(AtomicUsize::new(0));
        let c = changes.clone();
        let u = Updates::new(
            Arc::new(source),
            Arc::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        );
        (u, changes)
    }

    #[test]
    fn found_update_becomes_available() {
        let (u, changes) = updates(FakeSource::default().check_returns(Ok(Some(new_available("0.4.0")))));
        assert_eq!(u.state(), UpdateState::Idle);
        u.check_now().unwrap();
        assert_eq!(u.state(), UpdateState::Available { version: "0.4.0".into(), notes: "Fixes".into() });
        assert_eq!(u.visible_state(), u.state());
        assert!(u.last_check().is_some());
        assert_eq!(changes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn no_update_is_idle_and_records_last_check() {
        let (u, _) = updates(
            FakeSource::default().check_returns(Ok(Some(new_available("0.4.0")))).check_returns(Ok(None)),
        );
        u.check_now().unwrap();
        u.check_now().unwrap();
        assert_eq!(u.state(), UpdateState::Idle);
        assert!(u.last_check().is_some());
    }

    #[test]
    fn failed_check_keeps_state_and_last_check() {
        let (u, changes) = updates(FakeSource::default().check_returns(Err("404".into())));
        assert_eq!(u.check_now(), Err("404".to_string()));
        assert_eq!(u.state(), UpdateState::Idle);
        assert_eq!(u.last_check(), None);
        assert_eq!(changes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failed_check_keeps_what_was_found() {
        let (u, _) = updates(
            FakeSource::default().check_returns(Ok(Some(new_available("0.4.0")))).check_returns(Err("offline".into())),
        );
        u.check_now().unwrap();
        let before = u.last_check();
        assert!(u.check_now().is_err());
        assert!(matches!(u.state(), UpdateState::Available { .. }));
        assert_eq!(u.last_check(), before);
    }

    #[test]
    fn install_failure_shows_failed_and_retry_works() {
        let (u, _) = updates(
            FakeSource::default()
                .check_returns(Ok(Some(new_available("0.4.0"))))
                .install_returns(Err("bad signature".into()))
                .install_returns(Ok(())),
        );
        u.install(); // nothing found yet: ignored
        assert_eq!(u.state(), UpdateState::Idle);
        u.check_now().unwrap();
        u.install();
        assert_eq!(u.state(), UpdateState::Failed { version: "0.4.0".into(), reason: "bad signature".into() });
        u.install();
        // The source restarts Flick on success, so the state stays Downloading.
        assert_eq!(u.state(), UpdateState::Downloading { version: "0.4.0".into() });
    }

    #[test]
    fn disabled_hides_but_remembers() {
        let (u, changes) = updates(FakeSource::default().check_returns(Ok(Some(new_available("0.4.0")))));
        u.check_now().unwrap();
        u.set_enabled(false);
        assert_eq!(u.visible_state(), UpdateState::Idle);
        assert!(matches!(u.state(), UpdateState::Available { .. }));
        u.set_enabled(false); // unchanged: no notification
        assert_eq!(changes.load(Ordering::SeqCst), 2);
        u.set_enabled(true);
        assert!(matches!(u.visible_state(), UpdateState::Available { .. }));
    }

    #[test]
    fn automatic_check_truth_table() {
        for check in [false, true] {
            for online in [false, true] {
                for debug in [false, true] {
                    assert_eq!(
                        automatic_check_allowed(check, online, debug),
                        (check, online, debug) == (true, true, false),
                        "{check} {online} {debug}"
                    );
                }
            }
        }
    }
}
