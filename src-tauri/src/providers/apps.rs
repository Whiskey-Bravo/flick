//! Apps provider: Start-menu shortcuts and Store apps, fuzzy-searched in memory.

use std::collections::hash_map::{Entry, HashMap};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, RwLockReadGuard};
use std::time::Duration;

use crate::fuzzy;
use crate::launch::{self, Launcher};
use crate::process;
use crate::provider::{Action, HelpLine, Modifier, Outcome, Provider, Query, ResultItem};
use crate::uninstall::{self, plan, split_command, Program, ProgramSource, Uninstall};
use crate::winlist::{WinInfo, WindowList};

/// Shortcuts whose name has one of these as a whole word (case-insensitive,
/// split like `fuzzy::words`) are not apps. "Revo Uninstaller" and "HelpNDoc" stay.
const SKIP_WORDS: [&str; 8] = [
    "uninstall",
    "readme",
    "help",
    "documentation",
    "website",
    "deinstallieren",
    "hilfe",
    "dokumentation",
];

/// `Get-StartApps | ConvertTo-Json`, with stdout forced to UTF-8 so non-ASCII
/// app names survive the pipe (the console code page would mangle them).
const START_APPS_SCRIPT: &str =
    "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-StartApps | ConvertTo-Json";

#[derive(Clone, Debug, PartialEq)]
pub enum AppKind {
    Shortcut,
    Store,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppEntry {
    pub name: String,
    pub target: String,
    pub kind: AppKind,
    /// Other names the app is found by (exe or package name), already
    /// normalised. Filled during refresh, never at query time.
    pub aliases: Vec<String>,
}

/// Subtracted from an alias match: two `fuzzy::score` tiers. An alias prefix
/// match (700) ranks below a name word-prefix (800) or acronym (750) match;
/// an exact alias match (800) ties a name word-prefix match.
pub const ALIAS_PENALTY: u32 = 200;

/// `scan_store_apps` gives up (and kills PowerShell) after this long.
pub const STORE_SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// Shortcut targets that host many unrelated apps: their stem is no alias,
/// or every script, installer and Electron app would match it.
const GENERIC_HOSTS: [&str; 15] = [
    "cmd", "powershell", "pwsh", "wscript", "cscript", "rundll32", "msiexec", "explorer", "javaw",
    "java", "python", "pythonw", "node", "electron", "update",
];

/// An alias needs this many chars (spaces and separators ignored): a shorter
/// one would match almost any short query.
const MIN_ALIAS_CHARS: usize = 3;

/// `raw` normalised, unless it is shorter than [`MIN_ALIAS_CHARS`] or the
/// normalised `name` already contains it, spaces and separators ignored
/// ("Windows Terminal" has `WindowsTerminal`): then the name finds the app anyway.
fn alias_for(name: &str, raw: &str) -> Option<String> {
    let alias = fuzzy::normalize(raw.trim());
    let squashed: String = fuzzy::squash(&alias).collect();
    let name: String = fuzzy::squash(&fuzzy::normalize(name)).collect();
    (squashed.chars().count() >= MIN_ALIAS_CHARS && !name.contains(&squashed)).then_some(alias)
}

/// Last package-name segments that name a channel or packaging, not the app:
/// the segment before them is used instead.
const CHANNEL_WORDS: [&str; 9] =
    ["stable", "beta", "preview", "dev", "canary", "desktop", "app", "client", "store"];

/// Leading vendor words stripped from a package-name segment.
const VENDOR_WORDS: [&str; 2] = ["windows", "microsoft"];

/// The app part of a Store AppID's package name (before `_`): its last
/// `.`-segment, or the one before it when the last is a channel word, minus
/// one leading `Windows`/`Microsoft` followed by a letter.
/// `Microsoft.WindowsCalculator_8wekyb3d8bbwe!App` gives `Calculator`,
/// `Microsoft.MicrosoftEdge.Stable_…` gives `Edge`. `None` without `!` and
/// `_`, or when what is left is empty, only a vendor word, a channel word
/// (`WindowsApp`), all digits or hex-looking.
fn store_alias(app_id: &str) -> Option<&str> {
    let (family, _) = app_id.split_once('!')?;
    let (package, _) = family.split_once('_')?;
    let mut segments = package.rsplit('.');
    let mut segment = segments.next()?;
    if CHANNEL_WORDS.iter().any(|w| segment.eq_ignore_ascii_case(w)) {
        segment = segments.next()?;
    }
    for vendor in VENDOR_WORDS {
        let head = segment.get(..vendor.len());
        if head.is_some_and(|h| h.eq_ignore_ascii_case(vendor)) {
            let rest = &segment[vendor.len()..];
            if rest.is_empty() {
                return None;
            }
            if rest.starts_with(|c: char| c.is_alphabetic()) {
                segment = rest;
            }
            break;
        }
    }
    if CHANNEL_WORDS.iter().any(|w| segment.eq_ignore_ascii_case(w)) {
        return None;
    }
    let hex_looking = segment.chars().all(|c| c.is_ascii_hexdigit())
        && segment.chars().any(|c| c.is_ascii_digit());
    (!segment.is_empty() && !hex_looking).then_some(segment)
}

fn store_aliases(name: &str, app_id: &str) -> Vec<String> {
    store_alias(app_id).and_then(|raw| alias_for(name, raw)).into_iter().collect()
}

/// The stem of a shortcut's stored target when that is an `.exe`
/// (`WINWORD.EXE` gives `winword`), normalised. `None` for other targets,
/// generic hosts, or a stem the name already contains.
fn shortcut_alias(name: &str, target: &str) -> Option<String> {
    let path = Path::new(target);
    if !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")) {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    if GENERIC_HOSTS.iter().any(|h| stem.eq_ignore_ascii_case(h)) {
        return None;
    }
    alias_for(name, stem)
}

/// Fill each shortcut's alias from its `.lnk` target. Reads every link (a
/// few hundred, local disk) with COM initialised once: refresh thread only.
/// Returns each shortcut's `.exe` target, by shortcut path, and which
/// shortcuts pass command-line arguments.
fn add_shortcut_aliases(entries: &mut [AppEntry]) -> Targets {
    let mut found = Targets::default();
    launch::with_com(|| {
        for e in entries.iter_mut().filter(|e| e.kind == AppKind::Shortcut) {
            if let Some((target, has_args)) = launch::shortcut_details(&e.target) {
                e.aliases = shortcut_alias(&e.name, &target).into_iter().collect();
                if Path::new(&target).extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")) {
                    found.exes.insert(e.target.clone(), target);
                }
                if has_args {
                    found.with_args.insert(e.target.clone());
                }
            }
        }
    });
    found
}

/// What reading the shortcuts yields besides aliases.
#[derive(Debug, Default)]
pub struct Targets {
    /// Each shortcut's exe, by shortcut path.
    pub exes: HashMap<String, String>,
    /// Shortcuts that pass command-line arguments.
    pub with_args: HashSet<String>,
}

/// Prefix of a Store app's target; the rest is its AppUserModelID.
const STORE_PREFIX: &str = "shell:AppsFolder\\";

/// Where the uninstall list is read: the registry, except in tests, which
/// must never touch it.
fn real_programs() -> Arc<dyn ProgramSource> {
    #[cfg(not(test))]
    {
        Arc::new(crate::uninstall::RegistryPrograms)
    }
    #[cfg(test)]
    {
        Arc::new(NoPrograms)
    }
}

#[cfg(test)]
struct NoPrograms;

#[cfg(test)]
impl ProgramSource for NoPrograms {
    fn programs(&self) -> Vec<Program> {
        Vec::new()
    }
}

/// The folder Flick runs from, or empty when unknown (then no uninstall is offered).
#[cfg(not(test))]
fn own_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_default()
}

#[cfg(not(test))]
fn windows_dir() -> PathBuf {
    std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| r"C:\Windows".into())
}

