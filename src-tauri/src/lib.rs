mod after_hide;
mod answers;
mod bookmarks;
mod clipboard;
mod cliphistory;
mod chores;
mod commands;
mod config;
mod config_edit;
mod controls;
mod devices;
mod dirs;
mod direct_keys;
mod engine;
mod fuzzy;
mod games;
mod history;
mod hotkey;
mod icon;
mod item_search;
mod launch;
mod net;
mod notifications;
mod paste;
mod pins;
mod picker;
mod provider;
mod process;
mod providers;
mod quote;
mod ranking;
mod refresh;
mod reminders;
mod settings;
mod settings_window;
mod status;
mod updates;
mod tray;
mod uninstall;
mod window;
mod winlist;
mod wsl_detect;

use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;

use answers::number::{Formats, WindowsFormats};
use bookmarks::{BookmarkSource, Firefox};
use chores::{Chores, SystemChores};
use devices::{BtAudio, KsBtAudio, WlanWifi, Wifi};
use direct_keys::{DirectKeys, PluginKeys, PressGate, PressTarget, REPEAT_GATE};
use games::{GameSource, SystemGames};
use notifications::{Notifier, SystemNotifier, TITLE};
use provider::{Outcome, ResultItem, Then};
use providers::bookmarks::BookmarksProvider;
use providers::chores::ChoresProvider;
use providers::devices::DevicesProvider;
use providers::games::GamesProvider;
use providers::timers::TimersProvider;
use providers::words::WordsProvider;
use providers::workspaces::{ItemRunner, Report, WorkspacesProvider};
use reminders::{Fire, Reminders};
use answers::rates::RatesStore;
use answers::weather::WeatherStore;
use after_hide::FollowUps;
use cliphistory::{ClipHistory, WinRtHistory};
use clipboard::{ClipboardText, WindowsClipboard};
use commands::AfterHide;
use config::Config;
use engine::Engine;
use launch::{Launcher, SystemLauncher};
use provider::Provider;
use providers::answers::{AnswersConfig, AnswersProvider, DUCKDUCKGO};
use providers::apps::AppsProvider;
use providers::clips::ClipsProvider;
use providers::files::FilesProvider;
use providers::flick::{FlickProvider, Opener};
use providers::path::PathProvider;
use providers::recent::{DownloadsProvider, RecentProvider};
use dirs::{DirEntry, FileSystem, Loader, SystemFolders, KnownFolders, Links, ShellLinks, SystemFs};
use providers::history::HistoryProvider;
use providers::power::{PowerProvider, PowerRunner, SystemPower};
use providers::qr::QrProvider;
use providers::screen::{ColorProvider, SnipProvider};
use providers::settings::SettingsProvider;
use providers::status::StatusProvider;
use providers::system::SystemProvider;
use providers::windows::WindowsProvider;
use status::{StatusStore, SystemStatus};
use winlist::{SystemWindows, WindowList};
use controls::SystemControls;
use providers::shortcuts::ShortcutsProvider;
use providers::update::UpdateProvider;
use providers::welcome::WelcomeProvider;
use updates::{PluginSource, Updates};
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
    /// Open windows: refreshed whenever the bar is shown.
    windows: Arc<WindowList>,
    /// Re-reads the open windows and the audio state, one run at a time.
    refresher: Arc<refresh::Refresher>,
    /// Volume, media, theme and radio commands; caches the audio state.
    system: Arc<SystemProvider>,
    /// Battery, CPU, memory, disk and IP answers.
    status: Arc<StatusProvider>,
    files: Arc<FilesProvider>,
    path: Arc<PathProvider>,
    downloads: Arc<DownloadsProvider>,
    recent: Arc<RecentProvider>,
    /// `v`: Windows' clipboard history, held until the bar hides.
    clips: Arc<ClipsProvider>,
    /// `qr`: the clipboard text, held until the bar hides.
    qr: Arc<QrProvider>,
    shortcuts: Arc<ShortcutsProvider>,
    wsl: Arc<WslProvider>,
    welcome: Arc<WelcomeProvider>,
    update: Arc<UpdateProvider>,
    /// Installed games: scanned with the apps.
    games: Arc<GamesProvider>,
    /// Firefox bookmarks: read at start, then when the bar shows and they are stale.
    bookmarks: Arc<BookmarksProvider>,
    workspaces: Arc<WorkspacesProvider>,
    /// The one strong handle that keeps the reminder thread alive.
    reminders: Arc<Reminders>,
    timers: Arc<TimersProvider>,
    /// `tr` and `def`.
    words: Arc<WordsProvider>,
    /// `connect`, `disconnect` and `wifi`.
    devices: Arc<DevicesProvider>,
    /// Recycle Bin, eject, DNS cache and Explorer.
    chores: Arc<ChoresProvider>,
}

impl Providers {
    /// Cheap: no disk access. File roots and shortcuts arrive with the config.
    #[allow(clippy::too_many_arguments)]
    fn new(
        answers: Answers,
        control: Control,
        browse: Browse,
        capture: Capture,
        extras: Extras,
        apps: Arc<AppsProvider>,
        launcher: Arc<dyn Launcher>,
        distro: &str,
        open_settings: Opener,
        updates: Arc<Updates>,
    ) -> Self {
        Self {
            answers: answers.provider,
            weather: answers.weather,
            rates: answers.rates,
            formats: answers.formats,
            apps,
            refresher: control.refresher,
            windows: control.windows,
            system: control.system,
            status: control.status,
            files: FilesProvider::new(launcher.clone(), Arc::new(WindowsClipboard)),
            path: browse.path,
            downloads: browse.downloads,
            recent: browse.recent,
            clips: capture.clips,
            qr: capture.qr,
            shortcuts: ShortcutsProvider::new(launcher.clone(), distro),
            wsl: WslProvider::new(launcher, distro),
            welcome: WelcomeProvider::new(Box::new(window::shows), open_settings),
            update: UpdateProvider::new(updates),
            games: extras.games,
            bookmarks: extras.bookmarks,
            workspaces: extras.workspaces,
            reminders: extras.reminders,
            timers: extras.timers,
            words: extras.words,
            devices: extras.devices,
            chores: extras.chores,
        }
    }

    /// Reads the open windows and the audio state, and the bookmarks when they
    /// are stale. All can block, so this runs on other threads, never the caller's.
    pub(crate) fn refresh_on_show(&self) {
        self.refresher.request();
        self.bookmarks.refresh_if_stale();
    }

    /// The bar hid: drop the clipboard history and clipboard text read for it,
    /// and the `tr`/`def` text still waiting for the typing pause.
    pub(crate) fn on_hide(&self) {
        self.clips.forget();
        self.qr.forget();
        self.words.forget_waiting();
    }
}

/// Games, bookmarks, workspaces, timers and reminders, `tr`/`def`, devices and chores.
struct Extras {
    games: Arc<GamesProvider>,
    bookmarks: Arc<BookmarksProvider>,
    workspaces: Arc<WorkspacesProvider>,
    reminders: Arc<Reminders>,
    timers: Arc<TimersProvider>,
    words: Arc<WordsProvider>,
    devices: Arc<DevicesProvider>,
    chores: Arc<ChoresProvider>,
}

/// What the extras read and change on this PC (tests use fakes).
struct ExtraSources {
    games: Arc<dyn GameSource>,
    bookmarks: Arc<dyn BookmarkSource>,
    clipboard: Arc<dyn clipboard::Clipboard>,
    fetch: Arc<dyn net::Fetch>,
    bt: Arc<dyn BtAudio>,
    wifi: Arc<dyn Wifi>,
    chores: Arc<dyn Chores>,
    /// `reminders.json`.
    reminders: PathBuf,
    now: dirs::Clock,
    tz: jiff::tz::TimeZone,
}

impl Extras {
    /// Cheap, no I/O: games and bookmarks are read by background threads, the
    /// reminders file by the reminder thread (`Reminders::start`).
    /// `fire` shows a reminder's notification; `report` tells how a workspace run went.
    fn new(
        launcher: Arc<dyn Launcher>,
        on_ready: Arc<dyn Fn() + Send + Sync>,
        open_settings: Opener,
        fire: Fire,
        report: Report,
    ) -> Self {
        let sources = ExtraSources {
            games: Arc::new(SystemGames),
            bookmarks: Arc::new(Firefox),
            clipboard: Arc::new(WindowsClipboard),
            fetch: Arc::new(net::WinHttp),
            bt: Arc::new(KsBtAudio),
            wifi: Arc::new(WlanWifi),
            chores: Arc::new(SystemChores),
            reminders: reminders_path(),
            now: Arc::new(std::time::SystemTime::now),
            tz: jiff::tz::TimeZone::system(),
        };
        Self::over(launcher, on_ready, open_settings, fire, report, sources)
    }

    /// The providers over any sources (tests use fakes).
    fn over(
        launcher: Arc<dyn Launcher>,
        on_ready: Arc<dyn Fn() + Send + Sync>,
        open_settings: Opener,
        fire: Fire,
        report: Report,
        s: ExtraSources,
    ) -> Self {
        let reminders = Reminders::new(s.reminders, s.now.clone(), fire);
        Extras {
            games: GamesProvider::new(s.games, launcher.clone()),
            bookmarks: BookmarksProvider::new(s.bookmarks, launcher, s.clipboard.clone()),
            workspaces: WorkspacesProvider::new(report),
            timers: Arc::new(TimersProvider::new(reminders.clone(), s.now, s.tz, on_ready.clone())),
            reminders,
            words: Arc::new(WordsProvider::new(s.fetch, s.clipboard, open_settings, on_ready.clone())),
            devices: Arc::new(DevicesProvider::new(s.bt, s.wifi, on_ready.clone())),
            chores: Arc::new(ChoresProvider::new(s.chores, on_ready)),
        }
    }
}

/// The job behind `refresh_on_show`; `on_done` runs after each completed run.
fn refresh_windows_and_audio(
    windows: Arc<WindowList>,
    system: Arc<SystemProvider>,
    on_done: Arc<dyn Fn() + Send + Sync>,
) -> Arc<refresh::Refresher> {
    refresh::Refresher::new(
        Box::new(move || {
            // The window list reads each window's AppUserModelID through COM.
            launch::with_com(|| windows.refresh());
            system.refresh();
        }),
        Box::new(move || on_done()),
    )
}

