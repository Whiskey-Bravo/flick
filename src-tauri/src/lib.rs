mod answers;
mod clipboard;
mod commands;
mod config;
mod engine;
mod fuzzy;
mod history;
mod hotkey;
mod icon;
mod launch;
mod net;
mod pins;
mod provider;
mod process;
mod providers;
mod quote;
mod ranking;
mod tray;
mod window;
mod wsl_detect;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;

use answers::number::{Formats, WindowsFormats};
use answers::rates::RatesStore;
use answers::weather::WeatherStore;
use clipboard::WindowsClipboard;
use config::Config;
use engine::Engine;
use launch::{Launcher, SystemLauncher};
use provider::Provider;
use providers::answers::{AnswersConfig, AnswersProvider, DUCKDUCKGO};
use providers::apps::AppsProvider;
use providers::files::FilesProvider;
use providers::flick::{FlickProvider, Opener};
use providers::history::HistoryProvider;
use providers::power::{PowerProvider, PowerRunner, SystemPower};
use providers::settings::SettingsProvider;
use providers::shortcuts::ShortcutsProvider;
use providers::welcome::WelcomeProvider;
use providers::wsl::WslProvider;
use history::History;
use pins::Pins;
use ranking::Ranking;

const APPS_REFRESH_INTERVAL: Duration = Duration::from_secs(10 * 60);
const FILES_REFRESH_INTERVAL: Duration = Duration::from_secs(30 * 60);
const SHORTCUTS_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// The providers that are refreshed in the background or take config, shared
/// between the engine and the threads that keep them up to date.
#[derive(Clone)]
struct Providers {
    answers: Arc<AnswersProvider>,
    /// The answers' weather store: the home city is set on it off the query path.
    weather: Arc<WeatherStore>,
    rates: Arc<RatesStore>,
    /// The Windows locale: defaults for the answers' currency and temperature unit.
    formats: Arc<dyn Formats>,
    apps: Arc<AppsProvider>,
    files: Arc<FilesProvider>,
    shortcuts: Arc<ShortcutsProvider>,
    wsl: Arc<WslProvider>,
    welcome: Arc<WelcomeProvider>,
}

impl Providers {
    /// Cheap: no disk access. File roots and shortcuts arrive with the config.
    fn new(answers: Answers, apps: Arc<AppsProvider>, launcher: Arc<dyn Launcher>, distro: &str) -> Self {
        Self {
            answers: answers.provider,
            weather: answers.weather,
            rates: answers.rates,
            formats: answers.formats,
            apps,
            files: FilesProvider::new(launcher.clone()),
            shortcuts: ShortcutsProvider::new(launcher.clone(), distro),
            wsl: WslProvider::new(launcher, distro),
            welcome: WelcomeProvider::new(Box::new(window::shows)),
        }
    }
}

/// The answers provider and the stores behind it.
struct Answers {
    provider: Arc<AnswersProvider>,
    weather: Arc<WeatherStore>,
    rates: Arc<RatesStore>,
    formats: Arc<dyn Formats>,
}

impl Answers {
    /// Cheap: reads the locale and time zone, no files or network. The stores
    /// load on a background thread (`load_answer_stores`); `on_ready` runs
    /// when a fetch has finished.
    fn new(launcher: Arc<dyn Launcher>, on_ready: Arc<dyn Fn() + Send + Sync>) -> Self {
        let formats: Arc<dyn Formats> = Arc::new(WindowsFormats::new());
        let fetch: Arc<dyn net::Fetch> = Arc::new(net::WinHttp);
        let rates = RatesStore::new(rates_path(), fetch.clone(), on_ready.clone());
        let weather = WeatherStore::new(weather_path(), fetch, on_ready);
        let provider = AnswersProvider::new(
            formats.clone(),
            rates.clone(),
            weather.clone(),
            Arc::new(WindowsClipboard),
            launcher,
            Box::new(launch::open_config),
            Arc::new(jiff::Timestamp::now),
        );
        Answers { provider, weather, rates, formats }
    }
}

/// The answer keys of `c` with their defaults filled in: currency from the
/// Windows region (EUR when that is not an ECB currency), temperature unit
/// from the region, DuckDuckGo as the search engine.
fn answers_config(c: &Config, formats: &dyn Formats) -> AnswersConfig {
    let known = |code: &String| answers::units::CURRENCIES.contains(&code.as_str());
    let currency = Some(c.currency.trim().to_uppercase())
        .filter(|code| !code.is_empty())
        .or_else(|| formats.currency().map(|code| code.to_uppercase()).filter(known))
        .unwrap_or_else(|| "EUR".to_string());
    let fahrenheit = match c.temperature_unit.as_str() {
        "f" => true,
        "c" => false,
        _ => formats.fahrenheit(),
    };
    let search_engine = if c.search_engine.is_empty() { DUCKDUCKGO.to_string() } else { c.search_engine.clone() };
    AnswersConfig {
        online: c.online,
        weather_city: c.weather_city.trim().to_string(),
        currency,
        fahrenheit,
        search_engine,
    }
}

