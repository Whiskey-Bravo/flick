//! The user's own shortcuts from the config: plain commands, commands that
//! take what is typed after the name (`{arg}`), and commands that pick from a
//! list (`pick` folder or `choices`, bound to `{name}` / `{path}`).
//!
//! Values never become shell text: `run` commands get references to
//! environment variables (`bind_windows`), `wsl` scripts get a leading
//! single-quoted assignment (`bind_bash`), `open` targets get the plain text.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::config::ShortcutDef;
use crate::fuzzy;
use crate::launch::{bind_bash, bind_windows, check_windows_value, to_wsl_path, valid_distro, Launcher, Shell};
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem};

/// Most pick entries one query returns (the engine caps scoped results at 50).
const MAX_ENTRIES: usize = 50;
/// Shortcuts beat an equal match from another provider.
const BONUS: u32 = 50;
const GONE: &str = "shortcut no longer exists";
const CHOICE_GONE: &str = "choice no longer exists";

/// Values for a shortcut's placeholders.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Substitution {
    pub arg: Option<String>,
    pub name: Option<String>,
    pub path: Option<String>,
}

/// What to start for a shortcut, with its values bound. `cwd`: the picked
/// folder's Windows path for a `pick` shortcut (the folder to start in),
/// else `None`.
#[derive(Clone, Debug, PartialEq)]
pub enum Prepared {
    Run { command: String, env: Vec<(String, String)>, keep_open: bool, admin: bool, cwd: Option<String> },
    Wsl { script: String, keep_open: bool, cwd: Option<String> },
    Open { target: String, admin: bool },
}

const PLACEHOLDERS: [&str; 3] = ["arg", "name", "path"];

/// The `run`, `wsl` or `open` text of a shortcut.
fn template(def: &ShortcutDef) -> &str {
    def.run.as_deref().or(def.wsl.as_deref()).or(def.open.as_deref()).unwrap_or("")
}

/// `{x}` -> `FLICK_X`, as `bind_windows` / `bind_bash` name them.
fn variable(name: &str) -> String {
    format!("FLICK_{}", name.to_ascii_uppercase())
}