/// Uninstall plan per app target. Pure: the programs are passed in.
fn plans_for(
    entries: &[AppEntry],
    targets: &Targets,
    programs: &[Program],
    windows_dir: &Path,
    own_dir: &Path,
    broad: &[PathBuf],
) -> HashMap<String, Uninstall> {
    if own_dir.as_os_str().is_empty() {
        return HashMap::new();
    }
    entries
        .iter()
        .filter_map(|e| {
            let (exe, store_id) = match e.kind {
                AppKind::Shortcut => (targets.exes.get(&e.target).map(Path::new), None),
                AppKind::Store => (None, Some(e.target.strip_prefix(STORE_PREFIX).unwrap_or(&e.target))),
            };
            plan(&e.name, targets.with_args.contains(&e.target), exe, store_id, programs, windows_dir, own_dir, broad).map(|p| (e.target.clone(), p))
        })
        .collect()
}

/// The open windows of an app: for a shortcut those of its exe, for a Store
/// app those carrying its AppUserModelID.
fn windows_of(e: &AppEntry, exes: &HashMap<String, String>, list: &WindowList, snap: &[WinInfo]) -> Vec<WinInfo> {
    match e.kind {
        AppKind::Shortcut => match exes.get(&e.target) {
            Some(exe) => snap.iter().filter(|w| w.exe.eq_ignore_ascii_case(exe)).cloned().collect(),
            None => Vec::new(),
        },
        AppKind::Store => match e.target.strip_prefix(STORE_PREFIX) {
            Some(id) => snap
                .iter()
                .filter(|w| list.aumid(w.hwnd).is_some_and(|a| a.eq_ignore_ascii_case(id)))
                .cloned()
                .collect(),
            None => Vec::new(),
        },
    }
}

/// One entry per normalised name. `Shortcut` beats `Store`; otherwise the
/// earlier entry wins (callers pass the per-user root before all-users). The
/// kept entry also takes the dropped one's aliases.
pub fn dedupe(entries: Vec<AppEntry>) -> Vec<AppEntry> {
    let mut seen: HashMap<String, usize> = HashMap::with_capacity(entries.len());
    let mut out: Vec<AppEntry> = Vec::with_capacity(entries.len());
    for mut e in entries {
        match seen.entry(fuzzy::normalize(&e.name)) {
            Entry::Occupied(slot) => {
                let kept = &mut out[*slot.get()];
                if kept.kind == AppKind::Store && e.kind == AppKind::Shortcut {
                    std::mem::swap(kept, &mut e);
                }
                for alias in e.aliases {
                    if !kept.aliases.contains(&alias) {
                        kept.aliases.push(alias);
                    }
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(out.len());
                out.push(e);
            }
        }
    }
    out
}

fn is_skipped(name: &str) -> bool {
    let name = fuzzy::normalize(name);
    fuzzy::words(&name).iter().any(|w| SKIP_WORDS.contains(w))
}

/// Recursively collect `*.lnk` under each root, in root order. Unreadable
/// directories are skipped.
pub fn scan_start_menu(roots: &[PathBuf]) -> Vec<AppEntry> {
    let mut out = Vec::new();
    for root in roots {
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(read) = std::fs::read_dir(&dir) else { continue };
            for entry in read.flatten() {
                let Ok(ft) = entry.file_type() else { continue };
                let path = entry.path();
                if ft.is_dir() {
                    stack.push(path);
                    continue;
                }
                let is_lnk = path
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("lnk"));
                if !is_lnk {
                    continue;
                }
                let (Some(stem), Some(target)) =
                    (path.file_stem().and_then(|s| s.to_str()), path.to_str())
                else {
                    continue;
                };
                if is_skipped(stem) {
                    continue;
                }
                out.push(AppEntry {
                    name: stem.to_string(),
                    target: target.to_string(),
                    kind: AppKind::Shortcut,
                    aliases: Vec::new(),
                });
            }
        }
    }
    out
}

/// Windows PowerShell by absolute path, so no `powershell.exe` earlier on
/// `PATH` (or in the working directory) is run instead.
fn powershell_exe() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
}

/// Packaged (Store) apps from `Get-StartApps`. `None` when PowerShell fails or
/// returns no packaged apps (Windows 11 always has some), so the caller can
/// keep its last good list. Also `None` (PowerShell killed) when it has not
/// finished within `STORE_SCAN_TIMEOUT`.
pub fn scan_store_apps() -> Option<Vec<AppEntry>> {
    let stdout = process::run_hidden(
        &powershell_exe(),
        &["-NoProfile", "-NonInteractive", "-Command", START_APPS_SCRIPT],
        STORE_SCAN_TIMEOUT,
    )
    .ok()?;
    let apps = parse_start_apps(&String::from_utf8_lossy(&stdout));
    (!apps.is_empty()).then_some(apps)
}

/// Parse `ConvertTo-Json` output: an array, or a bare object when there is
/// exactly one app. Only AppIDs containing `!` (packaged apps) are kept.
fn parse_start_apps(json: &str) -> Vec<AppEntry> {
    let json = json.trim_start_matches('\u{feff}').trim();
    let items = match serde_json::from_str::<serde_json::Value>(json) {
        Ok(serde_json::Value::Array(items)) => items,
        Ok(obj @ serde_json::Value::Object(_)) => vec![obj],
        _ => return Vec::new(),
    };
    items
        .iter()
        .filter_map(|item| {
            let name = item.get("Name")?.as_str()?.trim();
            let app_id = item.get("AppID")?.as_str()?;
            (!name.is_empty() && app_id.contains('!')).then(|| AppEntry {
                name: name.to_string(),
                target: format!("shell:AppsFolder\\{app_id}"),
                kind: AppKind::Store,
                aliases: store_aliases(name, app_id),
            })
        })
        .collect()
}

/// Per-user Start menu first, then all-users (so per-user wins in `dedupe`).
fn start_menu_roots() -> Vec<PathBuf> {
    ["APPDATA", "ProgramData"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|base| PathBuf::from(base).join(r"Microsoft\Windows\Start Menu\Programs"))
        .collect()
}

pub struct AppsProvider {
    index: RwLock<Vec<AppEntry>>,
    /// Last good Store list, kept when a `Get-StartApps` run fails.
    store: Mutex<Vec<AppEntry>>,
    /// Held for a whole refresh, so overlapping refreshes queue.
    refreshing: Mutex<()>,
    /// Shows a shortcut in Explorer.
    launcher: Arc<dyn Launcher>,
    /// Set once the first scan has been swapped in.
    scanned: AtomicBool,
    /// How each app is uninstalled; swapped by `refresh`, read by queries.
    plans: RwLock<HashMap<String, Uninstall>>,
    /// Each shortcut's exe, from the last refresh.
    exes: RwLock<HashMap<String, String>>,
    /// The open windows, for `Close`. Without it no Close is offered.
    windows: OnceLock<Arc<WindowList>>,
    /// The uninstall list; read on `refresh` only.
    programs: Arc<dyn ProgramSource>,
    windows_dir: PathBuf,
    own_dir: PathBuf,
    /// Shared folders no single program owns.
    broad: Vec<PathBuf>,
}

/// What building rows needs besides the entry: held for a whole query, so
/// the open windows are listed once, from memory.
struct RowCtx<'a> {
    plans: RwLockReadGuard<'a, HashMap<String, Uninstall>>,
    exes: RwLockReadGuard<'a, HashMap<String, String>>,
    windows: Option<(&'a Arc<WindowList>, Vec<WinInfo>)>,
}