pub(crate) struct AppState {
    pub(crate) engine: Arc<Engine>,
    /// Shared with the engine; the config's `history` switch is applied to it.
    history: Arc<History>,
    providers: Providers,
    /// Last good config. Held for the whole of a reload, which serialises
    /// reloads and every push of config into the providers.
    config: Mutex<Config>,
    /// Result of the last successful WSL detection; `Unknown` until one
    /// succeeds. Lock order: `config`, then this.
    detected: Mutex<WslState>,
    /// Configured accelerator last applied successfully; `None` when the last
    /// `hotkey::apply` failed (the previous hotkey may then still be active).
    hotkey: Mutex<Option<String>>,
    /// Keeps the config watcher alive for the life of the app.
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    /// Latest config/hotkey error text, re-sent on every show so an error from
    /// startup (emitted before the page listened) still reaches the bar.
    config_error: Mutex<Option<String>>,
}

impl AppState {
    pub(crate) fn config_error(&self) -> Option<String> {
        self.config_error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The one place providers are put together: used by `run()` and by the tests.
/// Registration order is the order of the help list.
fn build_engine(
    p: &Providers,
    power: Box<dyn PowerRunner>,
    open_config: Opener,
    ranking: Arc<Ranking>,
    history: Arc<History>,
    pins: Arc<Pins>,
) -> Engine {
    let providers: Vec<Box<dyn Provider>> = vec![
        Box::new(p.answers.clone()),
        Box::new(p.apps.clone()),
        Box::new(p.files.clone()),
        Box::new(p.shortcuts.clone()),
        Box::new(p.wsl.clone()),
        Box::new(SettingsProvider),
        Box::new(PowerProvider::new(power)),
        Box::new(FlickProvider::new(open_config)),
        Box::new(HistoryProvider::new(history.clone())),
        // Last: it has no help line, so the help list is unchanged.
        Box::new(p.welcome.clone()),
    ];
    Engine::new(providers, ranking, history, pins)
}

/// What detection has said about WSL so far.
#[derive(Clone, Debug, PartialEq)]
enum WslState {
    /// No detection has succeeded yet (not run, or it timed out).
    Unknown,
    /// Detection found no usable WSL.
    NoWsl,
    /// Detection found this default distro.
    Distro(String),
}

impl WslState {
    /// `None` for a failed detection: the caller keeps its previous state.
    fn from_detection(detection: wsl_detect::Detection) -> Option<WslState> {
        match detection {
            wsl_detect::Detection::Found(Some(d)) => Some(WslState::Distro(d)),
            wsl_detect::Detection::Found(None) => Some(WslState::NoWsl),
            wsl_detect::Detection::Failed => None,
        }
    }
}

/// The distro to run WSL commands in: `""` when detection positively found no
/// WSL; else the configured one; else the detected one, else `""`.
fn effective_distro(configured: &str, detected: &WslState) -> String {
    match detected {
        WslState::NoWsl => String::new(),
        _ if !configured.is_empty() => configured.to_string(),
        WslState::Distro(d) => d.clone(),
        WslState::Unknown => String::new(),
    }
}

/// Keeps the previous state when `wsl.exe` did not answer in time.
fn record_detection(state: &AppState, detection: wsl_detect::Detection) {
    if let Some(new) = WslState::from_detection(detection) {
        *state.detected.lock().unwrap_or_else(|e| e.into_inner()) = new;
    }
}

/// Puts `config`'s shortcuts and the effective distro into the providers.
/// `detected` decides whether the configured or detected distro is used.
/// `shortcuts.set` reads the pick folders: never call on the main thread.
fn push_distro(p: &Providers, config: &Config, detected: &WslState) {
    let distro = effective_distro(&config.wsl_distro, detected);
    p.shortcuts.set(config.shortcuts.clone(), &distro, &config.search_engine);
    p.wsl.set_distro(&distro);
}

/// Stores a detection and pushes the resulting distro with the last good
/// config, whether or not the config file currently parses. Takes the config
/// lock (then `detected`); call with no lock held, after `wsl.exe` has run.
fn apply_detection(state: &AppState, detection: wsl_detect::Detection) {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    record_detection(state, detection);
    let detected = state.detected.lock().unwrap_or_else(|e| e.into_inner()).clone();
    push_distro(&state.providers, &config, &detected);
}

/// Push a newly loaded `new` config (replacing `old`) into the providers;
/// `detected` decides whether the configured or detected distro is used.
/// `shortcuts.set` reads the pick folders: never call on the main thread.
/// Returns true when the file roots changed; the caller then rescans the
/// files in the background (the watcher has already moved to the new roots).
fn push_config(p: &Providers, old: &Config, new: &Config, detected: &WslState) -> bool {
    push_distro(p, new, detected);
    // Unchanged roots are not re-set: that would restart a running scan and
    // re-register the watcher for nothing.
    let changed = old.file_roots != new.file_roots;
    if changed {
        p.files.set_roots(new.file_roots.clone());
    }
    changed
}

fn picks_path() -> PathBuf {
    config::config_path().with_file_name("picks.json")
}

fn history_path() -> PathBuf {
    config::config_path().with_file_name("history.json")
}

fn pins_path() -> PathBuf {
    config::config_path().with_file_name("pins.json")
}

fn rates_path() -> PathBuf {
    config::config_path().with_file_name("rates.json")
}

fn weather_path() -> PathBuf {
    config::config_path().with_file_name("weather.json")
}

pub fn run() {
    // Everything Flick starts inherits this directory unless told otherwise:
    // the user profile, not wherever Flick happened to be started from (and
    // Flick then holds no handle on that folder).
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let _ = std::env::set_current_dir(home);
    }
    let icons = icon::IconServer::new(Arc::new(icon::ShellIcons));
    tauri::Builder::default()
        // Must be first: a second launch shows the running instance and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| window::show(app)))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::query,
            commands::activate,
            commands::hide,
            commands::toggle_pin,
            commands::past_searches,
            commands::reveal
        ])
        // Result-row icons. Answered on the icon workers, never on this thread.
        .register_asynchronous_uri_scheme_protocol(icon::SCHEME, move |_ctx, request, responder| {
            let uri = request.uri();
            icons.request(uri.path(), uri.query(), move |status, body| {
                responder.respond(icon::response(status, body));
            });
        })
        .setup(|app| {
            setup(app.handle());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Flick");
}

/// Startup order: config → providers and engine → background index threads →
/// hotkey → tray → config watcher. Nothing here may stop startup, and nothing
/// here scans the disk: every index is built on a background thread.
fn setup(app: &AppHandle) {
    let path = config::config_path();
    let first_run = !path.exists();
    let loaded = config::load(&path, None);

    // One launcher for every provider that starts something.
    let launcher: Arc<dyn Launcher> = Arc::new(SystemLauncher);
    let ready = app.clone();
    // The bar re-runs its query when the text is unchanged, so a placeholder turns into the answer.
    let answers = Answers::new(launcher.clone(), Arc::new(move || {
        let _ = ready.emit_to(window::LABEL, "answers:ready", ());
    }));
    let providers = Providers::new(answers, AppsProvider::new(launcher.clone()), launcher, &effective_distro(&loaded.config.wsl_distro, &WslState::Unknown));
    providers.answers.set_config(&answers_config(&loaded.config, providers.formats.as_ref()));
    // Cheap (no watcher yet, no scan): the first file scan uses these roots.
    providers.files.set_roots(loaded.config.file_roots.clone());
    let ranking = Arc::new(Ranking::load(picks_path()));
    let history = Arc::new(History::load(history_path(), loaded.config.history));
    let pins = Arc::new(Pins::load(pins_path()));
    let engine = build_engine(
        &providers,
        Box::new(SystemPower),
        Box::new(launch::open_config),
        ranking,
        history.clone(),
        pins,
    );
    app.manage(AppState {
        engine: Arc::new(engine),
        history,
        providers: providers.clone(),
        config: Mutex::new(loaded.config.clone()),
        detected: Mutex::new(WslState::Unknown),
        hotkey: Mutex::new(None),
        watcher: Mutex::new(None),
        config_error: Mutex::new(None),
    });

    start_index_threads(app, providers);

    window::install_handlers(app);

    let hotkey_error = apply_hotkey(app, &loaded.config.hotkey);
    if first_run {
        // Armed before the show: the empty query that `bar:shown` triggers must find it ready.
        let state = app.state::<AppState>();
        state.providers.welcome.arm(hotkey_error.is_none().then_some(loaded.config.hotkey.as_str()));
        window::show(app);
    }

    // Before the tray, so its check mark reads the new state. Debug builds would
    // register the debug exe for start-at-login, so they skip this.
    if first_run && !cfg!(debug_assertions) {
        let _ = app.autolaunch().enable();
    }
    let _ = tray::build(app, &tooltip(hotkey_error.as_deref()), |app| {
        let app = app.clone();
        // Off the main thread: all of these can block. Config first, so a
        // slow PowerShell or file scan never delays it.
        std::thread::spawn(move || {
            // No lock held while `wsl.exe` runs. Pushed even when the config
            // has an error, which makes the reload skip its own push.
            let detection = wsl_detect::detect();
            apply_detection(&app.state::<AppState>(), detection);
            reload_config(&app);
            let state = app.state::<AppState>();
            let p = &state.providers;
            p.shortcuts.refresh();
            p.apps.refresh();
            p.files.refresh();
        });
    });

    let watch_handle = app.clone();
    if let Ok(w) = config::watch(path, move || reload_config(&watch_handle)) {
        *app.state::<AppState>().watcher.lock().unwrap_or_else(|e| e.into_inner()) = Some(w);
    }

    emit_config_error(app, join_errors(loaded.error, hotkey_error));
}

/// The threads that build and refresh the indexes, each doing its disk work
/// with no lock held that a query needs (build, then swap):
/// - apps: now, then every 10 minutes;
/// - shortcuts: the config's definitions now, pick folders every 5 minutes;
/// - files: the folder watcher, a scan now, then a rescan every 30 minutes.
fn start_index_threads(app: &AppHandle, p: Providers) {
    load_answer_stores(app, &p);

    let apps = p.apps;
    std::thread::spawn(move || loop {
        apps.refresh();
        std::thread::sleep(APPS_REFRESH_INTERVAL);
    });

    let handle = app.clone();
    let shortcuts = p.shortcuts;
    std::thread::spawn(move || {
        let state = handle.state::<AppState>();
        {
            // Right away, with no detection yet. Under the config lock, so
            // this never replaces the definitions of a reload that ran first
            // with older ones.
            let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
            let detected = state.detected.lock().unwrap_or_else(|e| e.into_inner()).clone();
            push_distro(&state.providers, &config, &detected);
        }
        // `wsl.exe` can take seconds: run it with no lock held.
        apply_detection(&state, wsl_detect::detect());
        loop {
            std::thread::sleep(SHORTCUTS_REFRESH_INTERVAL);
            // A timed-out (or never successful) detection is retried here,
            // again with no lock held.
            let unknown = *state.detected.lock().unwrap_or_else(|e| e.into_inner()) == WslState::Unknown;
            if unknown {
                apply_detection(&state, wsl_detect::detect());
            }
            shortcuts.refresh();
        }
    });

    let handle = app.clone();
    let files = p.files;
    std::thread::spawn(move || {
        {
            // Under the config lock: a reload's `set_roots` then runs either
            // before (and the watcher starts on its roots) or after (and
            // moves the watcher).
            let state = handle.state::<AppState>();
            let _config = state.config.lock().unwrap_or_else(|e| e.into_inner());
            // Without a watcher the periodic rescan still picks changes up.
            let _ = files.watch();
        }
        loop {
            files.refresh();
            std::thread::sleep(FILES_REFRESH_INTERVAL);
        }
    });
}

/// Reads `rates.json` and `weather.json`, then gives the weather store the home
/// city of the current config. Under the config lock, so a reload never sets
/// the home city between these steps.
fn load_answer_stores(app: &AppHandle, p: &Providers) {
    let (rates, weather) = (p.rates.clone(), p.weather.clone());
    let handle = app.clone();
    std::thread::spawn(move || {
        let state = handle.state::<AppState>();
        let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
        rates.load();
        weather.load();
        weather.set_home(config.weather_city.trim());
    });
}

/// Registers `accelerator`, records the result and updates the tray tooltip.
/// Returns the user-facing error text on failure.
fn apply_hotkey(app: &AppHandle, accelerator: &str) -> Option<String> {
    let result = hotkey::apply(app, accelerator);
    let state = app.state::<AppState>();
    *state.hotkey.lock().unwrap_or_else(|e| e.into_inner()) = result.is_ok().then(|| accelerator.to_string());
    let error = result.err();
    tray::set_tooltip(app, &tooltip(error.as_deref()));
    error
}

/// Tray tooltip: "Flick", or "Flick — <hotkey error>".
fn tooltip(error: Option<&str>) -> String {
    match error {
        Some(e) => format!("{} \u{2014} {e}", tray::DEFAULT_TOOLTIP),
        None => tray::DEFAULT_TOOLTIP.to_string(),
    }
}

/// Reload with last-good fallback; push a successfully loaded config into the
/// providers (rescanning files in the background when their folders changed);
/// re-apply the hotkey if it changed (or the last attempt failed); emit
/// `config:error`. Never call on the main thread.
fn reload_config(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let loaded = config::load(&config::config_path(), Some(&config));
    // On error `loaded.config` is the last good one, already pushed.
    let detected = state.detected.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if loaded.error.is_none() && push_config(&state.providers, &config, &loaded.config, &detected) {
        let files = state.providers.files.clone();
        std::thread::spawn(move || files.refresh());
    }
    // Under the config lock, so concurrent reloads apply the switch in order.
    // At most one `remove_file`, and only when the switch changed.
    if loaded.config.history != config.history {
        state.history.set_enabled(loaded.config.history);
    }
    let p = &state.providers;
    p.answers.set_config(&answers_config(&loaded.config, p.formats.as_ref()));
    // Touches weather.json: only when the home city changed.
    if loaded.config.weather_city.trim() != config.weather_city.trim() {
        p.weather.set_home(loaded.config.weather_city.trim());
    }
    *config = loaded.config;
    let applied = state.hotkey.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let hotkey_error = if hotkey_needs_apply(applied.as_deref(), &config.hotkey) {
        apply_hotkey(app, &config.hotkey)
    } else {
        None
    };
    emit_config_error(app, join_errors(loaded.error, hotkey_error));
}

/// `applied` is the configured hotkey that last registered successfully (`None`
/// after a failure). Re-apply when the config asks for something else, which
/// also retries a hotkey that failed before.
fn hotkey_needs_apply(applied: Option<&str>, wanted: &str) -> bool {
    applied != Some(wanted)
}

/// Config error first, then hotkey error.
fn join_errors(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (a, b) => a.or(b),
    }
}