/// `template` with each `{x}` in `values` replaced by its value, in one pass
/// (an inserted value is never scanned again).
fn insert_plain(template: &str, values: &[(&str, String)]) -> String {
    let mut out = String::new();
    let mut rest = template;
    'scan: while !rest.is_empty() {
        for (name, value) in values {
            if let Some(after) = rest.strip_prefix(&format!("{{{name}}}")) {
                out.push_str(value);
                rest = after;
                continue 'scan;
            }
        }
        let c = rest.chars().next().unwrap_or_default();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Percent-encode `v` (its UTF-8 bytes) for a URL: only the unreserved
/// characters `A-Z a-z 0-9 - . _ ~` stay as they are; a space is `%20`.
fn encode_url_component(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// An `http://` or `https://` target (any case).
fn is_web_url(target: &str) -> bool {
    let lower = target.get(..8).unwrap_or(target).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Bind the values of `sub` into `def`'s command. Pure: starts nothing.
/// - `run`: placeholders become `!FLICK_X!` references, values go in `env`;
/// - `wsl`: `{path}` is converted to `/mnt/...`, values go in a leading
///   assignment of the script;
/// - `open`: values are inserted as text (no shell), percent-encoded when the
///   target is an `http(s)://` URL.
///
/// `run` and `open` with `admin` take no values. A placeholder with no value
/// (config validation prevents this) is an error.
pub fn prepare(def: &ShortcutDef, sub: &Substitution, distro: &str) -> Result<Prepared, String> {
    let template = template(def);
    let cwd = if def.pick.is_some() { sub.path.clone() } else { None };
    let mut values: Vec<(&str, String)> = Vec::new();
    for (name, value) in PLACEHOLDERS.into_iter().zip([&sub.arg, &sub.name, &sub.path]) {
        if template.contains(&format!("{{{name}}}")) {
            let value = value.clone().ok_or_else(|| format!("nothing to put in {{{name}}}"))?;
            values.push((name, value));
        }
    }

    if let Some(command) = &def.run {
        let keep_open = def.terminal.unwrap_or(false);
        if values.is_empty() {
            return Ok(Prepared::Run { command: command.clone(), env: vec![], keep_open, admin: def.admin, cwd });
        }
        if def.admin {
            return Err("admin commands cannot take arguments".into());
        }
        for (_, value) in &values {
            check_windows_value(value)?;
        }
        let names: Vec<&str> = values.iter().map(|(n, _)| *n).collect();
        let command = bind_windows(command, &names)?;
        let env = values.into_iter().map(|(n, v)| (variable(n), v)).collect();
        return Ok(Prepared::Run { command, env, keep_open, admin: false, cwd });
    }

    if let Some(script) = &def.wsl {
        if distro.is_empty() {
            return Err(NO_WSL.into());
        }
        if !valid_distro(distro) {
            return Err("invalid wsl distro name".into());
        }
        for (name, value) in &mut values {
            if *name == "path" {
                *value = to_wsl_path(value).ok_or("cannot use this folder in wsl")?;
            }
        }
        let keep_open = def.terminal.unwrap_or(true);
        if values.is_empty() {
            return Ok(Prepared::Wsl { script: script.clone(), keep_open, cwd });
        }
        let pairs: Vec<(&str, &str)> = values.iter().map(|(n, v)| (*n, v.as_str())).collect();
        return Ok(Prepared::Wsl { script: bind_bash(script, &pairs)?, keep_open, cwd });
    }

    if def.open.is_some() {
        if def.admin && !values.is_empty() {
            return Err("admin commands cannot take arguments".into());
        }
        if is_web_url(template) {
            for (_, value) in &mut values {
                *value = encode_url_component(value);
            }
        }
        return Ok(Prepared::Open { target: insert_plain(template, &values), admin: def.admin });
    }
    Err("shortcut has no command".into())
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Plain,
    Arg,
    Pick,
}

fn kind(def: &ShortcutDef) -> Kind {
    if def.pick.is_some() || !def.choices.is_empty() {
        Kind::Pick
    } else if template(def).contains("{arg}") {
        Kind::Arg
    } else {
        Kind::Plain
    }
}

/// One choice of a pick shortcut: a sub-folder (with its full path) or a
/// `choices` text (no path).
#[derive(Clone)]
struct Entry {
    name: String,
    path: Option<String>,
}

/// Pick entries per shortcut name, or why they could not be read.
type Entries = HashMap<String, Result<Vec<Entry>, String>>;

#[derive(Default)]
struct State {
    defs: Vec<ShortcutDef>,
    distro: String,
    entries: Entries,
    /// Bumped by `set`, so a refresh that read older definitions is dropped.
    generation: u64,
}

/// Sub-folders of `dir`, sorted case-insensitively. Reads the disk.
fn read_folders(dir: &Path) -> Result<Vec<Entry>, String> {
    let list = std::fs::read_dir(dir).map_err(|_| format!("folder not found: {}", dir.display()))?;
    let mut out: Vec<Entry> = list
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            path: Some(e.path().to_string_lossy().into_owned()),
        })
        .collect();
    out.sort_by_cached_key(|e| e.name.to_lowercase());
    Ok(out)
}

/// The entries of every pick shortcut in `defs`. Reads the disk.
fn load_entries(defs: &[ShortcutDef]) -> Entries {
    defs.iter()
        .filter(|d| kind(d) == Kind::Pick)
        .map(|d| {
            let entries = match &d.pick {
                Some(dir) => read_folders(dir),
                None => {
                    let mut list: Vec<Entry> =
                        d.choices.iter().map(|c| Entry { name: c.clone(), path: None }).collect();
                    list.sort_by_cached_key(|e| e.name.to_lowercase());
                    Ok(list)
                }
            };
            (d.name.clone(), entries)
        })
        .collect()
}

/// Re-read the pick folders of the current definitions (off the lock), then
/// swap them in unless `set` replaced the definitions meanwhile.
fn refresh_state(state: &RwLock<State>) {
    let (defs, generation) = {
        let st = state.read().unwrap_or_else(|e| e.into_inner());
        (st.defs.clone(), st.generation)
    };
    let entries = load_entries(&defs);
    let mut st = state.write().unwrap_or_else(|e| e.into_inner());
    if st.generation == generation {
        st.entries = entries;
    }
}

pub struct ShortcutsProvider {
    launcher: Arc<dyn Launcher>,
    state: Arc<RwLock<State>>,
    /// A background refresh is running (at most one at a time).
    refreshing: Arc<AtomicBool>,
}

/// Clears the in-flight flag when the background refresh ends, even by panic.
struct InFlight(Arc<AtomicBool>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl ShortcutsProvider {
    pub fn new(launcher: Arc<dyn Launcher>, distro: &str) -> Arc<Self> {
        let state = State { distro: distro.to_string(), ..Default::default() };
        Arc::new(Self { launcher, state: Arc::new(RwLock::new(state)), refreshing: Arc::default() })
    }

    /// New definitions from the config (reload thread). Reads the pick
    /// folders before swapping, so queries never see a half-built list.
    pub fn set(&self, defs: Vec<ShortcutDef>, distro: &str) {
        let entries = load_entries(&defs);
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        st.generation += 1;
        *st = State { defs, distro: distro.to_string(), entries, generation: st.generation };
    }

    /// Re-read the pick folders (timer, and after a pick shortcut is chosen).
    pub fn refresh(&self) {
        refresh_state(&self.state);
    }

    /// Refresh on a background thread unless one is already running. True
    /// when a thread was started.
    fn spawn_refresh(&self) -> bool {
        if self.refreshing.swap(true, Ordering::AcqRel) {
            return false;
        }
        let (state, guard) = (self.state.clone(), InFlight(self.refreshing.clone()));
        std::thread::spawn(move || {
            let _guard = guard;
            refresh_state(&state);
        });
        true
    }

    fn run(&self, def: &ShortcutDef, sub: &Substitution, distro: &str) -> Result<Outcome, String> {
        match prepare(def, sub, distro)? {
            Prepared::Run { command, env, keep_open, admin, cwd } => {
                self.launcher.run_command(&Shell::Windows, &command, &env, keep_open, admin, cwd.as_deref())?
            }
            Prepared::Wsl { script, keep_open, cwd } => {
                let shell = Shell::Wsl { distro: distro.to_string() };
                self.launcher.run_command(&shell, &script, &[], keep_open, false, cwd.as_deref())?
            }
            Prepared::Open { target, admin: true } => self.launcher.open_with(&target, "", true)?,
            Prepared::Open { target, admin: false } => self.launcher.open(&target)?,
        }
        Ok(Outcome::Hide)
    }
}

/// The definition named `name` when it is of kind `want`.
fn find<'a>(defs: &'a [ShortcutDef], name: &str, want: Kind) -> Option<&'a ShortcutDef> {
    defs.iter().find(|d| d.name == name && kind(d) == want)
}

/// `about`, or the command when there is none.
/// Shown for a `wsl` shortcut when there is no WSL to run it in.
const NO_WSL: &str = "WSL is not installed";

fn describe(def: &ShortcutDef) -> String {
    if def.about.is_empty() {
        template(def).to_string()
    } else {
        def.about.clone()
    }
}

fn item(id: String, title: String, subtitle: String, score: u32) -> ResultItem {
    ResultItem { id, provider: "shortcuts", title, subtitle, score, confirm: false }
}