impl RowCtx<'_> {
    /// The row for an app; `score` 0 when rebuilt from an id.
    fn row(&self, e: &AppEntry, score: u32) -> ResultItem {
        let mut actions = match e.kind {
            AppKind::Shortcut => vec![Action::Admin, Action::Reveal],
            AppKind::Store => vec![],
        };
        if self.plans.contains_key(&e.target) {
            actions.push(Action::Uninstall);
        }
        if let Some((list, snap)) = &self.windows {
            if !windows_of(e, &self.exes, list, snap).is_empty() {
                actions.push(Action::Close);
            }
        }
        ResultItem {
            id: e.target.clone(),
            provider: "apps",
            title: e.name.clone(),
            subtitle: "App".into(),
            score,
            // Store apps too: `file:shell:AppsFolder\<AUMID>`, which the icon server resolves.
            icon: format!("file:{}", e.target),
            pinnable: true,
            actions,
            ..Default::default()
        }
    }
}

impl AppsProvider {
    pub fn new(launcher: Arc<dyn Launcher>) -> Arc<Self> {
        Self::build(launcher, Vec::new(), false, real_programs())
    }

    /// Where `Close` looks for open windows. Set once, at startup.
    pub fn set_windows(&self, list: Arc<WindowList>) {
        let _ = self.windows.set(list);
    }

    fn row_ctx(&self) -> RowCtx<'_> {
        RowCtx {
            plans: self.plans.read().unwrap_or_else(|e| e.into_inner()),
            exes: self.exes.read().unwrap_or_else(|e| e.into_inner()),
            windows: self.windows.get().map(|list| (list, list.snapshot())),
        }
    }

    /// An index that counts as scanned already (tests).
    #[cfg(test)]
    pub fn with_entries(e: Vec<AppEntry>) -> Arc<Self> {
        Self::with_launcher(Arc::new(crate::launch::testing::RecordingLauncher::default()), e)
    }

    #[cfg(test)]
    pub fn with_launcher(launcher: Arc<dyn Launcher>, e: Vec<AppEntry>) -> Arc<Self> {
        Self::build(launcher, e, true, real_programs())
    }

    /// As `with_launcher`, with the uninstall list read from `programs` on refresh (tests).
    #[cfg(test)]
    pub fn with_source(launcher: Arc<dyn Launcher>, e: Vec<AppEntry>, programs: Arc<dyn ProgramSource>) -> Arc<Self> {
        Self::build(launcher, e, true, programs)
    }

    /// Entries with their exes and the uninstall list already applied; the
    /// Windows folder is `C:\Windows` and Flick lives in `C:\Apps\Flick` (tests).
    #[cfg(test)]
    pub fn with_programs(
        launcher: Arc<dyn Launcher>,
        entries: Vec<AppEntry>,
        exes: Vec<(&str, &str)>,
        programs: Vec<Program>,
    ) -> Arc<Self> {
        let p = Self::build(launcher, entries, true, Arc::new(NoPrograms));
        let targets = Targets {
            exes: exes.into_iter().map(|(l, e)| (l.to_string(), e.to_string())).collect(),
            with_args: HashSet::new(),
        };
        let index = p.index.read().unwrap_or_else(|e| e.into_inner()).clone();
        *p.plans.write().unwrap_or_else(|e| e.into_inner()) =
            plans_for(&index, &targets, &programs, &p.windows_dir, &p.own_dir, &p.broad);
        *p.exes.write().unwrap_or_else(|e| e.into_inner()) = targets.exes;
        p
    }

    fn build(
        launcher: Arc<dyn Launcher>,
        e: Vec<AppEntry>,
        scanned: bool,
        programs: Arc<dyn ProgramSource>,
    ) -> Arc<Self> {
        #[cfg(test)]
        let (windows_dir, own_dir, broad) = (
            PathBuf::from(r"C:\Windows"),
            PathBuf::from(r"C:\Apps\Flick"),
            uninstall::broad_roots_from(|v| match v {
                "ProgramFiles" => Some(r"C:\Program Files".to_string()),
                "LOCALAPPDATA" => Some(r"C:\Users\someone\AppData\Local".to_string()),
                _ => None,
            }),
        );
        #[cfg(not(test))]
        let (windows_dir, own_dir, broad) = (windows_dir(), own_dir(), uninstall::broad_roots());
        Arc::new(Self {
            plans: RwLock::new(HashMap::new()),
            exes: RwLock::new(HashMap::new()),
            windows: OnceLock::new(),
            programs,
            windows_dir,
            own_dir,
            broad,
            index: RwLock::new(e),
            store: Mutex::new(Vec::new()),
            refreshing: Mutex::new(()),
            launcher,
            scanned: AtomicBool::new(scanned),
        })
    }

    /// Scan the Start menu (fast) and publish it at once, then read the
    /// shortcuts' targets for their aliases and publish again, then merge the
    /// Store apps when PowerShell answers (slow, seconds at login).
    pub fn refresh(&self) {
        let _queue = self.refreshing.lock().unwrap_or_else(|e| e.into_inner());
        let start = scan_start_menu(&start_menu_roots());
        self.refresh_locked(start, add_shortcut_aliases, scan_store_apps);
    }

    /// `refresh` with every stage passed in (tests).
    #[cfg(test)]
    fn refresh_from_stages(
        &self,
        start: Vec<AppEntry>,
        add_aliases: impl FnOnce(&mut [AppEntry]) -> Targets,
        scan_store: impl FnOnce() -> Option<Vec<AppEntry>>,
    ) {
        let _queue = self.refreshing.lock().unwrap_or_else(|e| e.into_inner());
        self.refresh_locked(start, add_aliases, scan_store);
    }

    /// `refresh` with the scans passed in and no shortcut aliases (tests).
    #[cfg(test)]
    fn refresh_from(&self, start: Vec<AppEntry>, scan_store: impl FnOnce() -> Option<Vec<AppEntry>>) {
        self.refresh_from_stages(start, |_| Targets::default(), scan_store);
    }

    /// `start` is published right away together with the last good Store list,
    /// each shortcut keeping the aliases the current index has for its target;
    /// `add_aliases` then fills the shortcut aliases (and returns their exes),
    /// the uninstall list is read once and `start` is published again;
    /// `scan_store` then runs, and its result replaces that list unless
    /// it is `None`. Each publish swaps the whole index, so queries never see
    /// a partial one. The caller holds `refreshing`.
    fn refresh_locked(
        &self,
        mut start: Vec<AppEntry>,
        add_aliases: impl FnOnce(&mut [AppEntry]) -> Targets,
        scan_store: impl FnOnce() -> Option<Vec<AppEntry>>,
    ) {
        {
            let index = self.index.read().unwrap_or_else(|e| e.into_inner());
            let known: HashMap<&str, &Vec<String>> =
                index.iter().filter(|e| !e.aliases.is_empty()).map(|e| (e.target.as_str(), &e.aliases)).collect();
            for e in start.iter_mut().filter(|e| e.aliases.is_empty()) {
                if let Some(aliases) = known.get(e.target.as_str()) {
                    e.aliases = (*aliases).clone();
                }
            }
        }
        let last_store = self.store.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.publish(&start, &last_store);
        let targets = add_aliases(&mut start);
        let programs = self.programs.programs();
        self.set_plans(&start, &last_store, &targets, &programs);
        *self.exes.write().unwrap_or_else(|e| e.into_inner()) = targets.exes.clone();
        self.publish(&start, &last_store);
        if let Some(fresh) = scan_store() {
            self.set_plans(&start, &fresh, &targets, &programs);
            self.publish(&start, &fresh);
            *self.store.lock().unwrap_or_else(|e| e.into_inner()) = fresh;
        }
        // Ready only after the Store attempt, so a pinned Store app is not
        // reported missing while the first Store scan is still running.
        self.scanned.store(true, Ordering::SeqCst);
    }

    fn set_plans(&self, start: &[AppEntry], store: &[AppEntry], targets: &Targets, programs: &[Program]) {
        let all: Vec<AppEntry> = start.iter().chain(store).cloned().collect();
        let plans = plans_for(&all, targets, programs, &self.windows_dir, &self.own_dir, &self.broad);
        *self.plans.write().unwrap_or_else(|e| e.into_inner()) = plans;
    }

    fn publish(&self, start: &[AppEntry], store: &[AppEntry]) {
        let fresh = dedupe(start.iter().chain(store).cloned().collect());
        *self.index.write().unwrap_or_else(|e| e.into_inner()) = fresh;
    }
}