/// The providers that steer running windows and the PC.
struct Control {
    refresher: Arc<refresh::Refresher>,
    windows: Arc<WindowList>,
    system: Arc<SystemProvider>,
    status: Arc<StatusProvider>,
}

impl Control {
    /// Cheap: nothing is read until the bar is shown or a status is asked for.
    /// `on_ready` runs when a status sample has finished.
    fn new(launcher: Arc<dyn Launcher>, on_ready: Arc<dyn Fn() + Send + Sync>) -> Self {
        let store = StatusStore::new(
            Arc::new(SystemStatus),
            Arc::new(net::WinHttp),
            on_ready.clone(),
            Duration::from_millis(500),
        );
        let windows = WindowList::new(Arc::new(SystemWindows::new()));
        let system = Arc::new(SystemProvider::new(Arc::new(SystemControls), launcher));
        // Done: the bar re-runs its query, so a row typed for the old snapshot is rebuilt.
        let refresher = refresh_windows_and_audio(windows.clone(), system.clone(), on_ready.clone());
        Control {
            refresher,
            windows,
            system,
            status: Arc::new(StatusProvider::new(store, Arc::new(WindowsClipboard))),
        }
    }
}

/// Folder browsing: a typed path, `dl` and `recent`.
struct Browse {
    path: Arc<PathProvider>,
    downloads: Arc<DownloadsProvider>,
    recent: Arc<RecentProvider>,
}

impl Browse {
    /// Cheap: nothing is read until a path, `dl` or `recent` is typed. Reads
    /// run on loader threads; `on_ready` runs when one has finished.
    fn new(launcher: Arc<dyn Launcher>, on_ready: Arc<dyn Fn() + Send + Sync>) -> Self {
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
        let folders = SystemFolders;
        Self::over(
            launcher,
            on_ready,
            Arc::new(SystemFs),
            Arc::new(ShellLinks),
            home,
            folders.downloads(),
            folders.recent(),
            Arc::new(std::time::SystemTime::now),
        )
    }

    /// The providers over any file system, links, folders and clock (tests use fakes).
    #[allow(clippy::too_many_arguments)]
    fn over(
        launcher: Arc<dyn Launcher>,
        on_ready: Arc<dyn Fn() + Send + Sync>,
        fs: Arc<dyn FileSystem>,
        links: Arc<dyn Links>,
        home: Option<PathBuf>,
        downloads: Option<PathBuf>,
        recent: Option<PathBuf>,
        now: dirs::Clock,
    ) -> Self {
        // One loader for path browsing and `dl`: they read the same kind of listing.
        let dirs: Arc<Loader<Vec<DirEntry>>> = Loader::new(on_ready.clone(), dirs::FRESH, dirs::SLOW);
        let store = Loader::new(on_ready, dirs::FRESH, dirs::SLOW);
        let clipboard: Arc<dyn clipboard::Clipboard> = Arc::new(WindowsClipboard);
        Browse {
            path: Arc::new(PathProvider::new(dirs.clone(), fs.clone(), home, launcher.clone(), clipboard.clone())),
            downloads: Arc::new(DownloadsProvider::new(
                dirs,
                fs.clone(),
                downloads,
                launcher.clone(),
                clipboard.clone(),
                now.clone(),
            )),
            recent: Arc::new(RecentProvider::new(store, fs, links, recent, launcher, clipboard, now)),
        }
    }
}

/// The clipboard rows: `v` and `qr`.
struct Capture {
    clips: Arc<ClipsProvider>,
    qr: Arc<QrProvider>,
}

impl Capture {
    /// Cheap: nothing is read until `v` or `qr` is typed. Reads run on loader
    /// threads; `on_ready` runs when one has finished.
    fn new(launcher: Arc<dyn Launcher>, on_ready: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::over(
            Arc::new(WinRtHistory::new()),
            Arc::new(WindowsClipboard),
            Arc::new(WindowsClipboard),
            launcher,
            Arc::new(std::time::SystemTime::now),
            on_ready,
            Arc::new(paste::clipboard_sequence),
        )
    }