/// Entries matching `text`, best first, at most `MAX_ENTRIES`, with their
/// fuzzy score.
fn matching<'a>(entries: &'a [Entry], text: &str) -> Vec<(&'a Entry, u32)> {
    let mut found: Vec<(&Entry, u32)> =
        entries.iter().filter_map(|e| fuzzy::score(text, &e.name).map(|s| (e, s))).collect();
    found.sort_by(|(a, sa), (b, sb)| sb.cmp(sa).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    found.truncate(MAX_ENTRIES);
    found
}

impl Provider for ShortcutsProvider {
    fn name(&self) -> &'static str {
        "shortcuts"
    }

    /// `shortcut:<name>` of a pick shortcut that is currently defined.
    fn owns_scope(&self, scope: &str) -> bool {
        let st = self.state.read().unwrap_or_else(|e| e.into_inner());
        scope.strip_prefix("shortcut:").is_some_and(|name| find(&st.defs, name, Kind::Pick).is_some())
    }

    /// From memory only: pick entries come from the cache filled by `set`
    /// and `refresh`.
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let st = self.state.read().unwrap_or_else(|e| e.into_inner());
        let text = q.text.trim();

        if let Some(scope) = &q.scope {
            let Some(def) = scope.strip_prefix("shortcut:").and_then(|n| find(&st.defs, n, Kind::Pick)) else {
                return Vec::new();
            };
            let Some(Ok(entries)) = st.entries.get(&def.name) else { return Vec::new() };
            let entry = |e: &Entry, score: u32| {
                let id = format!("pick:{}:{}", def.name, e.name);
                // Every entry sits directly in the pick folder: no subtitle.
                item(id, e.name.clone(), String::new(), score)
            };
            if text.is_empty() {
                // Descending scores keep the alphabetical order through the engine's sort.
                return entries.iter().take(MAX_ENTRIES).zip(0..).map(|(e, i)| entry(e, 1000 - i)).collect();
            }
            return matching(entries, text).into_iter().map(|(e, s)| entry(e, s)).collect();
        }

        if text.is_empty() {
            return Vec::new();
        }
        let (first, rest) = match text.split_once(char::is_whitespace) {
            Some((first, rest)) => (first, rest.trim()),
            None => (text, ""),
        };
        let first = first.to_lowercase();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut push = |item: ResultItem| {
            if seen.insert(item.id.clone()) {
                out.push(item);
            }
        };
        for def in &st.defs {
            let words = || std::iter::once(&def.name).chain(&def.aliases);
            let n = &def.name;
            let k = kind(def);
            // The user's own shortcut stays listed and says why it cannot run.
            let no_wsl = def.wsl.is_some() && st.distro.is_empty();
            let about = || if no_wsl { NO_WSL.to_string() } else { describe(def) };

            if let Some(score) = words().filter_map(|w| fuzzy::score(text, w)).max() {
                let (id, subtitle) = match k {
                    Kind::Plain => (format!("run:{n}"), about()),
                    Kind::Arg if no_wsl => (format!("need:{n}"), about()),
                    Kind::Arg => (format!("need:{n}"), format!("type: {n} <argument>")),
                    Kind::Pick if no_wsl => (format!("step:{n}"), about()),
                    Kind::Pick => (format!("step:{n}"), "choose…".to_string()),
                };
                push(item(id, n.clone(), subtitle, score + BONUS));
            }

            if rest.is_empty() || !words().any(|w| w.to_lowercase() == first) {
                continue;
            }
            match k {
                Kind::Plain => {}
                Kind::Arg => push(item(format!("arg:{n}:{rest}"), format!("{n} {rest}"), about(), 1000 + BONUS)),
                Kind::Pick => {
                    if let Some(Ok(entries)) = st.entries.get(n) {
                        for (e, score) in matching(entries, rest) {
                            push(item(
                                format!("pick:{n}:{}", e.name),
                                format!("{n} › {}", e.name),
                                about(),
                                score + BONUS,
                            ));
                        }
                    }
                }
            }
        }
        out
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        let gone = || Err(GONE.to_string());
        let Some((prefix, rest)) = id.split_once(':') else { return gone() };
        // Names cannot contain `:` (config validation), so the name ends at
        // the first `:` of `rest`; the value may contain more.
        let (name, value) = match prefix {
            "run" | "need" | "step" => (rest, None),
            "arg" | "pick" => match rest.split_once(':') {
                Some((name, value)) => (name, Some(value)),
                None => return gone(),
            },
            _ => return gone(),
        };
        let want = match prefix {
            "run" => Kind::Plain,
            "need" | "arg" => Kind::Arg,
            _ => Kind::Pick,
        };
        // Copy only this shortcut (and, for a pick, its one entry), then drop
        // the lock before starting anything: a launch can block (UAC prompt)
        // while the config reloads.
        let (def, distro, entry) = {
            let st = self.state.read().unwrap_or_else(|e| e.into_inner());
            let Some(def) = find(&st.defs, name, want) else { return gone() };
            let entry: Option<Result<Option<Entry>, String>> = st.entries.get(name).map(|r| match r {
                Ok(list) => Ok(value.and_then(|v| list.iter().find(|e| e.name == v).cloned())),
                Err(e) => Err(e.clone()),
            });
            (def.clone(), st.distro.clone(), entry)
        };

        match prefix {
            "run" => self.run(&def, &Substitution::default(), &distro),
            "need" => Ok(Outcome::Fill { text: format!("{} ", def.name) }),
            "arg" => {
                let sub = Substitution { arg: value.map(str::to_string), ..Default::default() };
                self.run(&def, &sub, &distro)
            }
            "step" => {
                // Folders may have changed since the last refresh.
                self.spawn_refresh();
                if let Some(Err(e)) = entry {
                    return Err(e);
                }
                Ok(Outcome::Step { prompt: def.name.clone(), scope: format!("shortcut:{}", def.name) })
            }
            _ => {
                let entry = match entry {
                    Some(Ok(Some(entry))) => entry,
                    Some(Ok(None)) => return Err(CHOICE_GONE.into()),
                    Some(Err(e)) => return Err(e),
                    None => return gone(),
                };
                // The cached list can be up to one refresh old.
                if entry.path.as_deref().is_some_and(|p| !Path::new(p).is_dir()) {
                    self.spawn_refresh();
                    return Err(CHOICE_GONE.into());
                }
                let sub = Substitution { name: Some(entry.name), path: entry.path, arg: None };
                self.run(&def, &sub, &distro)
            }
        }
    }

    /// The shortcut's name, plus the entry for picks. Typed arguments are
    /// never part of the key.
    fn rank_key(&self, id: &str) -> Option<String> {
        let (prefix, rest) = id.split_once(':')?;
        let key = match prefix {
            "run" | "need" | "step" => rest.to_string(),
            "arg" => rest.split_once(':')?.0.to_string(),
            "pick" => {
                let (name, entry) = rest.split_once(':')?;
                format!("{name}/{entry}")
            }
            _ => return None,
        };
        (!key.is_empty()).then_some(key)
    }

    fn help(&self) -> Vec<HelpLine> {
        let st = self.state.read().unwrap_or_else(|e| e.into_inner());
        if st.defs.is_empty() {
            return vec![HelpLine {
                example: "your shortcuts".into(),
                about: "define your own commands in the config (type: flick config)".into(),
                fill: "flick config".into(),
            }];
        }
        st.defs
            .iter()
            .map(|def| {
                let n = &def.name;
                let example = match kind(def) {
                    Kind::Plain => n.clone(),
                    Kind::Arg => format!("{n} <argument>"),
                    Kind::Pick => format!("{n} …"),
                };
                HelpLine { example, about: describe(def), fill: format!("{n} ") }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::testing::RecordingLauncher;

    // ---- helpers -----------------------------------------------------------

    fn base(name: &str) -> ShortcutDef {
        ShortcutDef {
            name: name.into(),
            aliases: vec![],
            about: String::new(),
            run: None,
            wsl: None,
            open: None,
            terminal: None,
            admin: false,
            pick: None,
            choices: vec![],
        }
    }

    fn run(name: &str, cmd: &str) -> ShortcutDef {
        ShortcutDef { run: Some(cmd.into()), ..base(name) }
    }

    fn wsl(name: &str, cmd: &str) -> ShortcutDef {
        ShortcutDef { wsl: Some(cmd.into()), ..base(name) }
    }

    fn open(name: &str, target: &str) -> ShortcutDef {
        ShortcutDef { open: Some(target.into()), ..base(name) }
    }

    fn picker(def: ShortcutDef, dir: &Path) -> ShortcutDef {
        ShortcutDef { pick: Some(dir.to_path_buf()), ..def }
    }

    fn setup(defs: Vec<ShortcutDef>) -> (Arc<ShortcutsProvider>, Arc<RecordingLauncher>) {
        let rec = Arc::new(RecordingLauncher::default());
        let p = ShortcutsProvider::new(rec.clone(), "Debian");
        p.set(defs, "Debian");
        (p, rec)
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    fn scoped(scope: &str, text: &str) -> Query {
        Query { text: text.into(), scope: Some(scope.into()) }
    }

    fn ids(items: &[ResultItem]) -> Vec<String> {
        items.iter().map(|i| i.id.clone()).collect()
    }

    fn titles(items: &[ResultItem]) -> Vec<String> {
        items.iter().map(|i| i.title.clone()).collect()
    }

    /// Sub-folders `Alpha`, `beta`, `gamma` and a file that is not a choice.
    fn projects() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for d in ["beta", "Alpha", "gamma"] {
            std::fs::create_dir(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        dir
    }

    fn full(dir: &Path, entry: &str) -> String {
        dir.join(entry).to_string_lossy().into_owned()
    }

    fn sub_name(v: &str) -> Substitution {
        Substitution { name: Some(v.into()), ..Default::default() }
    }

    // ---- plain ---------------------------------------------------------------

    #[test]
    fn plain_shortcut_runs_its_command() {
        let (p, rec) = setup(vec![ShortcutDef { about: "say hi".into(), ..run("hello", "notepad") }]);
        assert_eq!(p.name(), "shortcuts");
        let r = p.query(&q("hel"));
        assert_eq!(ids(&r), vec!["run:hello"]);
        assert_eq!(r[0].title, "hello");
        assert_eq!(r[0].subtitle, "say hi");
        assert_eq!(r[0].provider, "shortcuts");
        assert!(!r[0].confirm);
        assert_eq!(p.activate("run:hello", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(rec.calls(), vec!["run_command Windows env=[] keep=false elevated=false cwd=-: notepad"]);
    }

    #[test]
    fn subtitle_falls_back_to_the_command() {
        let (p, _) = setup(vec![run("hello", "notepad")]);
        assert_eq!(p.query(&q("hello"))[0].subtitle, "notepad");
    }

    #[test]
    fn plain_command_is_passed_unchanged_with_terminal_and_admin() {
        let def = ShortcutDef { terminal: Some(true), admin: true, ..run("x", "echo hi! & dir") };
        assert_eq!(
            prepare(&def, &Substitution::default(), "Debian"),
            Ok(Prepared::Run { command: "echo hi! & dir".into(), env: vec![], keep_open: true, admin: true, cwd: None })
        );
        let (p, rec) = setup(vec![def]);
        p.activate("run:x", Modifier::None).unwrap();
        assert_eq!(rec.calls(), vec!["run_command Windows env=[] keep=true elevated=true cwd=-: echo hi! & dir"]);
    }

    #[test]
    fn empty_text_gives_nothing() {
        let (p, _) = setup(vec![run("hello", "notepad")]);
        assert!(p.query(&q("")).is_empty());
        assert!(p.query(&q("   ")).is_empty());
    }

    #[test]
    fn alias_finds_the_shortcut() {
        let (p, _) = setup(vec![ShortcutDef { aliases: vec!["np".into()], ..run("notepad", "notepad") }]);
        let r = p.query(&q("np"));
        assert_eq!(ids(&r), vec!["run:notepad"]);
        assert_eq!(r[0].title, "notepad");
        assert_eq!(r[0].score, 1000 + 50);
    }

    #[test]
    fn shortcut_outscores_equal_app_match() {
        let (p, _) = setup(vec![run("hello", "notepad")]);
        let r = p.query(&q("hel"));
        assert_eq!(r[0].score, fuzzy::score("hel", "hello").unwrap() + 50);
    }

    #[test]
    fn launcher_error_is_returned_unchanged() {
        let (p, rec) = setup(vec![run("hello", "notepad")]);
        rec.fail_next("file not found");
        assert_eq!(p.activate("run:hello", Modifier::None), Err("file not found".to_string()));
    }

    // ---- arg -----------------------------------------------------------------

    #[test]
    fn arg_shortcut_without_text_fills_the_name() {
        let (p, rec) = setup(vec![run("search", "prog {arg}")]);
        let r = p.query(&q("search"));
        assert_eq!(ids(&r), vec!["need:search"]);
        assert_eq!(r[0].title, "search");
        assert_eq!(r[0].subtitle, "type: search <argument>");
        assert_eq!(
            p.activate("need:search", Modifier::None),
            Ok(Outcome::Fill { text: "search ".into() })
        );
        assert!(rec.calls().is_empty());
    }

    #[test]
    fn arg_shortcut_runs_with_bound_argument() {
        let (p, rec) = setup(vec![ShortcutDef { aliases: vec!["s".into()], ..run("search", "prog {arg}") }]);
        let r = p.query(&q("S a & b"));
        assert_eq!(ids(&r), vec!["arg:search:a & b"]);
        assert_eq!(r[0].title, "search a & b");
        assert_eq!(r[0].subtitle, "prog {arg}");
        assert_eq!(r[0].score, 1050);
        assert_eq!(p.activate("arg:search:a & b", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(
            rec.calls(),
            vec![r#"run_command Windows env=[FLICK_ARG=a & b] keep=false elevated=false cwd=-: prog "!FLICK_ARG!""#]
        );
    }

    #[test]
    fn argument_with_a_quote_is_refused_and_nothing_runs() {
        let (p, rec) = setup(vec![run("search", "prog {arg}")]);
        let id = r#"arg:search:x" & calc & ""#;
        assert_eq!(ids(&p.query(&q(r#"search x" & calc & ""#))), vec![id]);
        assert_eq!(
            p.activate(id, Modifier::None),
            Err(r#"cannot pass text containing " or % to this command"#.to_string())
        );
        assert!(rec.calls().is_empty());
    }

    #[test]
    fn argument_may_contain_colons() {
        let (p, rec) = setup(vec![run("search", "prog {arg}")]);
        p.activate("arg:search:a:b:c", Modifier::None).unwrap();
        assert_eq!(
            rec.calls(),
            vec![r#"run_command Windows env=[FLICK_ARG=a:b:c] keep=false elevated=false cwd=-: prog "!FLICK_ARG!""#]
        );
    }

    #[test]
    fn admin_with_a_value_is_refused_by_prepare() {
        let def = ShortcutDef { admin: true, ..run("x", "prog {arg}") };
        let sub = Substitution { arg: Some("a".into()), ..Default::default() };
        assert_eq!(prepare(&def, &sub, "Debian"), Err("admin commands cannot take arguments".to_string()));
    }

    // ---- pick ----------------------------------------------------------------

    #[test]
    fn pick_shortcut_steps_into_its_scope() {
        let dir = projects();
        let (p, rec) = setup(vec![picker(run("proj", "code {path}"), dir.path()), run("search", "p {arg}")]);
        let r = p.query(&q("proj"));
        assert_eq!(ids(&r), vec!["step:proj"]);
        assert_eq!(r[0].subtitle, "choose…");
        assert_eq!(
            p.activate("step:proj", Modifier::None),
            Ok(Outcome::Step { prompt: "proj".into(), scope: "shortcut:proj".into() })
        );
        assert!(p.owns_scope("shortcut:proj"));
        assert!(!p.owns_scope("shortcut:search"));
        assert!(!p.owns_scope("shortcut:other"));
        assert!(!p.owns_scope("proj"));
        assert!(rec.calls().is_empty());
    }

    #[test]
    fn scope_lists_subfolders_alphabetically() {
        let dir = projects();
        let (p, _) = setup(vec![picker(run("proj", "code {path}"), dir.path())]);
        let r = p.query(&scoped("shortcut:proj", ""));
        assert_eq!(titles(&r), vec!["Alpha", "beta", "gamma"]);
        assert_eq!(ids(&r), vec!["pick:proj:Alpha", "pick:proj:beta", "pick:proj:gamma"]);
        // Every entry sits directly in the pick folder: its path adds nothing.
        assert!(r.iter().all(|i| i.subtitle.is_empty()), "{r:?}");
        // The engine sorts by score: keep the alphabetical order.
        assert!(r[0].score > r[1].score && r[1].score > r[2].score);
    }

    #[test]
    fn scope_filters_by_text() {
        let dir = projects();
        let (p, _) = setup(vec![picker(run("proj", "code {path}"), dir.path())]);
        assert_eq!(titles(&p.query(&scoped("shortcut:proj", "gam"))), vec!["gamma"]);
        assert!(p.query(&scoped("shortcut:proj", "zzz")).is_empty());
        assert!(p.query(&scoped("shortcut:other", "")).is_empty());
    }

    #[test]
    fn scope_returns_at_most_50_entries_best_first() {
        let choices: Vec<String> = (0..80).map(|i| format!("item{i:02}")).collect();
        let mut def = run("c", "echo {name}");
        def.choices = choices;
        def.choices.push("best".into());
        let (p, _) = setup(vec![def]);
        assert_eq!(p.query(&scoped("shortcut:c", "")).len(), 50);
        let r = p.query(&scoped("shortcut:c", "item"));
        assert_eq!(r.len(), 50);
        let r = p.query(&scoped("shortcut:c", "best"));
        assert_eq!(titles(&r)[0], "best");
    }

    #[test]
    fn choices_are_entries_without_a_path() {
        let mut def = run("c", "echo {name}");
        def.choices = vec!["zeta".into(), "Eta".into()];
        let (p, rec) = setup(vec![def]);
        let r = p.query(&scoped("shortcut:c", ""));
        assert_eq!(titles(&r), vec!["Eta", "zeta"]);
        assert_eq!(r[0].subtitle, "");
        p.activate("pick:c:zeta", Modifier::None).unwrap();
        assert_eq!(
            rec.calls(),
            vec![r#"run_command Windows env=[FLICK_NAME=zeta] keep=false elevated=false cwd=-: echo "!FLICK_NAME!""#]
        );
    }

    #[test]
    fn inline_pick_lists_matches_after_the_name() {
        let dir = projects();
        let (p, rec) = setup(vec![picker(run("proj", "code {path}"), dir.path())]);
        let r = p.query(&q("proj be"));
        assert_eq!(ids(&r), vec!["pick:proj:beta"]);
        assert_eq!(r[0].title, "proj › beta");
        assert_eq!(r[0].subtitle, "code {path}");
        assert_eq!(r[0].score, fuzzy::score("be", "beta").unwrap() + 50);
        assert_eq!(p.activate("pick:proj:beta", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(
            rec.calls(),
            vec![format!(
                r#"run_command Windows env=[FLICK_PATH={beta}] keep=false elevated=false cwd={beta}: code "!FLICK_PATH!""#,
                beta = full(dir.path(), "beta")
            )]
        );
    }

    #[test]
    fn ids_are_never_duplicated() {
        // The whole text matches the shortcut and its first word names it.
        let mut def = ShortcutDef { aliases: vec!["pr".into()], ..run("proj", "echo {name}") };
        def.choices = vec!["proj".into(), "other".into()];
        let (p, _) = setup(vec![def]);
        let r = p.query(&q("proj proj"));
        assert_eq!(ids(&r), vec!["step:proj", "pick:proj:proj"]);
        let unique: HashSet<String> = ids(&r).into_iter().collect();
        assert_eq!(unique.len(), r.len());
    }

    #[test]
    fn picked_folder_with_spaces_and_umlauts_is_passed_intact() {
        let dir = tempfile::tempdir().unwrap();
        let name = "Résumé & more";
        std::fs::create_dir(dir.path().join(name)).unwrap();
        let path = full(dir.path(), name);
        let sub = Substitution { name: Some(name.into()), path: Some(path.clone()), arg: None };

        let win = picker(run("code", "code {path}"), dir.path());
        assert_eq!(
            prepare(&win, &sub, "Debian"),
            Ok(Prepared::Run {
                command: r#"code "!FLICK_PATH!""#.into(),
                env: vec![("FLICK_PATH".into(), path.clone())],
                keep_open: false,
                admin: false,
                cwd: Some(path.clone()),
            })
        );

        let lin = picker(wsl("ls", "ls {path}"), dir.path());
        let Ok(Prepared::Wsl { script, keep_open: true, cwd }) = prepare(&lin, &sub, "Debian") else {
            panic!("not a wsl script")
        };
        // The Windows path: wsl.exe translates its own working directory.
        assert_eq!(cwd.as_deref(), Some(path.as_str()));
        let wsl_path = to_wsl_path(&path).unwrap();
        assert!(wsl_path.starts_with("/mnt/") && wsl_path.ends_with("/Résumé & more"), "{wsl_path}");
        let assignment = format!("FLICK_PATH='{wsl_path}'; ");
        assert!(script.starts_with(&assignment), "{script}");
        assert_eq!(&script[assignment.len()..], r#"ls "${FLICK_PATH}""#);

        // The same through the provider.
        let (p, rec) = setup(vec![win, ShortcutDef { name: "lsw".into(), ..lin }]);
        p.activate(&format!("pick:code:{name}"), Modifier::None).unwrap();
        p.activate(&format!("pick:lsw:{name}"), Modifier::None).unwrap();
        assert_eq!(
            rec.calls(),
            vec![
                format!(r#"run_command Windows env=[FLICK_PATH={path}] keep=false elevated=false cwd={path}: code "!FLICK_PATH!""#),
                format!(r#"run_command Wsl(Debian) env=[] keep=true elevated=false cwd={path}: {script}"#),
            ]
        );
    }

    #[test]
    fn pick_shortcut_starts_in_the_picked_folder_others_in_the_default() {
        let dir = projects();
        let beta = full(dir.path(), "beta");
        let mut choice = run("c", "echo {name}");
        choice.choices = vec!["zeta".into()];
        let (p, rec) = setup(vec![
            picker(run("here", "echo %CD%"), dir.path()),
            picker(wsl("lsw", "pwd"), dir.path()),
            choice,
            run("plain", "notepad"),
        ]);
        p.activate("pick:here:beta", Modifier::None).unwrap();
        p.activate("pick:lsw:beta", Modifier::None).unwrap();
        p.activate("pick:c:zeta", Modifier::None).unwrap();
        p.activate("run:plain", Modifier::None).unwrap();
        assert_eq!(
            rec.calls(),
            vec![
                format!("run_command Windows env=[] keep=false elevated=false cwd={beta}: echo %CD%"),
                format!("run_command Wsl(Debian) env=[] keep=true elevated=false cwd={beta}: pwd"),
                r#"run_command Windows env=[FLICK_NAME=zeta] keep=false elevated=false cwd=-: echo "!FLICK_NAME!""#
                    .to_string(),
                "run_command Windows env=[] keep=false elevated=false cwd=-: notepad".to_string(),
            ]
        );
    }

    #[test]
    fn user_quoted_placeholder_keeps_one_pair_of_quotes() {
        let def = picker(run("code", r#"code "{path}""#), Path::new("C:/p"));
        let sub = Substitution { path: Some(r"C:\p\a b".into()), ..Default::default() };
        let Ok(Prepared::Run { command, .. }) = prepare(&def, &sub, "Debian") else { panic!() };
        assert_eq!(command, r#"code "!FLICK_PATH!""#);
    }

    #[test]
    fn folder_outside_a_drive_cannot_be_used_in_wsl() {
        let def = picker(wsl("ls", "ls {path}"), Path::new(r"\\server\share"));
        let sub = Substitution { path: Some(r"\\server\share\x".into()), ..Default::default() };
        assert_eq!(prepare(&def, &sub, "Debian"), Err("cannot use this folder in wsl".to_string()));
    }

    #[test]
    fn missing_pick_folder_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        let (p, rec) = setup(vec![picker(run("proj", "code {path}"), &missing)]);
        assert_eq!(ids(&p.query(&q("proj"))), vec!["step:proj"]);
        assert!(p.query(&q("proj x")).is_empty());
        assert!(p.query(&scoped("shortcut:proj", "")).is_empty());
        assert_eq!(
            p.activate("step:proj", Modifier::None),
            Err(format!("folder not found: {}", missing.display()))
        );
        assert!(rec.calls().is_empty());
    }

    #[test]
    fn entry_no_longer_in_the_list_is_an_error() {
        let dir = projects();
        let (p, rec) = setup(vec![picker(run("proj", "code {path}"), dir.path())]);
        assert_eq!(p.activate("pick:proj:..", Modifier::None), Err("choice no longer exists".to_string()));
        assert_eq!(p.activate("pick:proj:delta", Modifier::None), Err("choice no longer exists".to_string()));
        assert!(rec.calls().is_empty());
    }

    #[test]
    fn no_disk_access_in_query() {
        let dir = projects();
        let root = dir.path().to_path_buf();
        let (p, _) = setup(vec![picker(run("proj", "code {path}"), &root)]);
        p.refresh();
        drop(dir); // deletes the folder
        assert!(!root.exists());
        assert_eq!(titles(&p.query(&scoped("shortcut:proj", ""))), vec!["Alpha", "beta", "gamma"]);
        assert_eq!(ids(&p.query(&q("proj al"))), vec!["pick:proj:Alpha"]);
        // A refresh then picks up the change.
        p.refresh();
        assert!(p.query(&scoped("shortcut:proj", "")).is_empty());
    }

    #[test]
    fn refresh_picks_up_new_folders() {
        let dir = projects();
        let (p, _) = setup(vec![picker(run("proj", "code {path}"), dir.path())]);
        std::fs::create_dir(dir.path().join("delta")).unwrap();
        assert_eq!(p.query(&scoped("shortcut:proj", "")).len(), 3);
        p.refresh();
        assert_eq!(p.query(&scoped("shortcut:proj", "")).len(), 4);
    }

    // ---- wsl / open ------------------------------------------------------

    #[test]
    fn wsl_shortcut_defaults_to_open_terminal() {
        let (p, rec) = setup(vec![
            wsl("upgrade", "sudo apt upgrade"),
            ShortcutDef { terminal: Some(false), ..wsl("up", "apt update") },
        ]);
        p.activate("run:upgrade", Modifier::None).unwrap();
        p.activate("run:up", Modifier::None).unwrap();
        assert_eq!(
            rec.calls(),
            vec![
                "run_command Wsl(Debian) env=[] keep=true elevated=false cwd=-: sudo apt upgrade",
                "run_command Wsl(Debian) env=[] keep=false elevated=false cwd=-: apt update",
            ]
        );
    }

    #[test]
    fn wsl_shortcut_without_wsl_says_so() {
        assert_eq!(
            prepare(&wsl("v", "x"), &Substitution::default(), ""),
            Err("WSL is not installed".to_string())
        );
    }

    #[test]
    fn wsl_shortcut_is_listed_but_explains_itself_without_wsl() {
        let (p, rec) = setup(vec![wsl("upgrade", "x"), run("calc", "calc.exe")]);
        p.set(vec![wsl("upgrade", "x"), run("calc", "calc.exe")], "");
        assert_eq!(p.query(&Query { text: "upgrade".into(), scope: None })[0].subtitle, "WSL is not installed");
        assert_eq!(p.query(&Query { text: "calc".into(), scope: None })[0].subtitle, "calc.exe");
        assert_eq!(p.help().len(), 2);
        assert_eq!(p.activate("run:upgrade", Modifier::None), Err("WSL is not installed".to_string()));
        assert!(rec.calls().is_empty());
    }

    #[test]
    fn set_changes_the_distro() {
        let (p, rec) = setup(vec![wsl("upgrade", "x")]);
        p.set(vec![wsl("upgrade", "x")], "Ubuntu");
        p.activate("run:upgrade", Modifier::None).unwrap();
        assert_eq!(rec.calls(), vec!["run_command Wsl(Ubuntu) env=[] keep=true elevated=false cwd=-: x"]);
        assert_eq!(prepare(&wsl("v", "x"), &Substitution::default(), "a b"), Err("invalid wsl distro name".to_string()));
    }

    #[test]
    fn wsl_shortcut_value_never_appears_outside_its_assignment() {
        let value = "$(echo INJECTED)";
        let mut def = wsl("w", r#"echo "$(basename "{name}")""#);
        def.choices = vec![value.into()];
        let Ok(Prepared::Wsl { script, .. }) = prepare(&def, &sub_name(value), "Debian") else { panic!() };
        let assignment = "FLICK_NAME='$(echo INJECTED)'; ";
        assert!(script.starts_with(assignment), "{script}");
        assert_eq!(script.matches(value).count(), 1, "{script}");
        assert_eq!(&script[assignment.len()..], r#"echo "$(basename "${FLICK_NAME}")""#);

        let (p, rec) = setup(vec![def]);
        p.activate(&format!("pick:w:{value}"), Modifier::None).unwrap();
        assert_eq!(rec.calls(), vec![format!("run_command Wsl(Debian) env=[] keep=true elevated=false cwd=-: {script}")]);
    }

    #[test]
    fn url_components_are_percent_encoded() {
        assert_eq!(encode_url_component("a b"), "a%20b");
        assert_eq!(encode_url_component("c++"), "c%2B%2B");
        assert_eq!(encode_url_component("a&b=c"), "a%26b%3Dc");
        assert_eq!(encode_url_component("#x"), "%23x");
        assert_eq!(encode_url_component("Résumé"), "R%C3%A9sum%C3%A9");
        assert_eq!(encode_url_component("AZaz09-._~"), "AZaz09-._~");
        assert_eq!(encode_url_component("/?%"), "%2F%3F%25");
        assert_eq!(encode_url_component(""), "");
    }

    #[test]
    fn open_url_shortcut_encodes_values() {
        let (p, rec) = setup(vec![
            open("web", "https://example.com/search?q={arg}"),
            open("old", "HTTP://example.com/{arg}"),
        ]);
        p.activate("arg:web:a b", Modifier::None).unwrap();
        p.activate("arg:old:c++ & #x", Modifier::None).unwrap();
        assert_eq!(
            rec.calls(),
            vec!["open https://example.com/search?q=a%20b", "open HTTP://example.com/c%2B%2B%20%26%20%23x"]
        );
    }

    #[test]
    fn open_path_shortcut_inserts_values_as_plain_text() {
        let (p, rec) = setup(vec![
            open("note", "C:/notes/{arg}.txt"),
            open("mail", "mailto:{arg}"),
            ShortcutDef { admin: true, ..open("adm", "C:/tools/x.exe") },
        ]);
        p.activate("arg:note:a b & c", Modifier::None).unwrap();
        p.activate("arg:mail:x y", Modifier::None).unwrap();
        p.activate("run:adm", Modifier::None).unwrap();
        assert_eq!(
            rec.calls(),
            vec!["open C:/notes/a b & c.txt", "open mailto:x y", "open_with C:/tools/x.exe elevated=true: "]
        );
        // One pass: a value that looks like a placeholder stays as it is.
        let def = picker(open("o", "x/{name}/{path}"), Path::new("C:/p"));
        let sub = Substitution { name: Some("{path}".into()), path: Some("P".into()), arg: None };
        assert_eq!(prepare(&def, &sub, "Debian"), Ok(Prepared::Open { target: "x/{path}/P".into(), admin: false }));
    }

    #[test]
    fn admin_open_with_a_value_is_refused_by_prepare() {
        let def = ShortcutDef { admin: true, ..open("x", "https://e.com/{arg}") };
        let sub = Substitution { arg: Some("a".into()), ..Default::default() };
        assert_eq!(prepare(&def, &sub, "Debian"), Err("admin commands cannot take arguments".to_string()));
    }

    #[test]
    fn picked_folder_removed_since_refresh_is_an_error_and_refreshes() {
        let dir = projects();
        let (p, rec) = setup(vec![picker(run("proj", "code {path}"), dir.path())]);
        std::fs::remove_dir(dir.path().join("beta")).unwrap();
        assert_eq!(p.activate("pick:proj:beta", Modifier::None), Err("choice no longer exists".to_string()));
        assert!(rec.calls().is_empty());
        // The background refresh drops it from the list.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while p.query(&scoped("shortcut:proj", "")).len() != 2 {
            assert!(std::time::Instant::now() < deadline, "no refresh");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn at_most_one_background_refresh_runs() {
        let dir = projects();
        let (p, _) = setup(vec![picker(run("proj", "code {path}"), dir.path())]);
        {
            // Hold the state so the first refresh cannot finish.
            let _held = p.state.write().unwrap();
            assert!(p.spawn_refresh());
            assert!(!p.spawn_refresh());
            assert!(!p.spawn_refresh());
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !p.spawn_refresh() {
            assert!(std::time::Instant::now() < deadline, "refresh never finished");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn missing_value_is_an_error() {
        let mut def = run("c", "echo {path}");
        def.choices = vec!["a".into()];
        assert_eq!(prepare(&def, &sub_name("a"), "Debian"), Err("nothing to put in {path}".to_string()));
    }

    // ---- ids, ranking, help ------------------------------------------------

    #[test]
    fn removed_shortcut_id_is_an_error() {
        let dir = projects();
        let (p, rec) = setup(vec![
            run("hello", "notepad"),
            run("search", "p {arg}"),
            picker(run("proj", "code {path}"), dir.path()),
        ]);
        p.set(vec![], "Debian");
        let gone = Err(GONE.to_string());
        for id in ["run:hello", "need:search", "arg:search:x", "step:proj", "pick:proj:beta", "garbage", "", "run:"] {
            assert_eq!(p.activate(id, Modifier::None), gone, "{id}");
        }
        assert!(!p.owns_scope("shortcut:proj"));
        // An id of the wrong kind for the current definition is gone too.
        p.set(vec![run("hello", "p {arg}")], "Debian");
        assert_eq!(p.activate("run:hello", Modifier::None), gone);
        assert_eq!(p.activate("step:hello", Modifier::None), gone);
        assert!(rec.calls().is_empty());
    }

    #[test]
    fn rank_keys_never_contain_typed_text() {
        let (p, _) = setup(vec![]);
        assert_eq!(p.rank_key("run:hello").as_deref(), Some("hello"));
        assert_eq!(p.rank_key("need:search").as_deref(), Some("search"));
        assert_eq!(p.rank_key("step:proj").as_deref(), Some("proj"));
        assert_eq!(p.rank_key("arg:search:my secret text").as_deref(), Some("search"));
        assert_eq!(p.rank_key("arg:search:a:b").as_deref(), Some("search"));
        assert_eq!(p.rank_key("pick:proj:beta").as_deref(), Some("proj/beta"));
        assert_eq!(p.rank_key("garbage"), None);
    }

    #[test]
    fn help_lists_each_shortcut() {
        let dir = projects();
        let (p, _) = setup(vec![
            ShortcutDef { about: "say hi".into(), ..run("hello", "notepad") },
            run("search", "p {arg}"),
            picker(run("proj", "code {path}"), dir.path()),
        ]);
        let line = |e: &str, a: &str, f: &str| HelpLine { example: e.into(), about: a.into(), fill: f.into() };
        assert_eq!(
            p.help(),
            vec![
                line("hello", "say hi", "hello "),
                line("search <argument>", "p {arg}", "search "),
                line("proj …", "code {path}", "proj "),
            ]
        );
        p.set(vec![], "Debian");
        assert_eq!(
            p.help(),
            vec![line(
                "your shortcuts",
                "define your own commands in the config (type: flick config)",
                "flick config"
            )]
        );
    }
}