#[derive(Debug, PartialEq)]
enum Launch {
    Open,
    Uri,
    Elevated,
}

fn launch_kind(kind: &AppKind, modifier: Modifier) -> Launch {
    match (kind, modifier) {
        (_, Modifier::CtrlShift) => Launch::Elevated,
        (AppKind::Store, Modifier::None | Modifier::Ctrl) => Launch::Uri,
        (AppKind::Shortcut, Modifier::None | Modifier::Ctrl) => Launch::Open,
    }
}

/// A Store app that cannot be elevated must say so (never fall back to a plain
/// open). A cancelled UAC prompt stays `CANCELLED`, which the bar ignores.
fn store_elevation_error(err: String) -> String {
    if err == launch::CANCELLED {
        err
    } else {
        "this app cannot be run as administrator".into()
    }
}

impl AppsProvider {
    fn entry(&self, id: &str) -> Option<AppEntry> {
        self.index.read().unwrap_or_else(|e| e.into_inner()).iter().find(|e| e.target == id).cloned()
    }

    /// Runs the app's registered uninstaller, or opens Installed apps. The
    /// command is split into program and parameters for the launcher, never
    /// handed to a shell. Locks are released before launching.
    fn uninstall(&self, id: &str) -> Result<Outcome, String> {
        const NO: &str = "can't uninstall this";
        self.entry(id).ok_or_else(|| NO.to_string())?;
        let plan = self.plans.read().unwrap_or_else(|e| e.into_inner()).get(id).cloned();
        match plan.ok_or_else(|| NO.to_string())? {
            Uninstall::Direct { command } => {
                let (program, params) = split_command(&command).ok_or_else(|| NO.to_string())?;
                self.launcher.open_with(&program, &params, false)?;
            }
            Uninstall::Settings => self.launcher.open(uninstall::APPS_SETTINGS)?,
        }
        Ok(Outcome::Hide)
    }

    /// Asks every window of the app to close, skipping elevated ones.
    fn close(&self, id: &str) -> Result<Outcome, String> {
        const GONE: &str = "That window is gone.";
        let list = self.windows.get().ok_or_else(|| GONE.to_string())?;
        let entry = self.entry(id).ok_or_else(|| GONE.to_string())?;
        let wins = {
            let exes = self.exes.read().unwrap_or_else(|e| e.into_inner());
            windows_of(&entry, &exes, list, &list.snapshot())
        };
        let closed = wins.iter().filter(|w| !w.elevated).filter(|w| list.ops().close(w.hwnd, w.pid).is_ok()).count();
        if closed > 0 {
            Ok(Outcome::Hide)
        } else if wins.iter().any(|w| w.elevated) {
            Err("Needs administrator".to_string())
        } else {
            Err(GONE.to_string())
        }
    }
}