    /// The providers over any history, clipboard and clock (tests use fakes).
    fn over(
        history: Arc<dyn ClipHistory>,
        text: Arc<dyn ClipboardText>,
        clipboard: Arc<dyn clipboard::Clipboard>,
        launcher: Arc<dyn Launcher>,
        now: dirs::Clock,
        on_ready: Arc<dyn Fn() + Send + Sync>,
        sequence: paste::Sequence,
    ) -> Self {
        Capture {
            clips: Arc::new(ClipsProvider::new(history, launcher, now, on_ready.clone(), sequence)),
            qr: Arc::new(QrProvider::new(text, clipboard, on_ready)),
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
    pub(crate) history: Arc<History>,
    providers: Providers,
    /// What the last update check found; in memory only.
    pub(crate) updates: Arc<Updates>,
    /// Last good config. Held for the whole of a reload, which serialises
    /// reloads and every push of config into the providers.
    pub(crate) config: Mutex<Config>,
    /// Result of the last successful WSL detection; `Unknown` until one
    /// succeeds. Lock order: `config`, then this.
    pub(crate) detected: Mutex<WslState>,
    /// Configured accelerator last applied successfully; `None` when the last
    /// `hotkey::apply` failed (the previous hotkey may then still be active).
    pub(crate) hotkey: Mutex<Option<String>>,
    /// The accelerator registered as the bar hotkey now: kept when an apply
    /// fails, since `hotkey::apply` registers the previous one again. `None`
    /// until one has registered. The direct hotkeys are checked against it.
    bar_hotkey: Mutex<Option<String>>,
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
    open_settings: Opener,
    ranking: Arc<Ranking>,
    history: Arc<History>,
    pins: Arc<Pins>,
) -> Engine {
    let providers: Vec<Box<dyn Provider>> = vec![
        // First, so its row tops the empty bar. It has no help line.
        Box::new(p.update.clone()),
        Box::new(p.answers.clone()),
        Box::new(p.apps.clone()),
        Box::new(WindowsProvider::new(p.windows.clone())),
        Box::new(p.files.clone()),
        Box::new(p.path.clone()),
        Box::new(p.downloads.clone()),
        Box::new(p.recent.clone()),
        Box::new(p.clips.clone()),
        Box::new(p.qr.clone()),
        Box::new(SnipProvider),
        Box::new(ColorProvider),
        Box::new(p.games.clone()),
        Box::new(p.bookmarks.clone()),
        Box::new(p.workspaces.clone()),
        Box::new(p.shortcuts.clone()),
        Box::new(p.wsl.clone()),
        Box::new(SettingsProvider),
        Box::new(p.system.clone()),
        Box::new(p.devices.clone()),
        Box::new(p.status.clone()),
        Box::new(p.chores.clone()),
        Box::new(p.timers.clone()),
        Box::new(p.words.clone()),
        Box::new(PowerProvider::new(power)),
        Box::new(FlickProvider::new(open_config, open_settings)),
        Box::new(HistoryProvider::new(history.clone())),
        // Last: it has no help line, so the help list is unchanged.
        Box::new(p.welcome.clone()),
    ];
    Engine::new(providers, ranking, history, pins)
}

/// Opens the Settings window; builds it off the calling thread.
fn settings_opener(app: &AppHandle) -> Opener {
    let app = app.clone();
    Box::new(move || {
        settings_window::open_soon(&app);
        Ok(())
    })
}

/// What detection has said about WSL so far.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum WslState {
    /// No detection has succeeded yet (not run, or it timed out).
    Unknown,
    /// Detection found no usable WSL.
    NoWsl,
    /// Detection found these distros; `default` is the one to use.
    Present { default: String, distros: Vec<String> },
}

impl WslState {
    /// `None` for a failed detection: the caller keeps its previous state.
    fn from_detection(detection: wsl_detect::Detection) -> Option<WslState> {
        match detection {
            wsl_detect::Detection::Found(Some(f)) => Some(WslState::Present { default: f.default, distros: f.distros }),
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
        WslState::Present { default, .. } => default.clone(),
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
    p.status.set_online(new.online);
    p.words.set_online(new.online);
    p.workspaces.set(new.workspaces.clone());
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

fn reminders_path() -> PathBuf {
    config::config_path().with_file_name("reminders.json")
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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::query,
            commands::activate,
            commands::hide,
            commands::toggle_pin,
            commands::past_searches,
            commands::act,
            settings::settings_state,
            settings::settings_set,
            settings::settings_save_shortcut,
            settings::settings_check_shortcut,
            settings::settings_delete_shortcut,
            settings::settings_set_start_at_login,
            settings::settings_clear_history,
            settings::settings_pick_folder,
            settings::settings_pins,
            settings::settings_move_pin,
            settings::settings_check_city,
            settings::settings_open_file,
            settings::settings_currencies,
            settings::settings_pause_hotkey,
            settings::settings_save_workspace,
            settings::settings_check_workspace,
            settings::settings_delete_workspace,
            settings::settings_save_hotkey,
            settings::settings_delete_hotkey,
            item_search::settings_search_items,
            item_search::settings_items_found,
            direct_keys::settings_hotkey_status,
            updates::settings_update_status,
            updates::settings_check_now,
            updates::settings_install_update
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
        .build(tauri::generate_context!())
        .expect("error while running Flick")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                on_exit(app);
            }
        });
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
    // The Settings item picker asks again too (a device list read for `connect` has ended).
    let on_ready: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = ready.emit_to(window::LABEL, "answers:ready", ());
        let _ = ready.emit_to(settings::WINDOW, "answers:ready", ());
    });
    let answers = Answers::new(launcher.clone(), on_ready.clone());
    let browse = Browse::new(launcher.clone(), on_ready.clone());
    let capture = Capture::new(launcher.clone(), on_ready.clone());
    // Reminders that ring and direct hotkeys that fail say so in a notification.
    let notifier: Arc<dyn Notifier> = Arc::new(SystemNotifier);
    app.manage(notifier.clone());
    let fire: Fire = Arc::new(move |body: &str| {
        let _ = notifier.show(TITLE, body);
    });
    // A workspace whose items did not all open brings the bar back with its name and the count.
    let reopen = app.clone();
    let report: Report = Arc::new(move |name: &str, message: &str| window::reopen(&reopen, name, message));
    let extras = Extras::new(launcher.clone(), on_ready.clone(), settings_opener(app), fire, report);
    let control = Control::new(launcher.clone(), on_ready);
    // Paste, Win+V, the snip and the colour picker, run once the bar has hidden.
    app.manage::<Arc<dyn AfterHide>>(Arc::new(FollowUps {
        desktop: Arc::new(paste::SystemDesktop::new()),
        launcher: launcher.clone(),
        previous: paste::previous,
        pick_color: Box::new(|| picker::start(Arc::new(picker::GdiScreen), Arc::new(WindowsClipboard))),
        settle: after_hide::settle,
        shows: window::shows,
    }));
    let updates = Arc::new(Updates::new(Arc::new(PluginSource::new(app.clone())), updates::on_change(app.clone())));
    updates.set_enabled(loaded.config.check_updates);
    let providers = Providers::new(answers, control, browse, capture, extras, AppsProvider::new(launcher.clone()), launcher,
        &effective_distro(&loaded.config.wsl_distro, &WslState::Unknown),
        settings_opener(app),
        updates.clone(),
    );
    providers.apps.set_windows(providers.windows.clone());
    providers.answers.set_config(&answers_config(&loaded.config, providers.formats.as_ref()));
    providers.status.set_online(loaded.config.online);
    providers.words.set_online(loaded.config.online);
    providers.workspaces.set(loaded.config.workspaces.clone());
    // Cheap (no watcher yet, no scan): the first file scan uses these roots.
    providers.files.set_roots(loaded.config.file_roots.clone());
    let ranking = Arc::new(Ranking::load(picks_path()));
    let history = Arc::new(History::load(history_path(), loaded.config.history));
    let pins = Arc::new(Pins::load(pins_path()));
    let engine = Arc::new(build_engine(
        &providers,
        Box::new(SystemPower),
        Box::new(launch::open_config),
        settings_opener(app),
        ranking,
        history.clone(),
        pins,
    ));
    let runner: std::sync::Weak<dyn ItemRunner> = Arc::<Engine>::downgrade(&engine);
    providers.workspaces.set_runner(runner);
    app.manage(AppState {
        engine,
        history,
        providers: providers.clone(),
        updates,
        config: Mutex::new(loaded.config.clone()),
        detected: Mutex::new(WslState::Unknown),
        hotkey: Mutex::new(None),
        bar_hotkey: Mutex::new(None),
        watcher: Mutex::new(None),
        config_error: Mutex::new(None),
    });
    // Once: reads reminders.json, rings what was missed while Flick was not
    // running, then each one when it falls due. `AppState` keeps it alive.
    providers.reminders.start();

    start_index_threads(app, providers);
    updates::start_scheduler(app);

    window::install_handlers(app);

    let keys = Arc::new(DirectKeys::new(Arc::new(PluginKeys::new(app.clone(), on_direct_press))));
    app.manage(keys.clone());
    let hotkey_error = apply_hotkey(app, &loaded.config.hotkey);
    // After the bar hotkey, against the one that registered: it always wins.
    keys.apply(&registered_bar(app, &loaded.config.hotkey), &loaded.config.direct_hotkeys);
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
            p.games.refresh();
            p.bookmarks.refresh_now();
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
/// - apps, then games: now, then every 10 minutes;
/// - bookmarks: now (then whenever the bar shows and they are 5 minutes old);
/// - shortcuts: the config's definitions now, pick folders every 5 minutes;
/// - files: the folder watcher, a scan now, then a rescan every 30 minutes.
fn start_index_threads(app: &AppHandle, p: Providers) {
    load_answer_stores(app, &p);

    let (apps, games) = (p.apps, p.games);
    std::thread::spawn(move || loop {
        apps.refresh();
        games.refresh();
        std::thread::sleep(APPS_REFRESH_INTERVAL);
    });

    // Its own thread (Firefox's database can be slow to copy), marked as
    // running so a show meanwhile does not start a second read.
    p.bookmarks.refresh_if_stale();

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
pub(crate) fn apply_hotkey(app: &AppHandle, accelerator: &str) -> Option<String> {
    let result = hotkey::apply(app, accelerator);
    let state = app.state::<AppState>();
    *state.hotkey.lock().unwrap_or_else(|e| e.into_inner()) = result.is_ok().then(|| accelerator.to_string());
    if result.is_ok() {
        *state.bar_hotkey.lock().unwrap_or_else(|e| e.into_inner()) = Some(accelerator.to_string());
    }
    let error = result.err();
    tray::set_tooltip(app, &tooltip(error.as_deref()));
    error
}

/// The bar hotkey registered now, or `configured` when none has registered
/// yet (that combo is then kept from the direct keys too).
fn registered_bar(app: &AppHandle, configured: &str) -> String {
    let state = app.state::<AppState>();
    let registered = state.bar_hotkey.lock().unwrap_or_else(|e| e.into_inner()).clone();
    registered.unwrap_or_else(|| configured.to_string())
}

/// A direct hotkey was pressed. This may run on the main thread: only the
/// repeat gate and the lookup happen here; the item opens on its own thread.
fn on_direct_press(app: &AppHandle, keys: &str) {
    static GATE: LazyLock<PressGate> = LazyLock::new(|| PressGate::new(REPEAT_GATE));
    if !GATE.allow(keys, Instant::now()) {
        return;
    }
    let Some(item) = app.try_state::<Arc<DirectKeys>>().and_then(|k| k.item_for(keys)) else {
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || direct_keys::press(&AppPress { app }, &item));
}

/// What a direct hotkey acts on in the running app.
struct AppPress {
    app: AppHandle,
}

impl PressTarget for AppPress {
    fn item(&self, provider: &str, id: &str) -> Option<ResultItem> {
        self.app.state::<AppState>().engine.item(provider, id)
    }

    fn ready(&self, provider: &str) -> bool {
        self.app.state::<AppState>().engine.ready(provider)
    }

    fn open(&self, provider: &str, id: &str) -> Result<Outcome, String> {
        self.app.state::<AppState>().engine.open_item(provider, id)
    }

    fn show_armed(&self, item: ResultItem) {
        window::show_armed(&self.app, &item);
    }

    fn show_filled(&self, text: &str) {
        window::reopen(&self.app, text, "");
    }

    fn show_step(&self, prompt: &str, scope: &str) {
        window::show_step(&self.app, prompt, scope);
    }

    fn follow_up(&self, then: Then) {
        if let Some(follow) = self.app.try_state::<Arc<dyn AfterHide>>() {
            follow.run(then, window::shows());
        }
    }

    fn failed(&self, message: &str) {
        if let Some(notifier) = self.app.try_state::<Arc<dyn Notifier>>() {
            let _ = notifier.show(TITLE, message);
        }
    }
}

/// Flick is quitting, by Quit (`RunEvent::Exit`) or by the updater, which
/// starts the installer and ends the process without that event (it runs this
/// as its `on_before_exit`). Safe to run more than once.
pub(crate) fn on_exit(app: &AppHandle) {
    quit_cleanup(app.try_state::<AppState>().as_ref().map(|s| s.providers.reminders.as_ref()));
}

/// A reminder change whose save failed is written now (or it would ring again
/// as missed at the next start), and a notification balloon still showing
/// takes its icon out of the notification area. A second run finds nothing
/// left to do.
fn quit_cleanup(reminders: Option<&Reminders>) {
    if let Some(reminders) = reminders {
        reminders.flush();
    }
    notifications::remove_balloons();
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
pub(crate) fn reload_config(app: &AppHandle) {
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
    let old = std::mem::replace(&mut *config, loaded.config);
    if loaded.error.is_none() {
        state.updates.set_enabled(config.check_updates);
    }
    let applied = state.hotkey.lock().unwrap_or_else(|e| e.into_inner()).clone();
    // Under the config lock, so two reloads never apply hotkeys interleaved.
    let keys = app.state::<Arc<DirectKeys>>();
    let hotkey_error = reapply_hotkeys(
        &keys,
        &old,
        &config,
        hotkey_needs_apply(applied.as_deref(), &config.hotkey),
        |accelerator| apply_hotkey(app, accelerator),
        || registered_bar(app, &config.hotkey),
    );
    emit_config_error(app, join_errors(loaded.error, hotkey_error));
    // After the direct keys were applied, so the Hotkeys status read on this is fresh.
    let _ = app.emit_to(settings::WINDOW, "settings:changed", ());
}

/// `applied` is the configured hotkey that last registered successfully (`None`
/// after a failure). Re-apply when the config asks for something else, which
/// also retries a hotkey that failed before.
fn hotkey_needs_apply(applied: Option<&str>, wanted: &str) -> bool {
    applied != Some(wanted)
}

/// The direct hotkeys are applied again when the bar hotkey is (its combo
/// decides which of them may register) or when their definitions changed.
fn direct_keys_need_apply(old: &Config, new: &Config, bar_reapplied: bool) -> bool {
    bar_reapplied || old.direct_hotkeys != new.direct_hotkeys
}

/// The hotkeys after a config change, in the order that lets the bar win: the
/// direct keys let go first (the bar may move onto one of their combos), then
/// `apply_bar` registers the bar hotkey, then the direct keys register again
/// against `registered_bar`, the bar hotkey that is registered now. Returns the
/// bar hotkey's error. Never on the main thread.
fn reapply_hotkeys(
    keys: &DirectKeys,
    old: &Config,
    new: &Config,
    bar_needs_apply: bool,
    apply_bar: impl FnOnce(&str) -> Option<String>,
    registered_bar: impl FnOnce() -> String,
) -> Option<String> {
    if bar_needs_apply {
        return swap_bar_hotkey(keys, &new.hotkey, &new.direct_hotkeys, apply_bar, registered_bar);
    }
    if direct_keys_need_apply(old, new, false) {
        keys.release_all();
        keys.apply(&registered_bar(), &new.direct_hotkeys);
    }
    None
}

/// Moves the bar hotkey to `wanted` with the direct keys `defs` around it: they
/// let go first (the bar may move onto one of their combos), the bar registers,
/// then they register again against `registered_bar`, the bar hotkey that is
/// registered now (the old one when `wanted` failed). Used by a reload and by
/// the Settings window, so both let the bar win. Returns the bar hotkey's error.
/// Never on the main thread; callers hold the config lock.
pub(crate) fn swap_bar_hotkey(
    keys: &DirectKeys,
    wanted: &str,
    defs: &[config::DirectHotkeyDef],
    apply_bar: impl FnOnce(&str) -> Option<String>,
    registered_bar: impl FnOnce() -> String,
) -> Option<String> {
    keys.release_all();
    let error = apply_bar(wanted);
    keys.apply(&registered_bar(), defs);
    error
}

/// The Settings window's bar hotkey change, under the config lock (so a reload
/// never interleaves): the direct keys follow it as on a reload, and the reload
/// that follows finds the bar and the direct keys already in line.
pub(crate) fn change_bar_hotkey(app: &AppHandle, wanted: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let applied = state.hotkey.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let keys = app.state::<Arc<DirectKeys>>();
    settings::set_hotkey(&config::config_path(), applied.as_deref(), wanted, |h| {
        swap_bar_hotkey(&keys, h, &config.direct_hotkeys, |a| apply_hotkey(app, a), || registered_bar(app, &config.hotkey))
            .map_or(Ok(()), Err)
    })
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

    use config::{ItemRef, ShortcutDef};
    use direct_keys::KeyRegistry;
    use launch::testing::RecordingLauncher;
    use provider::Outcome;
    use providers::apps::{AppEntry, AppKind};
    use providers::files::FileEntry;
    use providers::power::PowerAction;
    use controls::{Audio, Controls, MediaKey, RadioKind};
    use status::{Battery, CpuTimes, Disk, Memory, StatusSource};
    use winlist::{WinInfo, WindowOps};

    /// Windows that exist only in memory; nothing is ever closed or focused.
    struct FakeWins(Vec<WinInfo>);

    impl WindowOps for FakeWins {
        fn list(&self) -> Vec<WinInfo> {
            self.0.clone()
        }
        fn exists(&self, _hwnd: isize) -> bool {
            true
        }
        fn focus(&self, _hwnd: isize) -> Result<(), String> {
            Ok(())
        }
        fn close(&self, _hwnd: isize, _pid: u32) -> Result<(), String> {
            Ok(())
        }
        fn kill(&self, _pid: u32, _exe: &str) -> Result<(), String> {
            Ok(())
        }
    }

    struct FakeControls;

    impl Controls for FakeControls {
        fn audio(&self) -> Result<Audio, String> {
            Ok(Audio { volume: 40, muted: false })
        }
        fn set_volume(&self, _pct: u8) -> Result<(), String> {
            Ok(())
        }
        fn set_muted(&self, _on: bool) -> Result<(), String> {
            Ok(())
        }
        fn media(&self, _key: MediaKey) -> Result<(), String> {
            Ok(())
        }
        fn set_dark(&self, _dark: bool) -> Result<(), String> {
            Ok(())
        }
        fn set_radio(&self, _kind: RadioKind, _on: bool) -> Result<(), String> {
            Ok(())
        }
    }

    struct FakeStatus;

    impl StatusSource for FakeStatus {
        fn battery(&self) -> Option<Battery> {
            Some(Battery { percent: 82, charging: false, minutes_left: None })
        }
        fn memory(&self) -> Memory {
            Memory { used: 1, total: 2 }
        }
        fn disks(&self) -> Vec<Disk> {
            vec![]
        }
        fn cpu_times(&self) -> CpuTimes {
            CpuTimes { idle: 0, kernel: 0, user: 0 }
        }
        fn local_ips(&self) -> Vec<String> {
            vec![]
        }
    }

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
        /// Shared, so the workspace runner can hold it weakly, as in `setup`.
        engine: Arc<Engine>,
        providers: Providers,
        launcher: Arc<RecordingLauncher>,
        power: Arc<Mutex<Vec<PowerAction>>>,
        opened: Arc<AtomicUsize>,
        dir: tempfile::TempDir,
        ranking: Arc<Ranking>,
        history: Arc<History>,
        /// Signalled when a status sample has finished.
        status_ready: std::sync::mpsc::Receiver<()>,
        /// Signalled when a folder, Downloads or Recent read has finished.
        browse_ready: std::sync::mpsc::Receiver<()>,
        links: Arc<dirs::testing::FakeLinks>,
        /// Windows' clipboard history as `v` reads it.
        clip_history: Arc<cliphistory::testing::FakeHistory>,
        /// The clipboard text as `qr` reads it.
        clip_text: Arc<clipboard::FakeText>,
        /// Signalled when a clipboard history or clipboard read has finished.
        capture_ready: std::sync::mpsc::Receiver<()>,
        /// Every URL asked for (weather, rates, status, `tr`, `def`).
        fetch: Arc<net::FakeFetch>,
        /// Signalled when a device, Recycle Bin, drive or lookup read has finished.
        extras_ready: std::sync::mpsc::Receiver<()>,
        /// What a workspace run reported: `(name, message)`.
        reports: std::sync::mpsc::Receiver<(String, String)>,
        bt: Arc<devices::testing::FakeBt>,
    }

    fn rig(apps: Vec<AppEntry>, files: Vec<FileEntry>) -> Rig {
        rig_with(apps, files, vec![], Default::default())
    }

    fn rig_with(apps: Vec<AppEntry>, files: Vec<FileEntry>, wins: Vec<WinInfo>, fs: Arc<dirs::testing::FakeFs>) -> Rig {
        rig_apps(|l| AppsProvider::with_launcher(l, apps), files, wins, fs)
    }

    fn rig_apps(
        make_apps: impl FnOnce(Arc<RecordingLauncher>) -> Arc<AppsProvider>,
        files: Vec<FileEntry>,
        wins: Vec<WinInfo>,
        fs: Arc<dirs::testing::FakeFs>,
    ) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let ranking = Arc::new(Ranking::load(dir.path().join("picks.json")));
        let launcher = Arc::new(RecordingLauncher::default());
        let fetch = Arc::new(net::FakeFetch::offline());
        let rates = RatesStore::new(dir.path().join("rates.json"), fetch.clone(), Arc::new(|| {}));
        let weather = WeatherStore::new(dir.path().join("weather.json"), fetch.clone(), Arc::new(|| {}));
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
        let windows = winlist::WindowList::new(Arc::new(FakeWins(wins)));
        windows.refresh();
        let (tx, status_ready) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let status = Arc::new(StatusProvider::new(
            StatusStore::new(
                Arc::new(FakeStatus),
                fetch.clone(),
                Arc::new(move || {
                    let _ = tx.lock().unwrap().send(());
                }),
                Duration::ZERO,
            ),
            Arc::new(clipboard::FakeClipboard::default()),
        ));
        let system = Arc::new(SystemProvider::new(Arc::new(FakeControls), launcher.clone()));
        let (browse_tx, browse_ready) = std::sync::mpsc::channel();
        let browse_tx = Mutex::new(browse_tx);
        let links = Arc::new(dirs::testing::FakeLinks::default());
        let browse = Browse::over(
            launcher.clone(),
            Arc::new(move || {
                let _ = browse_tx.lock().unwrap().send(());
            }),
            fs.clone(),
            links.clone(),
            Some(PathBuf::from(r"C:\Users\someone")),
            Some(PathBuf::from(r"C:\Users\someone\Downloads")),
            Some(PathBuf::from(r"C:\Users\someone\Recent")),
            Arc::new(|| std::time::UNIX_EPOCH + Duration::from_secs(1_000_000)),
        );
        let apps = make_apps(launcher.clone());
        apps.set_windows(windows.clone());
        let clip_history = Arc::new(cliphistory::testing::FakeHistory::new());
        let clip_text = Arc::new(clipboard::FakeText::new(Some("hello")));
        let (capture_tx, capture_ready) = std::sync::mpsc::channel();
        let capture_tx = Mutex::new(capture_tx);
        let capture = Capture::over(
            clip_history.clone(),
            clip_text.clone(),
            Arc::new(clipboard::FakeClipboard::default()),
            launcher.clone(),
            Arc::new(|| std::time::UNIX_EPOCH + Duration::from_secs(1_000_000)),
            Arc::new(move || {
                let _ = capture_tx.lock().unwrap().send(());
            }),
            Arc::new(|| 7),
        );
        let bt = Arc::new(devices::testing::FakeBt::new(vec![devices::BtDevice {
            id: "bt-1".into(),
            name: "Headphones".into(),
            connected: false,
        }]));
        let wifi = Arc::new(devices::testing::FakeWifi::new(vec![]));
        let chores = Arc::new(chores::testing::FakeChores::default());
        let (extras_tx, extras_ready) = std::sync::mpsc::channel();
        let extras_tx = Mutex::new(extras_tx);
        let (report_tx, reports) = std::sync::mpsc::channel();
        let report_tx = Mutex::new(report_tx);
        let extras = Extras::over(
            launcher.clone(),
            Arc::new(move || {
                let _ = extras_tx.lock().unwrap().send(());
            }),
            Box::new(|| Ok(())),
            // The reminder thread is never started here: nothing fires.
            Arc::new(|_: &str| {}),
            Arc::new(move |name: &str, message: &str| {
                let _ = report_tx.lock().unwrap().send((name.to_string(), message.to_string()));
            }),
            ExtraSources {
                games: Arc::new(FakeGameSource(vec![hades()])),
                bookmarks: Arc::new(FakeBookmarks(vec![bookmarks::Bookmark {
                    title: "Example Domain".into(),
                    url: "https://www.example.com/".into(),
                }])),
                clipboard: Arc::new(clipboard::FakeClipboard::default()),
                fetch: fetch.clone(),
                bt: bt.clone(),
                wifi,
                chores,
                reminders: dir.path().join("reminders.json"),
                now: Arc::new(|| std::time::SystemTime::from("2026-11-17T14:27:00Z".parse::<jiff::Timestamp>().unwrap())),
                tz: jiff::tz::TimeZone::UTC,
            },
        );
        // The fakes answer at once: read them here, as the background threads would.
        extras.games.refresh();
        extras.bookmarks.refresh();
        let providers = Providers {
            refresher: refresh_windows_and_audio(windows.clone(), system.clone(), Arc::new(|| {})),
            windows,
            system,
            status,
            answers,
            weather,
            rates,
            formats,
            apps,
            files: FilesProvider::with_entries(launcher.clone(), files),
            path: browse.path,
            downloads: browse.downloads,
            recent: browse.recent,
            clips: capture.clips,
            qr: capture.qr,
            shortcuts: ShortcutsProvider::new(launcher.clone(), "Debian"),
            wsl: WslProvider::new(launcher.clone(), "Debian"),
            welcome: WelcomeProvider::new(Box::new(|| 0), Box::new(|| Ok(()))),
            update: UpdateProvider::new(Arc::new(Updates::new(
                Arc::new(updates::tests::FakeSource::default()),
                Arc::new(|| {}),
            ))),
            games: extras.games,
            bookmarks: extras.bookmarks,
            workspaces: extras.workspaces,
            reminders: extras.reminders,
            timers: extras.timers,
            words: extras.words,
            devices: extras.devices,
            chores: extras.chores,
        };
        let power = Arc::new(Mutex::new(Vec::new()));
        let opened = Arc::new(AtomicUsize::new(0));
        let counter = opened.clone();
        let history = Arc::new(History::load(dir.path().join("history.json"), true));
        let engine = Arc::new(build_engine(
            &providers,
            Box::new(RecordingRunner(power.clone())),
            Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            Box::new(|| Ok(())),
            ranking.clone(),
            history.clone(),
            Arc::new(Pins::load(dir.path().join("pins.json"))),
        ));
        let runner: std::sync::Weak<dyn ItemRunner> = Arc::<Engine>::downgrade(&engine);
        providers.workspaces.set_runner(runner);
        Rig {
            engine,
            providers,
            launcher,
            power,
            opened,
            dir,
            ranking,
            history,
            status_ready,
            browse_ready,
            links,
            clip_history,
            clip_text,
            capture_ready,
            fetch,
            extras_ready,
            reports,
            bt,
        }
    }

    /// Games a scan would find; nothing is read from disk.
    struct FakeGameSource(Vec<games::Game>);

    impl GameSource for FakeGameSource {
        fn scan(&self) -> Vec<games::Game> {
            self.0.clone()
        }
    }

    /// Bookmarks as if read from Firefox; nothing is read from disk.
    struct FakeBookmarks(Vec<bookmarks::Bookmark>);

    impl BookmarkSource for FakeBookmarks {
        fn read(&self) -> Result<Vec<bookmarks::Bookmark>, String> {
            Ok(self.0.clone())
        }
    }

    fn hades() -> games::Game {
        games::Game {
            id: "steam:1145360".into(),
            name: "Hades".into(),
            store: games::Store::Steam,
            start: games::Start::Uri("steam://rungameid/1145360".into()),
            folder: Some(PathBuf::from(r"D:\SteamLibrary\steamapps\common\Hades")),
            icon: "builtin:game".into(),
        }
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

    fn chrome_window() -> WinInfo {
        WinInfo {
            hwnd: 1,
            pid: 20,
            title: "Docs".into(),
            exe: r"C:\Apps\Chrome\chrome.exe".into(),
            app: "Google Chrome".into(),
            elevated: false,
        }
    }

    #[test]
    fn an_open_window_ranks_above_its_app_even_when_learned() {
        let r = rig_with(vec![app("Google Chrome")], vec![], vec![chrome_window()], Default::default());
        let id = r.engine.query("chrome", None).iter().find(|i| i.provider == "apps").unwrap().id.clone();
        for _ in 0..30 {
            r.ranking.record("apps", &id);
        }
        assert_eq!(r.engine.query("chrome", None)[0].provider, "windows");
    }

    /// Waits for a background folder read to finish, then asks again.
    fn query_ready(r: &Rig, text: &str) -> Vec<provider::ResultItem> {
        let first = r.engine.query(text, None);
        if first.iter().any(|i| i.title == "Reading\u{2026}") {
            r.browse_ready.recv_timeout(Duration::from_secs(5)).expect("the folder read finishes");
            return r.engine.query(text, None);
        }
        first
    }

    #[test]
    fn typing_a_path_shows_only_path_rows() {
        let fs = Arc::new(dirs::testing::FakeFs::default());
        fs.add_dir(r"C:\", vec![dirs::testing::folder("Games")]);
        let r = rig_with(vec![], vec![file(r"C:\d\Games.txt")], vec![], fs);
        let first = r.engine.query("C:\\", None);
        assert!(!first.is_empty() && first.iter().all(|i| i.provider == "path"), "{:?}", titles(&first));
        assert_eq!(first[0].title, "Reading\u{2026}");
        let ready = query_ready(&r, "C:\\");
        assert!(ready.iter().all(|i| i.provider == "path"), "{:?}", titles(&ready));
        assert!(titles(&ready).contains(&"Games".to_string()), "{:?}", titles(&ready));
    }

    #[test]
    fn dl_and_recent_are_words() {
        let fs = Arc::new(dirs::testing::FakeFs::default());
        fs.add_dir(r"C:\Users\someone\Downloads", vec![dirs::testing::file("setup.zip", 2048)]);
        fs.add_dir(r"C:\Users\someone\Recent", vec![dirs::testing::file("notes.txt.lnk", 1)]);
        fs.set_kind(r"C:\Users\someone\Documents\notes.txt", Some(false));
        let r = rig_with(vec![], vec![], vec![], fs);
        let lnk = PathBuf::from(r"C:\Users\someone\Recent\notes.txt.lnk");
        r.links.add(&lnk, r"C:\Users\someone\Documents\notes.txt");
        let dl = query_ready(&r, "dl");
        assert!(dl.iter().any(|i| i.provider == "downloads" && i.title == "setup.zip"), "{:?}", titles(&dl));
        assert!(!r.engine.query("dlls", None).iter().any(|i| i.provider == "downloads"));
        let recent = query_ready(&r, "recent");
        assert!(!recent.is_empty() && recent.iter().any(|i| i.provider == "recent"), "{:?}", titles(&recent));
        let empty = r.engine.query("", None);
        assert!(!empty.iter().any(|i| matches!(i.provider, "path" | "downloads" | "recent")));
    }

    #[test]
    fn path_and_list_rows_are_never_recorded() {
        let fs = Arc::new(dirs::testing::FakeFs::default());
        fs.add_dir(r"C:\", vec![dirs::testing::folder("Games")]);
        fs.add_dir(r"C:\Users\someone\Downloads", vec![dirs::testing::file("setup.zip", 2048)]);
        let r = rig_with(vec![], vec![], vec![], fs);
        let path_row = query_ready(&r, "C:\\").into_iter().find(|i| i.title == "Games").unwrap();
        assert_eq!(r.engine.activate("path", &path_row.id, provider::Modifier::None, "C:\\", None), Ok(Outcome::Hide));
        let dl_row = query_ready(&r, "dl").into_iter().find(|i| i.provider == "downloads").unwrap();
        assert_eq!(r.engine.activate("downloads", &dl_row.id, provider::Modifier::None, "dl", None), Ok(Outcome::Hide));
        assert!(!r.dir.path().join("picks.json").exists());
        assert!(r.history.recents().is_empty());
        assert!(r.history.searches().is_empty());
        assert_eq!(r.launcher.calls().len(), 2);
    }

    #[test]
    fn a_running_app_offers_close() {
        let r = rig_apps(
            |l| {
                AppsProvider::with_programs(
                    l,
                    vec![app("Google Chrome")],
                    vec![(r"C:\definitely\missing\Google Chrome.lnk", r"C:\Apps\Chrome\chrome.exe")],
                    vec![],
                )
            },
            vec![],
            vec![chrome_window()],
            Default::default(),
        );
        let row = r.engine.query("google chrome", None).into_iter().find(|i| i.provider == "apps").unwrap();
        assert!(row.actions.contains(&provider::Action::Close), "{:?}", row.actions);
        assert_eq!(r.engine.act("apps", &row.id, provider::Action::Close), Ok(Outcome::Hide));
    }

    fn chrome_tab(hwnd: isize, title: &str) -> WinInfo {
        WinInfo { hwnd, title: title.into(), ..chrome_window() }
    }

    #[test]
    fn an_unrelated_window_title_does_not_beat_an_exact_app() {
        let r = rig_with(vec![app("Word")], vec![], vec![chrome_tab(1, "Wordle - Google Chrome")], Default::default());
        let res = r.engine.query("word", None);
        assert_eq!((res[0].provider, res[0].title.as_str()), ("apps", "Word"));
    }

    #[test]
    fn many_windows_of_one_app_do_not_fill_the_list() {
        let apps = ["Gimp", "Git Bash", "Godot", "Gedit", "Gnome Boxes"].map(app).to_vec();
        let wins = (1..=8).map(|n| chrome_tab(n, &format!("Tab {n}"))).collect();
        let r = rig_with(apps, vec![], wins, Default::default());
        let res = r.engine.query("g", None);
        // The first page of eight, what the bar shows.
        let windows = res[..8].iter().filter(|i| i.provider == "windows").count();
        assert!(windows <= 3, "{windows} window rows");
        assert_eq!(res[..8].iter().filter(|i| i.provider == "apps").count(), 5);
    }

    #[test]
    fn status_answers_sit_on_top() {
        let r = rig(vec![app("Battery")], vec![]);
        assert_eq!(r.engine.query("battery", None)[0].title, "Checking\u{2026}");
        r.status_ready.recv_timeout(Duration::from_secs(2)).expect("sample finished");
        let res = r.engine.query("battery", None);
        assert_eq!((res[0].provider, res[0].title.as_str()), ("status", "82%"));
    }

    #[test]
    fn help_lists_the_new_commands() {
        let r = rig(vec![], vec![]);
        let help = titles(&r.engine.query("help", None));
        for needle in ["close chrome", "vol 30", "battery"] {
            assert!(help.iter().any(|t| t.contains(needle)), "{needle}: {help:?}");
        }
    }

    #[test]
    fn windows_rows_are_never_recorded() {
        let r = rig_with(vec![], vec![], vec![chrome_window()], Default::default());
        let res = r.engine.query("docs", None);
        let row = res.iter().find(|i| i.provider == "windows").unwrap();
        assert_eq!(r.engine.activate("windows", &row.id, provider::Modifier::None, "docs", None), Ok(Outcome::Hide));
        assert_eq!(r.ranking.boost("windows", &row.id), 0);
        assert!(r.history.recents().is_empty());
        assert!(r.history.searches().is_empty());
        assert!(!r.dir.path().join("picks.json").exists());
    }

    #[test]
    fn system_toggles_are_never_recorded() {
        let r = rig(vec![], vec![]);
        for (text, id) in [("wifi off", "radio:wifi:off"), ("dark mode", "theme:dark"), ("mute", "mute")] {
            let res = r.engine.query(text, None);
            assert!(res.iter().any(|i| i.id == id), "{id}");
            assert_eq!(r.engine.activate("system", id, provider::Modifier::None, text, None), Ok(Outcome::Hide));
            assert_eq!(r.ranking.boost("system", id), 0);
        }
        assert!(r.history.recents().is_empty());
        assert!(r.history.searches().is_empty());
        assert!(!r.dir.path().join("picks.json").exists());
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
                "close chrome / quit all",
                "report.pdf",
                r"C:\ / ~\Documents\",
                "dl",
                "recent",
                "v / v invoice",
                "qr / qr some text",
                "snip",
                "color",
                "your shortcuts",
                "> htop -d 10",
                "bluetooth",
                "mute / vol 30 / next song / dark mode / wifi off",
                "connect headphones",
                "disconnect headphones",
                "wifi <name>",
                "battery / cpu / ram / disk / ip",
                "empty bin",
                "eject / eject E:",
                "flush dns",
                "restart explorer",
                "timer 5m",
                "remind 20m pizza / remind 18:30 call mum",
                "timers",
                "tr guten Morgen",
                "def serendipity",
                "lock / sleep / restart / shut down / sign out",
                "flick config",
                "settings",
                "clear history",
                "help",
                "Esc",
                "Ctrl+P",
                "Up",
                "Tab",
            ]
        );
        let flick = &help[34];
        assert_eq!(flick.subtitle, "open the config file to change the hotkey, file folders and shortcuts");
        assert_eq!(flick.id, "flick config");
    }

    #[test]
    fn help_word_ignores_case() {
        let r = rig(vec![], vec![]);
        assert_eq!(r.engine.query("Help", None).len(), 42);
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
        assert!(res[..file_at]
                .iter()
                .all(|i| i.provider == "settings" || (i.provider == "system" && i.id.starts_with("radio:bt:"))), "{:?}", titles(&res));
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
        let all = r.engine.query("calc", None);
        assert!(all.len() >= 8, "{:?}", titles(&all));
        // The first page of eight, what the bar shows.
        let res = &all[..8];
        assert_eq!((res[0].provider, res[0].title.as_str()), ("apps", "Rekenmachine"), "{:?}", titles(res));
        assert!(titles(res).contains(&"Math Tool".to_string()), "{:?}", titles(res));
        // Files hold at most five rows above the last other result; only rows
        // left over after every other result go to further files.
        let last_other = res.iter().rposition(|i| i.provider != "files").unwrap();
        assert!(res[..last_other].iter().filter(|i| i.provider == "files").count() <= 5, "{:?}", titles(res));
        assert!(res.iter().all(|i| i.provider != "files" || i.score < 650));
    }

    #[test]
    fn flick_config_is_findable() {
        let r = rig(vec![], vec![]);
        for text in ["flick config", "edit config", "flick", "config"] {
            let res = r.engine.query(text, None);
            let row = res.iter().find(|i| i.id == "config").unwrap_or_else(|| panic!("{text}"));
            assert_eq!((row.provider, row.title.as_str()), ("flick", "Flick: edit config"), "{text}");
        }
        for text in ["flick config", "edit config"] {
            assert_eq!(r.engine.query(text, None)[0].id, "config", "{text}");
        }
        let id = r.engine.query("flick config", None)[0].id.clone();
        assert_eq!(r.engine.activate("flick", &id, provider::Modifier::None, "", None), Ok(Outcome::Hide));
        assert_eq!(r.opened.load(Ordering::SeqCst), 1);
        assert!(r.launcher.calls().is_empty());
    }

    #[test]
    fn typing_settings_ranks_flick_settings_above_the_windows_app() {
        let r = rig(vec![app("Settings")], vec![]);
        let res = r.engine.query("settings", None);
        assert_eq!((res[0].provider, res[0].id.as_str()), ("flick", "settings"), "{:?}", res.iter().map(|i| &i.title).collect::<Vec<_>>());
        assert!(res.iter().any(|i| i.provider == "apps" && i.title == "Settings"));
        // Even after Windows Settings was launched from the bar many times.
        let app_id = res.iter().find(|i| i.provider == "apps").unwrap().id.clone();
        for _ in 0..30 {
            r.ranking.record("apps", &app_id);
        }
        let res = r.engine.query("settings", None);
        assert_eq!((res[0].provider, res[0].id.as_str()), ("flick", "settings"));
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

    fn ubuntu() -> WslState {
        WslState::Present { default: "Ubuntu".into(), distros: vec!["Ubuntu".into(), "Debian".into()] }
    }

    #[test]
    fn config_distro_wins_over_detected() {
        let ubuntu = ubuntu();
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
        let found = wsl_detect::Found { default: "Ubuntu".into(), distros: vec!["Ubuntu".into(), "Debian".into()] };
        assert_eq!(WslState::from_detection(Found(Some(found))), Some(ubuntu()));
        assert_eq!(WslState::from_detection(Found(None)), Some(WslState::NoWsl));
        assert_eq!(WslState::from_detection(Failed), None);
    }

    #[test]
    fn push_config_uses_the_detected_distro_when_none_is_configured() {
        let r = rig(vec![], vec![]);
        let old = Config::default();
        assert!(!push_config(&r.providers, &old, &old, &WslState::Unknown));
        assert!(r.engine.query("> ls", None).is_empty(), "no WSL: nothing offered");
        assert!(!push_config(&r.providers, &old, &old, &ubuntu()));
        assert_eq!(r.engine.query("> ls", None)[0].subtitle, "run in Ubuntu");
        let set = Config { wsl_distro: "Debian".into(), ..Config::default() };
        push_config(&r.providers, &old, &set, &ubuntu());
        assert_eq!(r.engine.query("> ls", None)[0].subtitle, "run in Debian");
    }

    #[test]
    fn push_distro_applies_the_effective_distro_to_wsl_and_shortcuts() {
        let r = rig(vec![], vec![]);
        let config = Config { shortcuts: vec![shortcut("hello", "notepad")], ..Config::default() };
        push_distro(&r.providers, &config, &ubuntu());
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

    fn clip(id: &str, text: &str, secs_ago: u64) -> cliphistory::Clip {
        cliphistory::Clip {
            id: id.into(),
            text: text.into(),
            copied: std::time::UNIX_EPOCH + Duration::from_secs(1_000_000 - secs_ago),
            pinned: false,
        }
    }

    /// A rig whose clipboard history holds an invoice, a link and a note.
    fn clip_rig() -> Rig {
        let r = rig(vec![app("VLC media player")], vec![]);
        r.clip_history.set(Ok(cliphistory::History::Items(vec![
            clip("a", "invoice 2026-114", 60),
            clip("b", "https://example.com/vlc", 300),
            clip("c", "meeting notes", 900),
        ])));
        r
    }

    /// Waits for a clipboard (history) read to finish, then asks again.
    fn query_capture(r: &Rig, text: &str) -> Vec<provider::ResultItem> {
        let first = r.engine.query(text, None);
        if first.iter().any(|i| i.title.starts_with("Reading clipboard")) {
            r.capture_ready.recv_timeout(Duration::from_secs(5)).expect("the clipboard read finishes");
            return r.engine.query(text, None);
        }
        first
    }

    fn of<'a>(rows: &'a [provider::ResultItem], provider: &str) -> Vec<&'a str> {
        rows.iter().filter(|i| i.provider == provider).map(|i| i.title.as_str()).collect()
    }

    #[test]
    fn v_is_a_word_route() {
        let r = clip_rig();
        let first = r.engine.query("v", None);
        assert_eq!(titles(&first), ["Reading clipboard history\u{2026}"]);
        let all = query_capture(&r, "v");
        assert!(all.iter().all(|i| i.provider == "clips"), "{:?}", titles(&all));
        assert_eq!(titles(&all), ["invoice 2026-114", "https://example.com/vlc", "meeting notes"]);
        assert_eq!(all[0].subtitle, "1 min ago");
        // A word that only starts with v is a normal search.
        let vlc = r.engine.query("vlc", None);
        assert!(of(&vlc, "clips").is_empty(), "{:?}", titles(&vlc));
        assert_eq!(vlc[0].provider, "apps");
        // Typing after `v ` filters what was read, without reading again.
        let inv = r.engine.query("v inv", None);
        assert_eq!(titles(&inv), ["invoice 2026-114"]);
        assert_eq!(r.clip_history.reads(), 1);
        assert!(r.launcher.calls().is_empty());
    }

    #[test]
    fn clipboard_rows_are_never_recorded() {
        let r = clip_rig();
        let row = query_capture(&r, "v").into_iter().find(|i| i.title == "meeting notes").unwrap();
        assert_eq!(
            r.engine.activate("clips", &row.id, provider::Modifier::None, "v", None),
            Ok(Outcome::HideThen { then: provider::Then::Paste { before: 7 } })
        );
        assert_eq!(r.clip_history.set_calls(), ["c"]);
        assert_eq!(r.ranking.boost("clips", &row.id), 0);
        assert!(r.history.recents().is_empty());
        assert!(r.history.searches().is_empty());
        assert!(!r.dir.path().join("picks.json").exists());
        assert!(!r.engine.toggle_pin("clips", &row.id));
        let empty = r.engine.query("", None);
        assert!(of(&empty, "clips").is_empty() && of(&empty, "qr").is_empty(), "{:?}", titles(&empty));
    }

    #[test]
    fn qr_rows_are_never_recorded() {
        let r = rig(vec![], vec![]);
        let res = r.engine.query("qr hi", None);
        assert_eq!(res.len(), 1);
        assert_eq!((res[0].provider, res[0].title.as_str()), ("qr", "hi"));
        assert!(!res[0].panel.is_empty());
        assert_eq!(r.engine.activate("qr", &res[0].id, provider::Modifier::None, "qr hi", None), Ok(Outcome::Hide));
        assert_eq!(r.ranking.boost("qr", &res[0].id), 0);
        assert!(r.history.recents().is_empty());
        assert!(r.history.searches().is_empty());
        assert!(!r.dir.path().join("picks.json").exists());
        assert!(!r.engine.toggle_pin("qr", &res[0].id));
        // Bare `qr` shows the clipboard text.
        let bare = query_capture(&r, "qr");
        assert_eq!(titles(&bare), ["hello"]);
        let empty = r.engine.query("", None);
        assert!(of(&empty, "clips").is_empty() && of(&empty, "qr").is_empty(), "{:?}", titles(&empty));
    }

    #[test]
    fn snip_and_color_top_their_words() {
        let r = rig(vec![], vec![]);
        assert_eq!(r.engine.query("snip", None)[0].provider, "snip");
        assert_eq!(r.engine.query("screenshot", None)[0].provider, "snip");
        let color = r.engine.query("color", None);
        assert_eq!(color[0].provider, "color");
        assert!(color.iter().any(|i| i.provider == "settings"), "{:?}", titles(&color));
        assert_eq!(r.engine.query("colour", None)[0].provider, "color");
        assert_eq!(
            r.engine.activate("snip", "snip", provider::Modifier::None, "snip", None),
            Ok(Outcome::HideThen { then: provider::Then::Snip })
        );
        assert_eq!(
            r.engine.activate("color", "color", provider::Modifier::None, "color", None),
            Ok(Outcome::HideThen { then: provider::Then::PickColor })
        );
        assert!(!r.dir.path().join("picks.json").exists());
        assert!(r.history.recents().is_empty());
        assert!(r.launcher.calls().is_empty(), "the snip opens only after the bar hides");
    }

    #[test]
    fn hiding_the_bar_forgets_what_was_read() {
        let r = clip_rig();
        query_capture(&r, "v");
        query_capture(&r, "qr");
        assert_eq!((r.clip_history.reads(), r.clip_text.calls()), (1, 1));
        r.providers.on_hide();
        assert_eq!(r.clip_history.forgets(), 1);
        assert_eq!(titles(&r.engine.query("v", None)), ["Reading clipboard history\u{2026}"]);
        assert_eq!(titles(&query_capture(&r, "v")).len(), 3);
        assert_eq!(r.clip_history.reads(), 2);
        assert_eq!(titles(&query_capture(&r, "qr")), ["hello"]);
        assert_eq!(r.clip_text.calls(), 2);
    }

    /// Providers whose rows only answer a typed command.
    const TYPED: [&str; 4] = ["timers", "words", "devices", "chores"];

    fn providers_of(rows: &[provider::ResultItem]) -> Vec<&'static str> {
        rows.iter().map(|i| i.provider).collect()
    }

    /// Waits for a device, Recycle Bin or drive read to finish, then asks again.
    fn query_extras(r: &Rig, text: &str) -> Vec<provider::ResultItem> {
        let first = r.engine.query(text, None);
        if first.iter().any(|i| i.title.starts_with("Reading") || i.subtitle == "Recycle Bin") {
            r.extras_ready.recv_timeout(Duration::from_secs(5)).expect("the read finishes");
            return r.engine.query(text, None);
        }
        first
    }

    #[test]
    fn games_and_bookmarks_are_found_by_name() {
        let r = rig(vec![], vec![]);
        let res = r.engine.query("hades", None);
        let game = res.iter().find(|i| i.provider == "games").unwrap_or_else(|| panic!("{:?}", titles(&res)));
        assert_eq!((game.title.as_str(), game.subtitle.as_str()), ("Hades", "Steam"));
        let res = r.engine.query("example", None);
        let mark = res.iter().find(|i| i.provider == "bookmarks").unwrap_or_else(|| panic!("{:?}", titles(&res)));
        assert_eq!((mark.title.as_str(), mark.subtitle.as_str()), ("Example Domain", "example.com"));
        assert!(r.launcher.calls().is_empty());
    }

    #[test]
    fn claimed_words_leave_lookalikes_to_search() {
        let r = rig(vec![app("Timeline"), app("Trello"), app("Connections")], vec![]);
        for text in ["timeline", "trello", "define", "connections", "ejector"] {
            let res = r.engine.query(text, None);
            assert!(!res.iter().any(|i| TYPED.contains(&i.provider)), "{text}: {:?}", providers_of(&res));
        }
        for (text, provider) in [("timer 5m", "timers"), ("tr hi", "words"), ("connect", "devices"), ("eject", "chores")] {
            let res = r.engine.query(text, None);
            assert!(!res.is_empty() && res.iter().all(|i| i.provider == provider), "{text}: {:?}", providers_of(&res));
        }
    }

    #[test]
    fn a_wifi_name_is_claimed_and_never_a_past_search() {
        let r = rig(vec![app("Notepad")], vec![]);
        let res = r.engine.query("wifi home", None);
        assert!(!res.is_empty() && res.iter().all(|i| i.provider == "devices"), "{:?}", providers_of(&res));
        // Claimed text is never a past search (`Engine::searchable`).
        let claimed = |text: &str| r.engine.providers().any(|p| p.claims(text));
        assert!(claimed("wifi home"));
        // The toggles keep their words, unclaimed.
        for text in ["wifi on", "wifi off", "wifi"] {
            assert!(!claimed(text), "{text}");
        }
        for text in ["wifi on", "wifi off"] {
            assert!(r.engine.query(text, None).iter().any(|i| i.provider == "system"), "{text}");
        }
    }

    #[test]
    fn typed_extras_are_never_recorded() {
        let r = rig(vec![], vec![]);
        let res = r.engine.query("timer 5m", None);
        assert_eq!(providers_of(&res), ["timers"]);
        assert_eq!(res[0].title, "Timer 5 min \u{2014} rings at 14:32");
        assert_eq!(r.engine.activate("timers", &res[0].id, provider::Modifier::None, "timer 5m", None), Ok(Outcome::Hide));
        assert!(r.history.recents().is_empty());
        assert!(r.history.searches().is_empty());
        assert!(!r.dir.path().join("picks.json").exists());
        assert!(r.dir.path().join("reminders.json").exists(), "a set timer is kept until it rings");
        let pending = r.engine.query("timers", None);
        let row = pending.iter().find(|i| i.confirm).unwrap_or_else(|| panic!("{:?}", titles(&pending)));
        assert_eq!(r.engine.activate("timers", &row.id, provider::Modifier::None, "timers", None), Ok(Outcome::Keep));
        assert!(!r.dir.path().join("reminders.json").exists(), "a cancelled timer leaves the disk");
        assert!(r.history.searches().is_empty());
        assert!(!r.dir.path().join("picks.json").exists());
    }

    fn item_ref(provider: &str, id: &str) -> ItemRef {
        ItemRef { provider: provider.into(), id: id.into(), title: id.into() }
    }

    #[test]
    fn a_workspace_opens_its_items_in_order() {
        // Apps open through the shell itself, not the launcher: the program
        // item here is a game, which opens through the recording launcher.
        let r = rig(vec![], vec![]);
        let game = r.engine.query("hades", None).into_iter().find(|i| i.provider == "games").unwrap();
        let mark = r.engine.query("example", None).into_iter().find(|i| i.provider == "bookmarks").unwrap();
        let work = config::WorkspaceDef {
            name: "work".into(),
            aliases: vec![],
            items: vec![
                item_ref("games", &game.id),
                item_ref("bookmarks", &mark.id),
                item_ref("apps", r"C:\definitely\missing\Gone.lnk"),
            ],
        };
        let old = Config::default();
        let new = Config { workspaces: vec![work], ..Config::default() };
        push_config(&r.providers, &old, &new, &WslState::Unknown);
        let row = r.engine.query("work", None).into_iter().find(|i| i.provider == "workspaces").unwrap();
        assert_eq!((row.title.as_str(), row.subtitle.as_str()), ("Open workspace work", "3 items"));
        assert_eq!(r.engine.activate("workspaces", "work", provider::Modifier::None, "work", None), Ok(Outcome::Hide));
        let report = r.reports.recv_timeout(Duration::from_secs(5)).expect("the run reports its failure");
        let calls = r.launcher.calls();
        assert_eq!(report, ("work".to_string(), "1 of 3 could not open".to_string()), "{calls:?}");
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].contains("steam://rungameid/1145360") && calls[1].contains("example.com"), "{calls:?}");
        assert_eq!(r.ranking.boost("games", &game.id), 0);
        assert_eq!(r.ranking.boost("bookmarks", &mark.id), 0);
    }

    #[test]
    fn confirm_chores_and_cancels_need_a_second_enter() {
        let r = rig(vec![], vec![]);
        let first = |text: &str, title: &str| {
            let res = query_extras(&r, text);
            res.into_iter().find(|i| i.title == title).unwrap_or_else(|| panic!("{text}: no {title}"))
        };
        assert!(first("empty bin", "Empty Recycle Bin").confirm);
        assert!(first("restart explorer", "Restart Explorer").confirm);
        assert!(!first("flush dns", "Flush DNS cache").confirm);
        let timer = r.engine.query("timer 5m", None).remove(0);
        r.engine.activate("timers", &timer.id, provider::Modifier::None, "timer 5m", None).unwrap();
        let pending = r.engine.query("timers", None);
        assert!(!pending.is_empty() && pending.iter().all(|i| i.confirm), "{:?}", titles(&pending));
    }

    #[test]
    fn online_off_reaches_tr_and_def() {
        let r = rig(vec![], vec![]);
        let off = Config { online: false, ..Config::default() };
        push_config(&r.providers, &Config::default(), &off, &WslState::Unknown);
        assert_eq!(titles(&r.engine.query("tr hallo", None)), ["Online answers are off"]);
        assert_eq!(titles(&r.engine.query("def Haus", None)), ["Online answers are off"]);
        std::thread::sleep(Duration::from_millis(600));
        assert!(r.fetch.urls().is_empty(), "{:?}", r.fetch.urls());
    }

    #[test]
    fn hiding_the_bar_forgets_waiting_lookups() {
        let r = rig(vec![], vec![]);
        assert_eq!(titles(&r.engine.query("tr hallo", None)), ["Translating\u{2026}"]);
        r.providers.on_hide();
        std::thread::sleep(Duration::from_millis(700));
        assert!(r.fetch.urls().is_empty(), "{:?}", r.fetch.urls());
        // A lookup left waiting is sent once the pause has passed.
        assert_eq!(titles(&r.engine.query("def Haus", None)), ["Looking up\u{2026}"]);
        r.extras_ready.recv_timeout(Duration::from_secs(5)).expect("the lookup finishes");
        assert_eq!(r.fetch.urls().len(), 1, "{:?}", r.fetch.urls());
    }

    #[test]
    fn quitting_writes_a_failed_reminder_save_once() {
        let r = rig(vec![], vec![]);
        let path = r.dir.path().join("reminders.json");
        let now = std::time::SystemTime::from("2026-11-17T14:27:00Z".parse::<jiff::Timestamp>().unwrap());
        let first = r.providers.reminders.add(now + Duration::from_secs(600), "first", false).unwrap();
        r.providers.reminders.add(now + Duration::from_secs(1200), "second", false).unwrap();
        // A folder with the file's name: the cancel's save fails.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(r.providers.reminders.cancel(first.id));
        std::fs::remove_dir(&path).unwrap();
        quit_cleanup(Some(&r.providers.reminders));
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("second") && !saved.contains("first"), "{saved}");
        // Run again (Quit after the updater's hook): nothing left to write.
        std::fs::remove_file(&path).unwrap();
        quit_cleanup(Some(&r.providers.reminders));
        quit_cleanup(None);
        assert!(!path.exists());
    }

    #[test]
    fn the_item_picker_never_fetches_or_reads() {
        let r = rig(vec![], vec![]);
        // A stored device row resolves before any list was read (a hotkey right after start).
        let standin = r.engine.item("devices", "connect:bt-9").expect("rebuilt from its id");
        assert_eq!((standin.title.as_str(), standin.pinnable), ("Bluetooth audio device", true));
        for text in ["ip", "weather vienna", "20 usd in eur", "tr hallo", "def Haus", "wifi home", "connect", "empty bin", "eject", "v"] {
            let rows = item_search::search(&r.engine, text, false);
            assert!(rows.iter().all(|row| !item_search::PICKER_SKIPS.contains(&row.provider.as_str())), "{text}");
        }
        // Longer than every typing pause (ip, weather, tr/def).
        std::thread::sleep(Duration::from_millis(800));
        assert!(r.fetch.urls().is_empty(), "{:?}", r.fetch.urls());
        assert_eq!(r.clip_history.reads(), 0);
        // What can be stored is found: a game, the empty-bin chore (armed on a
        // hotkey) and, once the list is read, a paired device.
        let found = item_search::search(&r.engine, "hades", false);
        assert_eq!(found.iter().map(|p| p.provider.as_str()).collect::<Vec<_>>(), ["games"]);
        let bin = item_search::search(&r.engine, "empty bin", false);
        let bin = bin.iter().find(|p| p.provider == "chores" && p.title == "Empty Recycle Bin").expect("the empty-bin row");
        assert!(bin.confirm);
        let devices = item_search::search(&r.engine, "connect", false);
        let headphones = devices.iter().find(|p| p.provider == "devices").expect("a device row");
        assert_eq!((headphones.id.as_str(), headphones.title.as_str()), ("connect:bt-1", "Headphones"));
        assert_eq!(r.engine.item("devices", "connect:bt-1").map(|i| i.subtitle), Some("Not connected".to_string()));
        // Once the list is read, a device that is not in it is gone.
        assert!(r.engine.item("devices", "connect:bt-9").is_none());
        assert!(r.bt.calls().iter().all(|c| !c.starts_with("connect")), "{:?}", r.bt.calls());
    }

    /// The global-shortcut plugin's one app-wide map, shared by the bar hotkey
    /// and the direct keys: a combo held by either is taken for the other.
    #[derive(Default)]
    struct SharedKeys {
        held: Mutex<Vec<String>>,
    }

    impl direct_keys::KeyRegistry for SharedKeys {
        fn register(&self, keys: &str) -> Result<(), direct_keys::RegisterError> {
            let mut held = self.held.lock().unwrap();
            if held.iter().any(|k| k == keys) {
                return Err(direct_keys::RegisterError::Taken);
            }
            held.push(keys.to_string());
            Ok(())
        }
        fn unregister(&self, keys: &str) -> bool {
            self.held.lock().unwrap().retain(|k| k != keys);
            true
        }
    }

    fn direct(keys: &str) -> config::DirectHotkeyDef {
        config::DirectHotkeyDef { keys: keys.into(), item: item_ref("apps", r"C:\x\Term.lnk") }
    }

    #[test]
    fn direct_keys_follow_a_bar_hotkey_change() {
        let base = Config { hotkey: "Alt+Space".into(), direct_hotkeys: vec![direct("Ctrl+Alt+T")], ..Config::default() };
        let moved = Config { hotkey: "Ctrl+Alt+T".into(), ..base.clone() };
        let more = Config { direct_hotkeys: vec![direct("Ctrl+Alt+T"), direct("Win+N")], ..base.clone() };
        assert!(direct_keys_need_apply(&base, &moved, true));
        assert!(direct_keys_need_apply(&base, &more, false));
        assert!(!direct_keys_need_apply(&base, &base.clone(), false));

        let registry = Arc::new(SharedKeys::default());
        let keys = DirectKeys::new(registry.clone());
        // The bar first, as in `setup`.
        registry.register(&config::normalize_combo(&base.hotkey)).unwrap();
        keys.apply(&base.hotkey, &base.direct_hotkeys);
        assert_eq!(keys.item_for("Ctrl+Alt+T").map(|i| i.id), Some(r"C:\x\Term.lnk".to_string()));

        // The bar hotkey moves onto the direct key's combo.
        let bar_registry = registry.clone();
        let error = reapply_hotkeys(
            &keys,
            &base,
            &moved,
            true,
            |accel| {
                bar_registry.unregister(&config::normalize_combo(&base.hotkey));
                bar_registry.register(&config::normalize_combo(accel)).err().map(|_| "taken".to_string())
            },
            || moved.hotkey.clone(),
        );
        assert_eq!(error, None, "the direct key let go before the bar registered");
        let states: Vec<direct_keys::KeyState> = keys.status(&|_| true).into_iter().map(|s| s.state).collect();
        assert_eq!(states, [direct_keys::KeyState::Clash]);
        assert_eq!(keys.item_for("Ctrl+Alt+T"), None);
        assert_eq!(*registry.held.lock().unwrap(), ["ctrl+alt+t"], "held once, by the bar");

        // Nothing changed: nothing is released or registered again.
        let untouched = reapply_hotkeys(&keys, &moved, &moved, false, |_| panic!("no bar apply"), || panic!("no direct apply"));
        assert_eq!(untouched, None);
    }

    #[test]
    fn every_route_and_claimed_help_word_is_kept_from_workspace_names() {
        let r = rig(vec![], vec![]);
        let mut checked = 0;
        for p in r.engine.providers() {
            if let Some(route) = p.route() {
                let typed = if route.needs_space { route.prefix.to_string() } else { format!("{}x", route.prefix) };
                assert!(config::is_command_word(&typed), "{} routes {}", p.name(), route.prefix);
                checked += 1;
            }
            for line in p.help() {
                let Some(word) = line.fill.split_whitespace().next() else { continue };
                if p.claims(word) || p.claims(&format!("{word} x")) {
                    assert!(config::is_command_word(word), "{} claims {word}", p.name());
                    checked += 1;
                }
            }
        }
        assert!(checked >= 10, "{checked}");
    }

    #[test]
    fn a_settings_bar_hotkey_change_lets_the_bar_win_and_frees_the_old_clash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "hotkey = \"Alt+Space\"\n").unwrap();
        let defs = vec![direct("Ctrl+Alt+T")];
        let registry = Arc::new(SharedKeys::default());
        let keys = DirectKeys::new(registry.clone());
        let bar = Mutex::new("Alt+Space".to_string());
        registry.register(&config::normalize_combo("Alt+Space")).unwrap();
        keys.apply("Alt+Space", &defs);
        assert!(keys.item_for("Ctrl+Alt+T").is_some());

        // As `change_bar_hotkey` does, with the plugin's one map faked.
        let change = |applied: &str, wanted: &str| {
            settings::set_hotkey(&path, Some(applied), wanted, |h| {
                swap_bar_hotkey(
                    &keys,
                    h,
                    &defs,
                    |accel| {
                        let mut bar = bar.lock().unwrap();
                        registry.unregister(&config::normalize_combo(&bar));
                        match registry.register(&config::normalize_combo(accel)) {
                            Ok(()) => {
                                *bar = accel.to_string();
                                None
                            }
                            Err(_) => {
                                registry.register(&config::normalize_combo(&bar)).unwrap();
                                Some(format!("hotkey \"{accel}\" is in use by another program"))
                            }
                        }
                    },
                    || bar.lock().unwrap().clone(),
                )
                .map_or(Ok(()), Err)
            })
        };
        let states = || keys.status(&|_| true).into_iter().map(|s| s.state).collect::<Vec<_>>();

        // The bar moves onto the direct key's combo: the bar wins.
        change("Alt+Space", "Ctrl+Alt+T").expect("the direct key let go first");
        assert_eq!(*bar.lock().unwrap(), "Ctrl+Alt+T");
        assert_eq!(states(), [direct_keys::KeyState::Clash]);
        assert_eq!(keys.item_for("Ctrl+Alt+T"), None);
        assert_eq!(*registry.held.lock().unwrap(), ["ctrl+alt+t"], "held once, by the bar");
        assert!(std::fs::read_to_string(&path).unwrap().contains("Ctrl+Alt+T"));

        // The bar moves away: the entry that clashed registers now, with no reload.
        change("Ctrl+Alt+T", "Ctrl+Shift+K").unwrap();
        assert_eq!(states(), [direct_keys::KeyState::Ok]);
        assert_eq!(keys.item_for("Ctrl+Alt+T").map(|i| i.id), Some(r"C:\x\Term.lnk".to_string()));
        let mut held = registry.held.lock().unwrap().clone();
        held.sort();
        assert_eq!(held, [config::normalize_combo("Ctrl+Alt+T"), config::normalize_combo("Ctrl+Shift+K")]);
    }
}