/// Stores the error for later shows, then emits it now.
fn emit_config_error(app: &AppHandle, error: Option<String>) {
    *app.state::<AppState>().config_error.lock().unwrap_or_else(|e| e.into_inner()) = error.clone();
    let _ = app.emit_to(window::LABEL, "config:error", error);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use config::ShortcutDef;
    use launch::testing::RecordingLauncher;
    use provider::Outcome;
    use providers::apps::{AppEntry, AppKind};
    use providers::files::FileEntry;
    use providers::power::PowerAction;

    struct RecordingRunner(Arc<Mutex<Vec<PowerAction>>>);

    impl PowerRunner for RecordingRunner {
        fn run(&self, a: PowerAction) -> Result<(), String> {
            self.0.lock().unwrap().push(a);
            Ok(())
        }
    }

    /// Every real provider, wired as in `run()`, with nothing that can start a
    /// process: a recording launcher, a recording power runner and a counting
    /// config opener. Keep `dir` alive for the whole test (it holds picks.json).
    struct Rig {
        engine: Engine,
        providers: Providers,
        launcher: Arc<RecordingLauncher>,
        power: Arc<Mutex<Vec<PowerAction>>>,
        opened: Arc<AtomicUsize>,
        dir: tempfile::TempDir,
    }

    fn rig(apps: Vec<AppEntry>, files: Vec<FileEntry>) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let ranking = Arc::new(Ranking::load(dir.path().join("picks.json")));
        let launcher = Arc::new(RecordingLauncher::default());
        let fetch = Arc::new(net::FakeFetch::offline());
        let rates = RatesStore::new(dir.path().join("rates.json"), fetch.clone(), Arc::new(|| {}));
        let weather = WeatherStore::new(dir.path().join("weather.json"), fetch, Arc::new(|| {}));
        let formats: Arc<dyn Formats> = Arc::new(answers::number::TestFormats { decimal: ',', group: '.' });
        let answers = AnswersProvider::new(
            formats.clone(),
            rates.clone(),
            weather.clone(),
            Arc::new(clipboard::FakeClipboard::default()),
            launcher.clone(),
            Box::new(|| Ok(())),
            Arc::new(|| "2026-11-17T18:00:00Z".parse().unwrap()),
        );
        let providers = Providers {
            answers,
            weather,
            rates,
            formats,
            apps: AppsProvider::with_entries(apps),
            files: FilesProvider::with_entries(launcher.clone(), files),
            shortcuts: ShortcutsProvider::new(launcher.clone(), "Debian"),
            wsl: WslProvider::new(launcher.clone(), "Debian"),
            welcome: WelcomeProvider::new(Box::new(|| 0)),
        };
        let power = Arc::new(Mutex::new(Vec::new()));
        let opened = Arc::new(AtomicUsize::new(0));
        let counter = opened.clone();
        let engine = build_engine(
            &providers,
            Box::new(RecordingRunner(power.clone())),
            Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            ranking,
            Arc::new(History::load(dir.path().join("history.json"), true)),
            Arc::new(Pins::load(dir.path().join("pins.json"))),
        );
        Rig { engine, providers, launcher, power, opened, dir }
    }

    fn app(name: &str) -> AppEntry {
        AppEntry {
            name: name.into(),
            target: format!(r"C:\definitely\missing\{name}.lnk"),
            kind: AppKind::Shortcut,
            aliases: Vec::new(),
        }
    }

    fn file(path: &str) -> FileEntry {
        FileEntry::new(PathBuf::from(path), false)
    }

    fn shortcut(name: &str, run: &str) -> ShortcutDef {
        ShortcutDef {
            name: name.into(),
            aliases: vec![],
            about: String::new(),
            run: Some(run.into()),
            wsl: None,
            open: None,
            terminal: None,
            admin: false,
            pick: None,
            choices: vec![],
        }
    }

    fn titles(items: &[provider::ResultItem]) -> Vec<String> {
        items.iter().map(|i| i.title.clone()).collect()
    }

    #[test]
    fn answers_sit_above_an_app_with_the_same_text() {
        let r = rig(vec![app("12*7")], vec![]);
        let res = r.engine.query("12*7", None);
        assert_eq!((res[0].provider, res[0].title.as_str()), ("answers", "= 84"));
        assert_eq!((res[1].provider, res[1].title.as_str()), ("apps", "12*7"));
        // A bare keyword ranks like a name.
        let r = rig(vec![app("Time")], vec![]);
        assert_eq!(r.engine.query("time", None)[0].provider, "apps");
        assert!(r.launcher.calls().is_empty());
    }

    /// Windows formats as a region may report them.
    struct Region(Option<&'static str>, bool);

    impl Formats for Region {
        fn number(&self, x: f64) -> String {
            x.to_string()
        }
        fn date(&self, d: jiff::civil::Date, _long: bool) -> String {
            d.to_string()
        }
        fn time(&self, t: jiff::civil::Time) -> String {
            t.to_string()
        }
        fn fahrenheit(&self) -> bool {
            self.1
        }
        fn currency(&self) -> Option<String> {
            self.0.map(String::from)
        }
    }

    #[test]
    fn answers_config_fills_defaults_from_the_region() {
        let c = answers_config(&Config::default(), &Region(Some("USD"), true));
        assert_eq!(
            c,
            AnswersConfig {
                online: true,
                weather_city: String::new(),
                currency: "USD".into(),
                fahrenheit: true,
                search_engine: "https://duckduckgo.com/?q={arg}".into(),
            }
        );
        // Not an ECB currency, or none: EUR.
        assert_eq!(answers_config(&Config::default(), &Region(Some("XAF"), false)).currency, "EUR");
        assert_eq!(answers_config(&Config::default(), &Region(None, false)).currency, "EUR");
        let set = Config {
            online: false,
            weather_city: " London ".into(),
            currency: "gbp".into(),
            temperature_unit: "c".into(),
            search_engine: "https://www.google.com/search?q={arg}".into(),
            ..Config::default()
        };
        let c = answers_config(&set, &Region(Some("USD"), true));
        assert!(!c.online && !c.fahrenheit);
        assert_eq!((c.weather_city.as_str(), c.currency.as_str()), ("London", "GBP"));
        assert_eq!(c.search_engine, "https://www.google.com/search?q={arg}");
        let f = Config { temperature_unit: "f".into(), ..Config::default() };
        assert!(answers_config(&f, &Region(None, false)).fahrenheit);
    }

    #[test]
    fn engine_with_all_real_providers_answers_bluetooth() {
        let r = rig(vec![], vec![]);
        let results = r.engine.query("bluetooth", None);
        assert_eq!(results[0].provider, "settings");
        assert!(r.power.lock().unwrap().is_empty());
    }

    #[test]
    fn sleep_ranks_the_power_action_first() {
        let r = rig(vec![], vec![]);
        let res = r.engine.query("sleep", None);
        assert_eq!((res[0].provider, res[0].title.as_str()), ("power", "Sleep"));
        assert!(r.power.lock().unwrap().is_empty());
    }

    #[test]
    fn help_lists_all_providers_in_order() {
        let r = rig(vec![], vec![]);
        let help = r.engine.query("help", None);
        assert!(help.iter().all(|i| i.provider == engine::HELP));
        assert_eq!(
            titles(&help),
            [
                "12*7",
                "5 km in miles",
                "20 usd in eur",
                "time tokyo",
                "in 45 days",
                "weather london",
                "blender",
                "report.pdf",
                "your shortcuts",
                "> htop -d 10",
                "bluetooth",
                "lock / sleep / restart / shut down / sign out",
                "flick config",
                "clear history",
                "help",
                "Esc",
                "Ctrl+P",
                "Up",
            ]
        );
        let flick = &help[12];
        assert_eq!(flick.subtitle, "open the config file to change the hotkey, file folders and shortcuts");
        assert_eq!(flick.id, "flick config");
    }

    #[test]
    fn help_word_ignores_case() {
        let r = rig(vec![], vec![]);
        assert_eq!(r.engine.query("Help", None).len(), 18);
        let filtered = r.engine.query("HELP fi", None);
        assert!(!filtered.is_empty());
        assert!(filtered.iter().all(|i| i.provider == engine::HELP));
        assert!(titles(&filtered).contains(&"report.pdf".to_string()), "{:?}", titles(&filtered));
    }

    #[test]
    fn greater_than_routes_to_wsl_only() {
        let r = rig(vec![app("Htop")], vec![file(r"C:\d\htop notes.txt")]);
        let res = r.engine.query("> htop -d 10", None);
        assert_eq!(res.len(), 1);
        assert_eq!((res[0].provider, res[0].title.as_str()), ("wsl", "> htop -d 10"));
        assert_eq!(res[0].subtitle, "run in Debian");
        // Without `>` the WSL provider is never asked.
        let plain = r.engine.query("htop", None);
        assert!(!plain.is_empty());
        assert!(plain.iter().all(|i| i.provider != "wsl"));
        // Typed commands are never learned.
        assert_eq!(r.engine.activate("wsl", "htop -d 10", provider::Modifier::None, "", None), Ok(Outcome::Hide));
        assert_eq!(
            r.launcher.calls(),
            ["run_command Wsl(Debian) env=[] keep=true elevated=false cwd=-: htop -d 10"]
        );
        assert!(!r.dir.path().join("picks.json").exists());
    }

    #[test]
    fn shortcut_beats_app_with_same_name() {
        let r = rig(vec![app("hello")], vec![]);
        r.providers.shortcuts.set(vec![shortcut("hello", "notepad")], "Debian", "");
        let res = r.engine.query("hello", None);
        assert_eq!((res[0].provider, res[0].title.as_str()), ("shortcuts", "hello"));
        assert_eq!((res[1].provider, res[1].title.as_str()), ("apps", "hello"));
    }

    #[test]
    fn file_ranks_below_app_and_setting() {
        let r = rig(
            vec![app("Blender")],
            vec![file(r"C:\d\Blender.txt"), file(r"C:\d\Bluetooth.txt")],
        );
        let res = r.engine.query("blender", None);
        assert_eq!((res[0].provider, res[0].title.as_str()), ("apps", "Blender"));
        assert_eq!((res[1].provider, res[1].title.as_str()), ("files", "Blender.txt"));
        let res = r.engine.query("bluetooth", None);
        assert_eq!(res[0].provider, "settings");
        let file_at = res.iter().position(|i| i.provider == "files").unwrap();
        assert!(res[..file_at].iter().all(|i| i.provider == "settings"), "{:?}", titles(&res));
    }

    #[test]
    fn calc_finds_the_app_by_alias_above_at_most_five_files() {
        let mut calculatrice = app("Rekenmachine");
        calculatrice.aliases = vec!["calculator".into()];
        // A weak app match (alias substring, 450) that scores below every file.
        let mut weak = app("Math Tool");
        weak.aliases = vec!["scicalc".into()];
        let files = (0..8)
            .map(|i| FileEntry::new(PathBuf::from(format!(r"C:\d\sdk{i}\Calculator")), true).at_depth(5))
            .chain((0..4).map(|i| file(&format!(r"C:\d\calculator {i}.py"))))
            .collect();
        let r = rig(vec![calculatrice, weak], files);
        let res = r.engine.query("calc", None);
        assert_eq!(res.len(), 8);
        assert_eq!((res[0].provider, res[0].title.as_str()), ("apps", "Rekenmachine"), "{:?}", titles(&res));
        assert!(titles(&res).contains(&"Math Tool".to_string()), "{:?}", titles(&res));
        // Files hold at most five rows above the last other result; only rows
        // left over after every other result go to further files.
        let last_other = res.iter().rposition(|i| i.provider != "files").unwrap();
        assert!(res[..last_other].iter().filter(|i| i.provider == "files").count() <= 5, "{:?}", titles(&res));
        assert!(res.iter().all(|i| i.provider != "files" || i.score < 650));
    }

    #[test]
    fn flick_config_is_findable() {
        let r = rig(vec![], vec![]);
        for text in ["flick config", "edit config", "flick", "config"] {
            let res = r.engine.query(text, None);
            assert_eq!((res[0].provider, res[0].title.as_str()), ("flick", "Flick: edit config"), "{text}");
        }
        let id = r.engine.query("flick config", None)[0].id.clone();
        assert_eq!(r.engine.activate("flick", &id, provider::Modifier::None, "", None), Ok(Outcome::Hide));
        assert_eq!(r.opened.load(Ordering::SeqCst), 1);
        assert!(r.launcher.calls().is_empty());
    }

    #[test]
    fn push_config_updates_every_provider() {
        let r = rig(vec![], vec![]);
        let old = Config::default();
        let new = Config {
            wsl_distro: "Ubuntu".into(),
            shortcuts: vec![shortcut("hello", "notepad")],
            ..Config::default()
        };
        assert!(!push_config(&r.providers, &old, &new, &WslState::Unknown), "same roots: no rescan");
        assert_eq!(r.engine.query("hello", None)[0].provider, "shortcuts");
        assert_eq!(r.engine.query("> ls", None)[0].subtitle, "run in Ubuntu");
        let moved = Config { file_roots: vec![PathBuf::from(r"C:\definitely\missing")], ..new.clone() };
        assert!(push_config(&r.providers, &new, &moved, &WslState::Unknown), "roots changed: rescan");
        assert!(r.launcher.calls().is_empty());
    }

    #[test]
    fn config_distro_wins_over_detected() {
        let ubuntu = WslState::Distro("Ubuntu".into());
        assert_eq!(effective_distro("Debian", &ubuntu), "Debian");
        assert_eq!(effective_distro("", &ubuntu), "Ubuntu");
        assert_eq!(effective_distro("", &WslState::Unknown), "");
    }

    #[test]
    fn no_wsl_beats_a_configured_distro_and_unknown_does_not() {
        assert_eq!(effective_distro("Debian", &WslState::NoWsl), "");
        assert_eq!(effective_distro("", &WslState::NoWsl), "");
        assert_eq!(effective_distro("Debian", &WslState::Unknown), "Debian");
    }

    #[test]
    fn detection_maps_to_state_and_failure_keeps_the_old_one() {
        use wsl_detect::Detection::{Failed, Found};
        assert_eq!(WslState::from_detection(Found(Some("Ubuntu".into()))), Some(WslState::Distro("Ubuntu".into())));
        assert_eq!(WslState::from_detection(Found(None)), Some(WslState::NoWsl));
        assert_eq!(WslState::from_detection(Failed), None);
    }

    #[test]
    fn push_config_uses_the_detected_distro_when_none_is_configured() {
        let r = rig(vec![], vec![]);
        let old = Config::default();
        assert!(!push_config(&r.providers, &old, &old, &WslState::Unknown));
        assert!(r.engine.query("> ls", None).is_empty(), "no WSL: nothing offered");
        assert!(!push_config(&r.providers, &old, &old, &WslState::Distro("Ubuntu".into())));
        assert_eq!(r.engine.query("> ls", None)[0].subtitle, "run in Ubuntu");
        let set = Config { wsl_distro: "Debian".into(), ..Config::default() };
        push_config(&r.providers, &old, &set, &WslState::Distro("Ubuntu".into()));
        assert_eq!(r.engine.query("> ls", None)[0].subtitle, "run in Debian");
    }

    #[test]
    fn push_distro_applies_the_effective_distro_to_wsl_and_shortcuts() {
        let r = rig(vec![], vec![]);
        let config = Config { shortcuts: vec![shortcut("hello", "notepad")], ..Config::default() };
        push_distro(&r.providers, &config, &WslState::Distro("Ubuntu".into()));
        assert_eq!(r.engine.query("> ls", None)[0].subtitle, "run in Ubuntu");
        assert_eq!(r.engine.query("hello", None)[0].provider, "shortcuts");
        push_distro(&r.providers, &config, &WslState::NoWsl);
        assert!(r.engine.query("> ls", None).is_empty());
    }

    #[test]
    fn tooltip_prefixes_errors_with_flick() {
        assert_eq!(tooltip(None), "Flick");
        assert_eq!(
            tooltip(Some("hotkey \"X\" is not a valid shortcut")),
            "Flick \u{2014} hotkey \"X\" is not a valid shortcut"
        );
    }

    #[test]
    fn picked_power_action_stays_below_equal_tier_app() {
        let r = rig(vec![app("Sleep Timer")], vec![]);
        for _ in 0..20 {
            r.engine.activate("power", "sleep", provider::Modifier::None, "", None).unwrap();
        }
        assert_eq!(r.power.lock().unwrap().len(), 20);
        let titles = titles(&r.engine.query("sleep", None));
        let app = titles.iter().position(|t| t == "Sleep Timer").unwrap();
        let power = titles.iter().position(|t| t == "Sleep").unwrap();
        assert!(app < power, "{titles:?}");
        assert!(!r.dir.path().join("picks.json").exists());
    }

    #[test]
    fn join_errors_keeps_both() {
        assert_eq!(join_errors(None, None), None);
        assert_eq!(join_errors(Some("a".into()), None), Some("a".into()));
        assert_eq!(join_errors(None, Some("b".into())), Some("b".into()));
        assert_eq!(join_errors(Some("a".into()), Some("b".into())), Some("a; b".into()));
    }

    #[test]
    fn hotkey_reapplied_only_when_changed_or_failed() {
        assert!(!hotkey_needs_apply(Some("Alt+Space"), "Alt+Space"));
        assert!(hotkey_needs_apply(Some("Alt+Space"), "Ctrl+Space"));
        assert!(hotkey_needs_apply(None, "Alt+Space"));
    }
}