impl Provider for AppsProvider {
    fn name(&self) -> &'static str {
        "apps"
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        let needle = fuzzy::normalize(text);
        let ctx = self.row_ctx();
        let index = self.index.read().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<ResultItem> = index
            .iter()
            .filter_map(|e| {
                // Aliases are stored normalised: no allocation per alias.
                let by_alias = e
                    .aliases
                    .iter()
                    .filter_map(|a| fuzzy::score_prenormalized(&needle, a))
                    .max()
                    .map(|s| s.saturating_sub(ALIAS_PENALTY));
                let score = fuzzy::score(text, &e.name).max(by_alias)?;
                Some(ctx.row(e, score))
            })
            .collect();
        out.sort_by(|a, b| b.score.cmp(&a.score));
        out
    }

    fn item(&self, id: &str) -> Option<ResultItem> {
        let ctx = self.row_ctx();
        let index = self.index.read().unwrap_or_else(|e| e.into_inner());
        index.iter().find(|e| e.target == id).map(|e| ctx.row(e, 0))
    }

    fn ready(&self) -> bool {
        self.scanned.load(Ordering::SeqCst)
    }

    /// Reveal (a Start menu shortcut only), Uninstall and Close.
    fn act(&self, id: &str, action: Action) -> Result<Outcome, String> {
        match action {
            Action::Reveal => {}
            Action::Uninstall => return self.uninstall(id),
            Action::Close => return self.close(id),
            _ => return Err("can't do that here".into()),
        }
        let shortcut = {
            let index = self.index.read().unwrap_or_else(|e| e.into_inner());
            index.iter().any(|e| e.target == id && e.kind == AppKind::Shortcut)
        };
        if !shortcut {
            return Err("can't show this item's location".into());
        }
        self.launcher.reveal(id)?;
        Ok(Outcome::Hide)
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: "blender".into(),
            about: "open an app by name; Ctrl+Shift+Enter runs it as administrator".into(),
            fill: String::new(),
        }]
    }

    fn activate(&self, id: &str, modifier: Modifier) -> Result<Outcome, String> {
        // Copy out and release the lock before launching: a UAC prompt blocks.
        let kind = {
            let index = self.index.read().unwrap_or_else(|e| e.into_inner());
            index
                .iter()
                .find(|e| e.target == id)
                .map(|e| e.kind.clone())
                .ok_or_else(|| "app no longer exists".to_string())?
        };
        match (launch_kind(&kind, modifier), kind) {
            (Launch::Open, _) => launch::open(id)?,
            (Launch::Uri, _) => launch::open_uri(id)?,
            (Launch::Elevated, AppKind::Store) => launch::open_elevated(id).map_err(store_elevation_error)?,
            (Launch::Elevated, AppKind::Shortcut) => launch::open_elevated(id)?,
        }
        Ok(Outcome::Hide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Action;
    use std::fs;

    fn shortcut(name: &str, target: &str) -> AppEntry {
        AppEntry {
            name: name.into(),
            target: target.into(),
            kind: AppKind::Shortcut,
            aliases: Vec::new(),
        }
    }

    fn with_aliases(mut e: AppEntry, aliases: &[&str]) -> AppEntry {
        e.aliases = aliases.iter().map(|a| a.to_string()).collect();
        e
    }

    /// As `parse_start_apps` builds it, alias included.
    fn store(name: &str, app_id: &str) -> AppEntry {
        AppEntry {
            name: name.into(),
            target: format!("shell:AppsFolder\\{app_id}"),
            kind: AppKind::Store,
            aliases: store_aliases(name, app_id),
        }
    }

    #[test]
    fn item_rebuilds_a_known_app() {
        let rec = Arc::new(crate::launch::testing::RecordingLauncher::default());
        let p = AppsProvider::with_launcher(
            rec.clone(),
            vec![shortcut("Blender", r"C:\b.lnk"), store("Calc", "Microsoft.WindowsCalculator_x!App")],
        );
        let r = p.item(r"C:\b.lnk").unwrap();
        assert_eq!((r.title.as_str(), r.icon.as_str(), r.score), ("Blender", r"file:C:\b.lnk", 0));
        assert!(r.pinnable);
        assert_eq!(r.actions, [Action::Admin, Action::Reveal]);
        assert!(p.item("nope").is_none());
        let s = p.item(r"shell:AppsFolder\Microsoft.WindowsCalculator_x!App").unwrap();
        assert_eq!(s.icon, r"file:shell:AppsFolder\Microsoft.WindowsCalculator_x!App");
        assert!(s.pinnable);
        assert_eq!(s.actions, []);
        assert_eq!(p.act(r"C:\b.lnk", Action::Reveal), Ok(Outcome::Hide));
        assert!(p.act(&s.id, Action::Reveal).is_err());
        assert!(p.act(r"C:\b.lnk", Action::CopyPath).is_err());
        assert_eq!(rec.calls(), [r"reveal C:\b.lnk".to_string()]);
        assert!(p.ready());
        assert!(!AppsProvider::new(rec).ready());
    }

    #[test]
    fn ready_only_after_the_store_scan_attempt() {
        let rec = Arc::new(crate::launch::testing::RecordingLauncher::default());
        for store_result in [Some(vec![store("Calc", "x!a")]), None] {
            let p = AppsProvider::new(rec.clone());
            let p2 = p.clone();
            p.refresh_from(vec![shortcut("Blender", r"C:\b.lnk")], move || {
                // Start menu published (twice), Store scan still running.
                assert_eq!(p2.query(&q("blender")).len(), 1);
                assert!(!p2.ready());
                store_result
            });
            assert!(p.ready());
        }
    }

    #[test]
    fn store_alias_from_app_id() {
        let calc = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
        assert_eq!(store_alias(calc), Some("Calculator"));
        assert_eq!(store_alias("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"), Some("Terminal"));
        // A localised name: `calc` still finds it, under its own title, as an
        // alias prefix match.
        let p = AppsProvider::with_entries(vec![store("Rekenmachine", calc)]);
        let out = p.query(&q("calc"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Rekenmachine");
        assert_eq!(out[0].id, format!("shell:AppsFolder\\{calc}"));
        assert_eq!(out[0].score, 700);
    }

    #[test]
    fn store_alias_skips_channel_words_and_vendor_prefix() {
        assert_eq!(store_alias("Microsoft.MicrosoftEdge.Stable_8wekyb3d8bbwe!App"), Some("Edge"));
        assert_eq!(store_alias("Microsoft.Office.Desktop_x!Word"), Some("Office"));
        assert_eq!(store_alias("Microsoft.Windows.Photos_x!App"), Some("Photos"));
        assert_eq!(store_alias("Contoso.WINDOWSthing_x!App"), Some("thing"));
        // Only the vendor word: nothing left.
        assert_eq!(store_alias("Microsoft.Windows_x!App"), None);
        assert_eq!(store_alias("Contoso.Microsoft_x!App"), None);
        // Not followed by a letter: kept whole.
        assert_eq!(store_alias("Contoso.Windows11Tool_x!App"), Some("Windows11Tool"));
    }

    #[test]
    fn windows_package_aliases_do_not_bury_name_matches() {
        let mut entries: Vec<AppEntry> = [
            ("Rekenmachine", "WindowsCalculator"),
            ("Caméra", "WindowsCamera"),
            ("Horloge", "WindowsAlarms"),
            ("Cartes", "WindowsMaps"),
            ("Bloc-notes", "WindowsNotepad"),
            ("Enregistreur vocal", "WindowsSoundRecorder"),
            ("Hub de commentaires", "WindowsFeedbackHub"),
            ("Photos", "WindowsPhotos"),
            ("Pense-bêtes", "WindowsStickyNotes"),
            ("Invite de commandes", "WindowsTerminal"),
        ]
        .iter()
        .map(|(name, pkg)| store(name, &format!("Microsoft.{pkg}_8wekyb3d8bbwe!App")))
        .collect();
        entries.push(shortcut("Remote Win-Kit", r"C:\kit.lnk"));
        let p = AppsProvider::with_entries(entries);
        let out = p.query(&q("win"));
        assert_eq!(out[0].title, "Remote Win-Kit");
        assert!(out[1..].iter().all(|r| r.score < out[0].score), "{out:?}");
    }

    #[test]
    fn dedupe_merges_aliases_of_the_dropped_entry() {
        let out = dedupe(vec![
            with_aliases(store("Notepad", "x!a"), &["notepadstore", "shared"]),
            with_aliases(shortcut("Notepad", r"C:\n.lnk"), &["shared", "np"]),
        ]);
        let merged = with_aliases(shortcut("Notepad", r"C:\n.lnk"), &["shared", "np", "notepadstore"]);
        assert_eq!(out, vec![merged]);
        let out = dedupe(vec![
            with_aliases(shortcut("Blender", r"C:\b.lnk"), &["blenderexe"]),
            with_aliases(store("Blender", "x!b"), &["blenderstore", "blenderexe"]),
        ]);
        let merged = with_aliases(shortcut("Blender", r"C:\b.lnk"), &["blenderexe", "blenderstore"]);
        assert_eq!(out, vec![merged]);
    }

    #[test]
    fn refresh_publishes_start_menu_before_reading_shortcut_aliases() {
        let p = AppsProvider::with_entries(vec![]);
        let (p2, p3) = (p.clone(), p.clone());
        p.refresh_from_stages(
            vec![shortcut("Word", r"C:\w.lnk")],
            move |start| {
                // First publish already happened, without aliases.
                assert_eq!(p2.query(&q("word")).len(), 1);
                assert!(p2.query(&q("winword")).is_empty());
                start[0].aliases = vec!["winword".into()];
                Targets::default()
            },
            move || {
                // Aliases published before the Store scan returns.
                assert_eq!(p3.query(&q("winword")).len(), 1);
                Some(vec![store("Rekenmachine", "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App")])
            },
        );
        assert_eq!(p.query(&q("winword")).len(), 1);
        assert_eq!(p.query(&q("calc")).len(), 1);
    }

    #[test]
    fn a_later_refresh_keeps_answering_aliases_while_links_are_read_again() {
        let p = AppsProvider::with_entries(vec![]);
        let add = |start: &mut [AppEntry]| {
            start[0].aliases = vec!["winword".into()];
            Targets::default()
        };
        p.refresh_from_stages(vec![shortcut("Word", r"C:\w.lnk")], add, || None);
        assert_eq!(p.query(&q("winword")).len(), 1);
        let p2 = p.clone();
        p.refresh_from_stages(
            vec![shortcut("Word", r"C:\w.lnk"), shortcut("Excel", r"C:\x.lnk")],
            move |start| {
                // The first publish of this refresh kept the alias found last time.
                assert_eq!(p2.query(&q("winword")).len(), 1);
                start[0].aliases = vec!["winword".into()];
                Targets::default()
            },
            || None,
        );
        assert_eq!(p.query(&q("winword")).len(), 1);
    }

    #[test]
    fn channel_word_left_after_the_vendor_is_no_alias() {
        assert_eq!(store_alias("Contoso.WindowsApp_x!App"), None);
        assert_eq!(store_alias("Contoso.MicrosoftStore_x!App"), None);
    }

    #[test]
    fn aliases_shorter_than_three_chars_are_dropped() {
        assert_eq!(shortcut_alias("Some Tool", r"C:\x\st.exe"), None);
        assert_eq!(shortcut_alias("Some Tool", r"C:\x\q.exe"), None);
        assert!(store_aliases("Some App", "Contoso.QX_x!App").is_empty());
        assert_eq!(shortcut_alias("Some Tool", r"C:\x\stl.exe"), Some("stl".to_string()));
    }

    #[test]
    fn store_alias_absent_for_odd_app_id() {
        for id in [
            "DC5C6510.2032887045529_2v019pwa6amcg!Agilebits.OnePassword", // all digits
            "Contoso.ABC123DEF_x!App",                                     // hex-looking
            "Microsoft.Office.MSACCESS.EXE.15",                            // not packaged
            "NoPublisherHash!App",                                         // no `_`
            "Contoso._x!App",                                              // empty segment
        ] {
            assert_eq!(store_alias(id), None, "{id}");
        }
        let one_password = "DC5C6510.2032887045529_2v019pwa6amcg!Agilebits.OnePassword";
        assert!(store_aliases("1Password", one_password).is_empty());
        // A package without a dot is its own segment.
        assert_eq!(store_alias("Spotify_x!App"), Some("Spotify"));
    }

    #[test]
    fn shortcut_alias_from_target_stem() {
        let word = r"C:\Program Files\Microsoft Office\root\Office16\WINWORD.EXE";
        assert_eq!(shortcut_alias("Word", word), Some("winword".to_string()));
        // The name already has it.
        assert_eq!(shortcut_alias("Visual Studio Code", r"C:\VS Code\Code.exe"), None);
        assert_eq!(shortcut_alias("Some Doc", r"C:\docs\manual.pdf"), None); // not an .exe
        assert_eq!(shortcut_alias("Some Tool", ""), None);
        // Generic hosts: many unrelated shortcuts point at them.
        for host in [
            r"C:\Windows\System32\cmd.exe",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        ] {
            assert_eq!(shortcut_alias("Developer Prompt", host), None, "{host}");
        }
        assert_eq!(shortcut_alias("Discord", r"C:\Users\x\AppData\Local\Discord\Update.exe"), None);
        let word = with_aliases(shortcut("Word", r"C:\w.lnk"), &["winword"]);
        let p = AppsProvider::with_entries(vec![word]);
        assert_eq!(p.query(&q("winword"))[0].title, "Word");
    }

    #[test]
    fn alias_equal_to_name_is_dropped() {
        assert_eq!(shortcut_alias("Blender", r"C:\Program Files\Blender\blender.exe"), None);
        assert_eq!(shortcut_alias("Notepad++", r"C:\Program Files\Notepad++\NOTEPAD++.EXE"), None);
        let terminal = "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App";
        assert!(store_aliases("Terminal", terminal).is_empty());
        // Spaces and separators ignored.
        assert!(store_aliases("Contoso Terminal", "Contoso.ContosoTerminal_x!App").is_empty());
        assert!(store_aliases("Notepad Pro", "Contoso.Note-Pad_x!App").is_empty());
        // Kept when the name does not contain it.
        let music = store_aliases("Spotify", "SpotifyAB.SpotifyMusic_x!Spotify");
        assert_eq!(music, vec!["spotifymusic".to_string()]);
    }

    #[test]
    fn alias_match_scores_below_name_match() {
        let p = AppsProvider::with_entries(vec![
            with_aliases(shortcut("Word", r"C:\w.lnk"), &["winword"]),
            shortcut("WinWord", r"C:\ww.lnk"),
        ]);
        let out = p.query(&q("winword"));
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].title.as_str(), out[0].score), ("WinWord", 1000));
        assert_eq!((out[1].title.as_str(), out[1].score), ("Word", 1000 - ALIAS_PENALTY));
        // The name score wins when it is the better one.
        assert_eq!(p.query(&q("word"))[0].title, "Word");
        assert_eq!(p.query(&q("word"))[0].score, 1000);
    }

    #[test]
    fn parse_start_apps_fills_aliases() {
        let json = r#"[
            {"Name": "Rekenmachine", "AppID": "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"},
            {"Name": "1Password", "AppID": "DC5C6510.2032887045529_2v019pwa6amcg!Agilebits.OnePassword"}
        ]"#;
        let out = parse_start_apps(json);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].aliases, vec!["calculator".to_string()]);
        assert!(out[1].aliases.is_empty());
    }

    #[test]
    fn has_help_line() {
        let help = AppsProvider::with_entries(vec![]).help();
        assert_eq!(help.len(), 1);
        assert_eq!(help[0].example, "blender");
        assert_eq!(help[0].about, "open an app by name; Ctrl+Shift+Enter runs it as administrator");
        assert_eq!(help[0].fill, "");
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    #[test]
    fn dedupe_same_name_across_roots_keeps_one() {
        let out = dedupe(vec![
            shortcut("Blender", r"C:\user\Blender.lnk"),
            shortcut("blender", r"C:\common\Blender.lnk"),
        ]);
        assert_eq!(out, vec![shortcut("Blender", r"C:\user\Blender.lnk")]);
    }

    #[test]
    fn dedupe_prefers_shortcut_over_store() {
        let out = dedupe(vec![
            store("Notepad", "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App"),
            shortcut("Notepad", r"C:\user\Notepad.lnk"),
        ]);
        assert_eq!(out, vec![shortcut("Notepad", r"C:\user\Notepad.lnk")]);
    }

    #[test]
    fn dedupe_keeps_distinct_names_in_order() {
        let out = dedupe(vec![shortcut("A", "a"), store("B", "x!b"), shortcut("C", "c")]);
        let names: Vec<_> = out.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["A", "B", "C"]);
    }

    #[test]
    fn scan_finds_nested_lnk_and_uses_stem_as_name() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        let lnk = nested.join("Blender.lnk");
        fs::write(&lnk, b"").unwrap();
        fs::write(nested.join("notes.txt"), b"").unwrap();

        let out = scan_start_menu(&[dir.path().to_path_buf()]);
        assert_eq!(out, vec![shortcut("Blender", lnk.to_str().unwrap())]);
    }

    #[test]
    fn scan_skips_uninstallers() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["Uninstall Foo.lnk", "Foo README.lnk", "Foo Help.lnk", "Foo.lnk"] {
            fs::write(dir.path().join(f), b"").unwrap();
        }
        let out = scan_start_menu(&[dir.path().to_path_buf()]);
        let names: Vec<_> = out.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Foo"]);
    }

    #[test]
    fn scan_missing_root_is_empty() {
        let out = scan_start_menu(&[PathBuf::from(r"C:\definitely\missing\root")]);
        assert!(out.is_empty());
    }

    #[test]
    fn parse_start_apps_array_keeps_only_packaged() {
        let json = r#"[
            {"Name": "1Password", "AppID": "DC5C6510.2032887045529_2v019pwa6amcg!Agilebits.OnePassword"},
            {"Name": "Access", "AppID": "Microsoft.Office.MSACCESS.EXE.15"}
        ]"#;
        assert_eq!(
            parse_start_apps(json),
            vec![store("1Password", "DC5C6510.2032887045529_2v019pwa6amcg!Agilebits.OnePassword")]
        );
    }

    #[test]
    fn parse_start_apps_single_object() {
        let json = "\u{feff}{\"Name\": \"Calculator\", \"AppID\": \"Microsoft.WindowsCalculator_8wekyb3d8bbwe!App\"}\r\n";
        assert_eq!(
            parse_start_apps(json),
            vec![store("Calculator", "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App")]
        );
    }

    #[test]
    fn parse_start_apps_empty_or_garbage_is_empty() {
        assert!(parse_start_apps("").is_empty());
        assert!(parse_start_apps("Get-StartApps : not recognized").is_empty());
        assert!(parse_start_apps("42").is_empty());
        assert!(parse_start_apps(r#"[{"Name": null, "AppID": "a!b"}, {"AppID": "c!d"}]"#).is_empty());
    }

    #[test]
    fn query_bl_finds_blender() {
        let p = AppsProvider::with_entries(vec![
            shortcut("Tablet Tool", r"C:\t.lnk"),
            shortcut("Blender", r"C:\b.lnk"),
            shortcut("Calculator", r"C:\c.lnk"),
        ]);
        let out = p.query(&q("bl"));
        assert_eq!(out[0].title, "Blender");
        assert_eq!(out[0].id, r"C:\b.lnk");
        assert_eq!(out[0].provider, "apps");
        assert_eq!(out[0].subtitle, "App");
        assert!(!out[0].confirm);
        assert!(out.iter().all(|r| r.title != "Calculator"));
    }

    #[test]
    fn query_empty_text_returns_nothing() {
        let p = AppsProvider::with_entries(vec![shortcut("Blender", r"C:\b.lnk")]);
        assert!(p.query(&q("")).is_empty());
        assert!(p.query(&q("   ")).is_empty());
    }

    #[test]
    fn activate_unknown_id_is_err() {
        let p = AppsProvider::with_entries(vec![shortcut("Blender", r"C:\b.lnk")]);
        assert_eq!(
            p.activate(r"C:\nope.lnk", Modifier::None),
            Err("app no longer exists".to_string())
        );
    }

    #[test]
    fn activate_missing_target_is_err_not_panic() {
        let p = AppsProvider::with_entries(vec![shortcut("Missing", r"C:\definitely\missing.lnk")]);
        assert!(p.activate(r"C:\definitely\missing.lnk", Modifier::None).is_err());
    }

    #[test]
    fn skip_words_match_whole_words_only() {
        let dir = tempfile::tempdir().unwrap();
        for f in [
            "Revo Uninstaller.lnk",
            "HelpNDoc.lnk",
            "Uninstall Foo.lnk",
            "Foo Help.lnk",
            "Foo-Website.lnk",
            "Foo deinstallieren.lnk",
            "Foo Hilfe.lnk",
            "Foo Dokumentation.lnk",
        ] {
            fs::write(dir.path().join(f), b"").unwrap();
        }
        let out = scan_start_menu(&[dir.path().to_path_buf()]);
        let mut names: Vec<_> = out.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, ["HelpNDoc", "Revo Uninstaller"]);
    }

    #[test]
    fn refresh_publishes_start_menu_before_store_scan_returns() {
        let p = AppsProvider::with_entries(vec![]);
        let p2 = p.clone();
        p.refresh_from(vec![shortcut("Blender", r"C:\b.lnk")], move || {
            // Store scan still running: the Start-menu apps are already findable.
            assert_eq!(p2.query(&q("blender")).len(), 1);
            Some(vec![store("Calculator", "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App")])
        });
        assert_eq!(p.query(&q("calc")).len(), 1);
        assert_eq!(p.query(&q("blender")).len(), 1);
    }

    #[test]
    fn failed_store_scan_keeps_last_good_store_apps() {
        let p = AppsProvider::with_entries(vec![]);
        let calc = store("Calculator", "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App");
        p.refresh_from(vec![], || Some(vec![calc.clone()]));
        let p2 = p.clone();
        p.refresh_from(vec![shortcut("Blender", r"C:\b.lnk")], move || {
            // Also kept while the new scan runs.
            assert_eq!(p2.query(&q("calc")).len(), 1);
            None
        });
        assert_eq!(p.query(&q("calc")).len(), 1);
        assert_eq!(p.query(&q("blender")).len(), 1);
        // A later successful scan replaces the list.
        p.refresh_from(vec![], || Some(vec![]));
        assert!(p.query(&q("calc")).is_empty());
    }

    #[test]
    fn overlapping_refreshes_run_one_at_a_time() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let p = AppsProvider::with_entries(vec![]);
        let running = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..3)
            .map(|_| {
                let (p, running, most) = (p.clone(), running.clone(), most.clone());
                std::thread::spawn(move || {
                    p.refresh_from(vec![], || {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        most.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        running.fetch_sub(1, Ordering::SeqCst);
                        Some(vec![])
                    })
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(most.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn powershell_is_called_by_absolute_system32_path() {
        let exe = powershell_exe();
        assert!(exe.is_absolute(), "{}", exe.display());
        assert!(exe.ends_with(r"System32\WindowsPowerShell\v1.0\powershell.exe"), "{}", exe.display());
    }

    #[test]
    fn store_apps_run_elevated_with_ctrl_shift() {
        assert_eq!(launch_kind(&AppKind::Store, Modifier::CtrlShift), Launch::Elevated);
        assert_eq!(launch_kind(&AppKind::Store, Modifier::None), Launch::Uri);
        assert_eq!(launch_kind(&AppKind::Store, Modifier::Ctrl), Launch::Uri);
        assert_eq!(launch_kind(&AppKind::Shortcut, Modifier::CtrlShift), Launch::Elevated);
        assert_eq!(launch_kind(&AppKind::Shortcut, Modifier::None), Launch::Open);
        assert_eq!(launch_kind(&AppKind::Shortcut, Modifier::Ctrl), Launch::Open);
    }

    #[test]
    fn failed_store_elevation_says_so_but_cancel_stays_cancel() {
        assert_eq!(store_elevation_error("access denied".into()), "this app cannot be run as administrator");
        assert_eq!(store_elevation_error(launch::CANCELLED.into()), launch::CANCELLED);
    }

    use crate::launch::testing::RecordingLauncher;
    use crate::uninstall::{Program, ProgramSource};
    use crate::winlist::{WinInfo, WindowList, WindowOps};
    use std::sync::atomic::AtomicUsize;

    const FOO_EXE: &str = r"C:\Program Files\Foo\foo.exe";
    const CALC_ID: &str = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";

    fn foo_program() -> Program {
        Program {
            name: "Foo".into(),
            install_location: Some(r"C:\Program Files\Foo".into()),
            uninstall: Some(r#""C:\Program Files\Foo\unins000.exe" /SILENT"#.into()),
            system: false,
        }
    }

    /// Windows that only record `close`; nothing real is touched.
    struct FakeWins {
        wins: Vec<WinInfo>,
        aumids: HashMap<isize, String>,
        closes: Mutex<Vec<String>>,
    }

    impl WindowOps for FakeWins {
        fn list(&self) -> Vec<WinInfo> {
            self.wins.clone()
        }
        fn exists(&self, _hwnd: isize) -> bool {
            true
        }
        fn focus(&self, _hwnd: isize) -> Result<(), String> {
            Ok(())
        }
        fn close(&self, hwnd: isize, pid: u32) -> Result<(), String> {
            self.closes.lock().unwrap().push(format!("close {hwnd} {pid}"));
            Ok(())
        }
        fn kill(&self, _pid: u32, _exe: &str) -> Result<(), String> {
            Ok(())
        }
        fn aumid(&self, hwnd: isize) -> Option<String> {
            self.aumids.get(&hwnd).cloned()
        }
    }

    fn win(hwnd: isize, pid: u32, exe: &str, elevated: bool) -> WinInfo {
        WinInfo { hwnd, pid, title: "t".into(), exe: exe.into(), app: "a".into(), elevated }
    }

    fn fake_windows(wins: Vec<WinInfo>, aumids: &[(isize, &str)]) -> (Arc<WindowList>, Arc<FakeWins>) {
        let ops = Arc::new(FakeWins {
            wins,
            aumids: aumids.iter().map(|(h, a)| (*h, a.to_string())).collect(),
            closes: Mutex::new(Vec::new()),
        });
        let list = WindowList::new(ops.clone());
        list.refresh();
        (list, ops)
    }

    fn app_with_programs(rec: &Arc<crate::launch::testing::RecordingLauncher>) -> Arc<AppsProvider> {
        AppsProvider::with_programs(
            rec.clone(),
            vec![
                shortcut("Foo", r"C:\Foo.lnk"),
                shortcut("Command Prompt", r"C:\Cmd.lnk"),
                shortcut("Loose", r"C:\Loose.lnk"),
                store("Calculator", CALC_ID),
                store("Settings", "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel"),
            ],
            vec![(r"C:\Foo.lnk", FOO_EXE), (r"C:\Cmd.lnk", r"C:\Windows\System32\cmd.exe")],
            vec![foo_program()],
        )
    }

    fn actions_of(p: &AppsProvider, id: &str) -> Vec<Action> {
        p.item(id).unwrap().actions
    }

    #[test]
    fn app_rows_offer_the_spec_actions() {
        let rec = Arc::new(RecordingLauncher::default());
        let p = app_with_programs(&rec);
        let calc = format!("shell:AppsFolder\\{CALC_ID}");
        // Not running (no window list yet): no Close.
        assert_eq!(actions_of(&p, r"C:\Foo.lnk"), [Action::Admin, Action::Reveal, Action::Uninstall]);
        assert_eq!(actions_of(&p, &calc), [Action::Uninstall]);
        // Windows parts never get Uninstall.
        assert_eq!(actions_of(&p, r"C:\Cmd.lnk"), [Action::Admin, Action::Reveal]);
        let settings = r"shell:AppsFolder\windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";
        assert_eq!(actions_of(&p, settings), []);
        // No exe known: Installed apps.
        assert_eq!(actions_of(&p, r"C:\Loose.lnk"), [Action::Admin, Action::Reveal, Action::Uninstall]);

        let (list, _) = fake_windows(vec![win(1, 10, &FOO_EXE.to_uppercase(), false)], &[]);
        p.set_windows(list);
        assert_eq!(
            actions_of(&p, r"C:\Foo.lnk"),
            [Action::Admin, Action::Reveal, Action::Uninstall, Action::Close]
        );
        assert_eq!(actions_of(&p, r"C:\Loose.lnk"), [Action::Admin, Action::Reveal, Action::Uninstall]);
        assert_eq!(actions_of(&p, &calc), [Action::Uninstall]);
        // Query rows carry the same actions.
        assert_eq!(p.query(&q("foo"))[0].actions.len(), 4);
    }

    #[test]
    fn store_rows_offer_close_when_a_window_has_their_aumid() {
        let rec = Arc::new(RecordingLauncher::default());
        let p = app_with_programs(&rec);
        let (list, _) = fake_windows(vec![win(2, 11, r"C:\Windows\ApplicationFrameHost.exe", false)], &[(2, CALC_ID)]);
        p.set_windows(list);
        let calc = format!("shell:AppsFolder\\{CALC_ID}");
        assert_eq!(actions_of(&p, &calc), [Action::Uninstall, Action::Close]);
        assert_eq!(actions_of(&p, r"C:\Foo.lnk"), [Action::Admin, Action::Reveal, Action::Uninstall]);
    }

    /// A window whose AUMID comes from its process (not a window property) and
    /// differs only in case still makes the Store row offer Close.
    #[test]
    fn store_close_offered_for_process_aumid_ignoring_case() {
        let rec = Arc::new(RecordingLauncher::default());
        let p = app_with_programs(&rec);
        let (list, _) = fake_windows(
            vec![win(3, 12, r"C:\Program Files\WindowsApps\Microsoft.WindowsCalculator\CalculatorApp.exe", false)],
            &[(3, "MICROSOFT.WINDOWSCALCULATOR_8WEKYB3D8BBWE!APP")],
        );
        p.set_windows(list);
        let calc = format!("shell:AppsFolder\\{CALC_ID}");
        assert_eq!(actions_of(&p, &calc), [Action::Uninstall, Action::Close]);
    }

    #[test]
    fn uninstall_runs_the_registered_uninstaller() {
        let rec = Arc::new(RecordingLauncher::default());
        let p = app_with_programs(&rec);
        assert_eq!(p.act(r"C:\Foo.lnk", Action::Uninstall), Ok(Outcome::Hide));
        assert_eq!(rec.calls(), [r"open_with C:\Program Files\Foo\unins000.exe elevated=false: /SILENT".to_string()]);
    }

    #[test]
    fn uninstall_falls_back_to_installed_apps_settings() {
        let rec = Arc::new(RecordingLauncher::default());
        let p = app_with_programs(&rec);
        let calc = format!("shell:AppsFolder\\{CALC_ID}");
        assert_eq!(p.act(&calc, Action::Uninstall), Ok(Outcome::Hide));
        assert_eq!(p.act(r"C:\Loose.lnk", Action::Uninstall), Ok(Outcome::Hide));
        assert_eq!(rec.calls(), ["open ms-settings:appsfeatures".to_string(), "open ms-settings:appsfeatures".to_string()]);
        // No plan, no uninstall; and nothing was launched for it.
        assert!(p.act(r"C:\Cmd.lnk", Action::Uninstall).is_err());
        assert!(p.act(r"C:\Missing.lnk", Action::Uninstall).is_err());
        assert_eq!(rec.calls().len(), 2);
    }

    #[test]
    fn close_closes_every_window_of_the_app() {
        let rec = Arc::new(RecordingLauncher::default());
        let p = app_with_programs(&rec);
        let (list, ops) = fake_windows(
            vec![win(1, 10, FOO_EXE, false), win(2, 10, FOO_EXE, false), win(3, 12, r"C:\other.exe", false)],
            &[],
        );
        p.set_windows(list);
        assert_eq!(p.act(r"C:\Foo.lnk", Action::Close), Ok(Outcome::Hide));
        assert_eq!(*ops.closes.lock().unwrap(), ["close 1 10", "close 2 10"]);
    }

    #[test]
    fn close_skips_elevated_windows_and_says_so() {
        let rec = Arc::new(RecordingLauncher::default());
        let p = app_with_programs(&rec);
        let (list, ops) = fake_windows(vec![win(1, 10, FOO_EXE, true)], &[]);
        p.set_windows(list);
        assert_eq!(p.act(r"C:\Foo.lnk", Action::Close), Err("Needs administrator".to_string()));
        assert!(ops.closes.lock().unwrap().is_empty());
    }

    #[test]
    fn close_without_windows_says_they_are_gone() {
        let rec = Arc::new(RecordingLauncher::default());
        let p = app_with_programs(&rec);
        assert_eq!(p.act(r"C:\Foo.lnk", Action::Close), Err("That window is gone.".to_string()));
        let (list, ops) = fake_windows(vec![win(3, 12, r"C:\other.exe", false)], &[]);
        p.set_windows(list);
        assert_eq!(p.act(r"C:\Foo.lnk", Action::Close), Err("That window is gone.".to_string()));
        assert!(ops.closes.lock().unwrap().is_empty());
    }

    #[test]
    fn store_close_matches_by_aumid() {
        let rec = Arc::new(RecordingLauncher::default());
        let p = app_with_programs(&rec);
        let host = r"C:\Windows\ApplicationFrameHost.exe";
        let (list, ops) = fake_windows(vec![win(1, 10, host, false), win(2, 10, host, false)], &[(2, CALC_ID)]);
        p.set_windows(list);
        let calc = format!("shell:AppsFolder\\{CALC_ID}");
        assert_eq!(p.act(&calc, Action::Close), Ok(Outcome::Hide));
        assert_eq!(*ops.closes.lock().unwrap(), ["close 2 10"]);
    }

    /// Counts reads; returns fixed programs.
    struct CountingPrograms {
        reads: AtomicUsize,
    }

    impl ProgramSource for CountingPrograms {
        fn programs(&self) -> Vec<Program> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            vec![foo_program()]
        }
    }

    #[test]
    fn query_never_reads_the_registry() {
        let source = Arc::new(CountingPrograms { reads: AtomicUsize::new(0) });
        let rec = Arc::new(RecordingLauncher::default());
        let p = AppsProvider::with_source(rec, vec![shortcut("Foo", r"C:\Foo.lnk")], source.clone());
        let _ = p.query(&q("foo"));
        let _ = p.item(r"C:\Foo.lnk");
        assert_eq!(source.reads.load(Ordering::SeqCst), 0);
        p.refresh_from_stages(
            vec![shortcut("Foo", r"C:\Foo.lnk")],
            |_| Targets {
                exes: HashMap::from([(r"C:\Foo.lnk".to_string(), FOO_EXE.to_string())]),
                with_args: HashSet::new(),
            },
            || Some(vec![store("Calculator", CALC_ID)]),
        );
        assert_eq!(source.reads.load(Ordering::SeqCst), 1);
        assert_eq!(actions_of(&p, r"C:\Foo.lnk"), [Action::Admin, Action::Reveal, Action::Uninstall]);
        assert_eq!(actions_of(&p, &format!("shell:AppsFolder\\{CALC_ID}")), [Action::Uninstall]);
        let _ = p.query(&q("foo"));
        assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    }
}
