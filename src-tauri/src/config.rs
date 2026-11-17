//! Config: `%APPDATA%\Flick\config.toml`, parsed with a last-good fallback and hot-reloaded.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

const DEBOUNCE: Duration = Duration::from_millis(300);

pub(crate) const DEFAULT_FILE: &str = "\
## Flick configuration. Changes are picked up as soon as you save.
##
## hotkey: global shortcut that shows the launcher, e.g. \"Alt+Space\".
# hotkey = \"Alt+Space\"
##
## wsl_distro: the WSL distro for > commands and wsl shortcuts. Leave unset to use WSL's default distro.
# wsl_distro = \"Ubuntu\"
##
## file_roots: folders searched for files. Leave unset to search your whole user folder.
# file_roots = [\"C:/Users/you/Desktop\", \"C:/Users/you/Documents\", \"C:/Users/you/Downloads\"]
##
## history: remember recent searches and launches (false: forget and stop).
# history = true
##
## online: allow currency and weather answers to go online (false: never).
# online = true
##
## weather_city: the city for a bare `weather`.
# weather_city = \"London\"
##
## currency: the currency for a bare `20 usd`. Leave unset to use the one of your Windows region.
# currency = \"EUR\"
##
## temperature_unit: \"c\" or \"f\". Leave unset to follow your Windows region.
# temperature_unit = \"c\"
##
## search_engine: the web search behind `? text` and Enter on a weather answer; {arg} is the search text.
# search_engine = \"https://duckduckgo.com/?q={arg}\"
##
## check_updates: ask GitHub about once a day whether a new version exists (false: never).
# check_updates = true
##
## Shortcuts: type the name (or an alias), then press Space for arguments or choices.
## Each needs exactly one of run (Windows command), wsl (command in the distro) or open (path or URL).
## Placeholders: {arg} is the text typed after the name; with pick (folder whose sub-folders
## are the choices) or choices (a list of texts), {name} is the picked entry and {path} its full path.
##
## A plain link:
# [[shortcut]]
# name = \"docs\"
# about = \"Rust documentation\"
# open = \"https://docs.rs\"
##
## A link that takes what you type (it is encoded for the web address):
# [[shortcut]]
# name = \"search\"
# aliases = [\"s\"]
# about = \"Web search\"
# open = \"https://duckduckgo.com/?q={arg}\"
##
## Pick a sub-folder of a folder:
# [[shortcut]]
# name = \"proj\"
# about = \"Open a project in the editor\"
# pick = \"C:/Users/you/Projects\"
# run = \"code {path}\"
##
## A command in WSL that needs a terminal (for sudo):
# [[shortcut]]
# name = \"upgrade\"
# about = \"Upgrade Linux packages\"
# wsl = \"sudo apt upgrade\"
# terminal = true
";

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub hotkey: String,
    /// WSL distro for `wsl` shortcuts and `>` commands; empty means WSL's default distro.
    pub wsl_distro: String,
    /// Folders to index for file search; empty means the defaults.
    pub file_roots: Vec<PathBuf>,
    /// Remember recent searches and launches; false forgets them and stops.
    pub history: bool,
    /// Currency and weather answers may go online.
    pub online: bool,
    /// City for a bare `weather`; empty means none.
    pub weather_city: String,
    /// ISO code for a bare `20 usd`; empty means the Windows region's.
    pub currency: String,
    /// `"c"`, `"f"`, or empty for the Windows region's.
    pub temperature_unit: String,
    /// Web search URL with `{arg}`; empty means DuckDuckGo.
    pub search_engine: String,
    /// Look for a new version in the background (also needs `online`).
    pub check_updates: bool,
    /// User-defined shortcuts, validated by `validate`.
    #[serde(rename = "shortcut")]
    pub shortcuts: Vec<ShortcutDef>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hotkey: "Alt+Space".to_string(),
            wsl_distro: String::new(),
            file_roots: Vec::new(),
            history: true,
            online: true,
            weather_city: String::new(),
            currency: String::new(),
            temperature_unit: String::new(),
            search_engine: String::new(),
            check_updates: true,
            shortcuts: Vec::new(),
        }
    }
}

/// One `[[shortcut]]` table.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShortcutDef {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub about: String,
    /// Windows command line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Command run in the WSL distro.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wsl: Option<String>,
    /// Path or URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open: Option<String>,
    /// Keep a terminal open; default is true for `wsl`, false for `run`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<bool>,
    /// Run elevated (`run`/`open` only).
    #[serde(default)]
    pub admin: bool,
    /// Folder whose sub-folders are the choices.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pick: Option<PathBuf>,
    #[serde(default)]
    pub choices: Vec<String>,
}

pub struct Loaded {
    pub config: Config,
    pub error: Option<String>,
}

/// `%APPDATA%\Flick\config.toml` (falls back to a relative `Flick\config.toml`).
pub fn config_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    base.join("Flick").join("config.toml")
}

/// Parses config text into a `Config`. Errors are short, with a line number when known.
pub fn parse(text: &str) -> Result<Config, String> {
    let config: Config = toml::from_str(text).map_err(|e| {
        let msg = e.message().lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(", ");
        match e.span() {
            Some(span) => {
                let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
                format!("line {line}: {msg}")
            }
            None => msg.to_string(),
        }
    })?;
    validate(&config)?;
    Ok(config)
}

/// Every `{letters}` placeholder in `text`, in order. Other braces are not placeholders.
fn placeholders(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find('{') {
        rest = &rest[i + 1..];
        if let Some(j) = rest.find('}') {
            let word = &rest[..j];
            if !word.is_empty() && word.chars().all(|c| c.is_ascii_alphabetic()) {
                found.push(word);
            }
        }
    }
    found
}

fn one_word(s: &str) -> bool {
    !s.trim().is_empty() && !s.contains(char::is_whitespace)
}

/// Checks the answer keys, then shortcut definitions in file order; the first
/// broken rule is reported.
pub fn validate(c: &Config) -> Result<(), String> {
    let currency = c.currency.trim().to_uppercase();
    if !currency.is_empty() && !crate::answers::units::CURRENCIES.contains(&currency.as_str()) {
        return Err(format!("currency \"{}\" is not one of the supported currencies", c.currency));
    }
    if !matches!(c.temperature_unit.as_str(), "" | "c" | "f") {
        return Err("temperature_unit must be \"c\" or \"f\"".into());
    }
    let engine = &c.search_engine;
    let engine_ok = engine.is_empty() || (engine.starts_with("https://") && engine.contains("{arg}"));
    if !engine_ok {
        return Err("search_engine must start with https:// and contain {arg}".into());
    }
    let mut used: Vec<String> = vec!["help".to_string()];
    for s in &c.shortcuts {
        let n = &s.name;
        let fail = |msg: &str| Err(format!("shortcut \"{n}\": {msg}"));

        if !one_word(n) || s.aliases.iter().any(|a| !one_word(a)) {
            return fail("name must be one word");
        }
        // Result ids are `kind:<name>:<value>`; aliases never appear in them.
        if n.contains(':') {
            return fail("name cannot contain :");
        }
        // `?x` is read as `? x`: only the one-character name `?` can be typed.
        if std::iter::once(n).chain(&s.aliases).any(|w| w.starts_with('?') && w.chars().count() > 1) {
            return fail("name cannot start with ?");
        }
        // `>` routes to WSL commands: such a name could never be reached.
        if std::iter::once(n).chain(&s.aliases).any(|w| w.starts_with('>')) {
            return fail("name cannot start with >");
        }
        for word in std::iter::once(n).chain(&s.aliases) {
            let lower = word.to_lowercase();
            if used.contains(&lower) {
                return fail("name is already used");
            }
            used.push(lower);
        }

        let actions = [s.run.as_ref(), s.wsl.as_ref(), s.open.as_ref()];
        if actions.iter().filter(|a| a.is_some()).count() != 1 {
            return fail("needs exactly one of run, wsl, open");
        }
        let picks = s.pick.is_some() || !s.choices.is_empty();
        if s.pick.is_some() && !s.choices.is_empty() {
            return fail("use pick or choices, not both");
        }

        let command = actions.iter().flatten().next().map_or("", |a| a.as_str());
        let found = placeholders(command);
        if picks && found.contains(&"arg") {
            return fail("{arg} cannot be combined with pick or choices");
        }
        let uses_entry = found.iter().any(|p| matches!(*p, "name" | "path"));
        if !picks && uses_entry {
            return fail("{name} and {path} need pick or choices");
        }
        if picks && !uses_entry {
            return fail("pick or choices need {name} or {path}");
        }
        if s.pick.is_none() && found.contains(&"path") {
            return fail("{path} needs pick");
        }
        if let Some(p) = found.iter().find(|p| !matches!(**p, "arg" | "name" | "path")) {
            return fail(&format!("unknown placeholder {{{p}}}"));
        }
        if s.admin && s.wsl.is_some() {
            return fail("admin is not available for wsl");
        }

        // Report a template that cannot bind its values now, not when it is used.
        let names: Vec<&str> = ["arg", "name", "path"].into_iter().filter(|n| found.contains(n)).collect();
        if s.admin && !names.is_empty() {
            return fail("admin cannot be combined with placeholders");
        }
        if let Some(run) = &s.run {
            if let Err(e) = crate::launch::bind_windows(run, &names) {
                return fail(&e);
            }
        }
        if let Some(wsl) = &s.wsl {
            let dummy: Vec<(&str, &str)> = names.iter().map(|n| (*n, "x")).collect();
            if let Err(e) = crate::launch::bind_bash(wsl, &dummy) {
                return fail(&e);
            }
        }
    }
    Ok(())
}

/// Reads the file. On error keeps `last_good` (or `Config::default()`) and returns the message.
/// Creates the file (and its folder) with commented defaults if it does not exist.
pub fn load(path: &Path, last_good: Option<&Config>) -> Loaded {
    let fallback = || last_good.cloned().unwrap_or_default();
    match std::fs::read_to_string(path) {
        Ok(text) => match parse(&text) {
            Ok(config) => Loaded { config, error: None },
            Err(e) => Loaded { config: fallback(), error: Some(format!("config.toml: {e}")) },
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let created = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|_| std::fs::write(path, DEFAULT_FILE));
            match created {
                Ok(()) => Loaded { config: Config::default(), error: None },
                Err(e) => Loaded {
                    config: fallback(),
                    error: Some(format!("config.toml: could not create: {e}")),
                },
            }
        }
        Err(e) => Loaded { config: fallback(), error: Some(format!("config.toml: could not read: {e}")) },
    }
}

/// Calls `on_change` (debounced 300 ms) whenever the file is saved. Editors save via
/// temp-file + rename, so the parent folder is watched and events are filtered by file name.
/// Dropping the returned watcher stops watching.
pub fn watch(
    path: PathBuf,
    on_change: impl Fn() + Send + 'static,
) -> notify::Result<RecommendedWatcher> {
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&dir);
    let file_name = path.file_name().map(|n| n.to_os_string());

    let (tx, rx) = mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            if event.paths.iter().any(|p| p.file_name().map(|n| n.to_os_string()) == file_name) {
                let _ = tx.send(());
            }
        }
    })?;
    watcher.watch(&dir, RecursiveMode::NonRecursive)?;

    std::thread::spawn(move || {
        // Ends when the watcher (and with it `tx`) is dropped.
        while rx.recv().is_ok() {
            while rx.recv_timeout(DEBOUNCE).is_ok() {}
            on_change();
        }
    });
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.hotkey, "Alt+Space");
        assert_eq!(c.wsl_distro, "");
        assert!(c.file_roots.is_empty());
        assert!(c.shortcuts.is_empty());
    }
    #[test] fn parses_hotkey() { assert_eq!(parse("hotkey = \"Ctrl+Space\"").unwrap().hotkey, "Ctrl+Space"); }
    #[test]
    fn check_updates_defaults_on_and_parses_false() {
        assert!(Config::default().check_updates);
        assert!(parse("").unwrap().check_updates);
        assert!(!parse("check_updates = false").unwrap().check_updates);
    }
    #[test]
    fn config_history_defaults_on_and_parses_false() {
        assert!(Config::default().history);
        assert!(parse("").unwrap().history);
        assert!(!parse("history = false").unwrap().history);
    }

    #[test] fn empty_file_is_defaults() { assert_eq!(parse("").unwrap(), Config::default()); }
    #[test] fn unknown_key_is_error() { assert!(parse("hotkye = \"x\"").is_err()); }

    #[test]
    fn error_has_line_number() {
        let e = parse("hotkey = \"a\"\nhotkey = = =").unwrap_err();
        assert!(e.starts_with("line 2:"), "{e}");
        assert!(!e.contains('\n'), "{e}");
    }

    #[test]
    fn invalid_toml_without_last_good_uses_defaults_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "hotkey = = =").unwrap();
        let l = load(&p, None);
        assert_eq!(l.config, Config::default());
        assert!(l.error.unwrap().starts_with("config.toml: line 1:"));
    }

    #[test]
    fn invalid_toml_keeps_last_good() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "hotkey = = =").unwrap();
        let good = Config { hotkey: "Ctrl+J".into(), ..Default::default() };
        let l = load(&p, Some(&good));
        assert_eq!(l.config, good);
        assert!(l.error.is_some());
    }

    #[test]
    fn missing_file_is_created_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("config.toml");
        let l = load(&p, None);
        assert!(l.error.is_none());
        assert_eq!(l.config, Config::default());
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("hotkey") && text.contains("Alt+Space"));
        assert_eq!(parse(&text).unwrap(), Config::default());
    }

    #[test]
    fn create_failure_returns_defaults_with_error() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "x").unwrap();
        let l = load(&blocker.join("config.toml"), None);
        assert_eq!(l.config, Config::default());
        let e = l.error.unwrap();
        assert!(e.starts_with("config.toml: could not create"), "{e}");
        assert!(!e.contains("config file"), "{e}");
    }

    #[test]
    fn watch_fires_on_save_via_rename() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "").unwrap();
        let (tx, rx) = mpsc::channel();
        let _w = watch(p.clone(), move || { let _ = tx.send(()); }).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let tmp = dir.path().join("config.toml.tmp");
        std::fs::write(&tmp, "hotkey = \"Ctrl+J\"").unwrap();
        std::fs::rename(&tmp, &p).unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(10)).is_ok(), "no change callback");
    }

    fn uncommented_default_file() -> String {
        DEFAULT_FILE
            .lines()
            .map(|l| l.strip_prefix("# ").unwrap_or(l))
            .collect::<Vec<_>>()
            .join("
")
    }

    fn sc(name: &str, extra: &str) -> String {
        format!("[[shortcut]]
name = \"{name}\"
{extra}
")
    }

    fn err(text: &str) -> String { parse(text).unwrap_err() }

    #[test]
    fn default_file_parses_to_default() {
        assert_eq!(parse(DEFAULT_FILE).unwrap(), Config::default());
    }

    #[test]
    fn parses_full_example() {
        let c = parse(&uncommented_default_file()).unwrap();
        assert_eq!(c.hotkey, "Alt+Space");
        assert_eq!(c.wsl_distro, "Ubuntu");
        assert_eq!(c.file_roots.len(), 3);
        assert_eq!(c.shortcuts.len(), 4);
        let base = ShortcutDef {
            name: String::new(), aliases: vec![], about: String::new(), run: None, wsl: None,
            open: None, terminal: None, admin: false, pick: None, choices: vec![],
        };
        assert_eq!(c.shortcuts[0], ShortcutDef {
            name: "docs".into(), about: "Rust documentation".into(),
            open: Some("https://docs.rs".into()), ..base.clone()
        });
        assert_eq!(c.shortcuts[1], ShortcutDef {
            name: "search".into(), aliases: vec!["s".into()], about: "Web search".into(),
            open: Some("https://duckduckgo.com/?q={arg}".into()), ..base.clone()
        });
        assert_eq!(c.shortcuts[2], ShortcutDef {
            name: "proj".into(), about: "Open a project in the editor".into(),
            run: Some("code {path}".into()), pick: Some(PathBuf::from("C:/Users/you/Projects")),
            ..base.clone()
        });
        assert_eq!(c.shortcuts[3], ShortcutDef {
            name: "upgrade".into(), about: "Upgrade Linux packages".into(),
            wsl: Some("sudo apt upgrade".into()), terminal: Some(true), ..base
        });
    }

    #[test]
    fn name_must_be_one_word() {
        assert_eq!(err(&sc("", "open = \"x\"")), "shortcut \"\": name must be one word");
        assert_eq!(err(&sc("  ", "open = \"x\"")), "shortcut \"  \": name must be one word");
        assert_eq!(err(&sc("two words", "open = \"x\"")), "shortcut \"two words\": name must be one word");
        assert_eq!(
            err(&sc("a", "open = \"x\"
aliases = [\"b c\"]")),
            "shortcut \"a\": name must be one word"
        );
    }

    #[test]
    fn names_must_be_unique() {
        let dup = format!("{}{}", sc("Foo", "open = \"x\""), sc("foo", "open = \"x\""));
        assert_eq!(err(&dup), "shortcut \"foo\": name is already used");
        let alias = format!("{}{}", sc("a", "open = \"x\""), sc("b", "open = \"x\"
aliases = [\"A\"]"));
        assert_eq!(err(&alias), "shortcut \"b\": name is already used");
        assert_eq!(err(&sc("help", "open = \"x\"")), "shortcut \"help\": name is already used");
        assert_eq!(
            err(&sc("a", "open = \"x\"
aliases = [\"HELP\"]")),
            "shortcut \"a\": name is already used"
        );
        assert_eq!(
            err(&sc("a", "open = \"x\"
aliases = [\"A\"]")),
            "shortcut \"a\": name is already used"
        );
    }

    #[test]
    fn needs_exactly_one_action() {
        let m = "shortcut \"a\": needs exactly one of run, wsl, open";
        assert_eq!(err(&sc("a", "")), m);
        assert_eq!(err(&sc("a", "run = \"x\"
open = \"y\"")), m);
        assert_eq!(err(&sc("a", "run = \"x\"
wsl = \"y\"
open = \"z\"")), m);
    }

    #[test]
    fn pick_or_choices_not_both() {
        assert_eq!(
            err(&sc("a", "run = \"x {name}\"
pick = \"C:/p\"
choices = [\"q\"]")),
            "shortcut \"a\": use pick or choices, not both"
        );
    }

    #[test]
    fn arg_not_with_pick_or_choices() {
        let m = "shortcut \"a\": {arg} cannot be combined with pick or choices";
        assert_eq!(err(&sc("a", "run = \"x {arg} {name}\"
pick = \"C:/p\"")), m);
        assert_eq!(err(&sc("a", "run = \"x {arg} {name}\"
choices = [\"q\"]")), m);
    }

    #[test]
    fn name_path_need_pick_or_choices() {
        let m = "shortcut \"a\": {name} and {path} need pick or choices";
        assert_eq!(err(&sc("a", "run = \"x {name}\"")), m);
        assert_eq!(err(&sc("a", "wsl = \"x {path}\"")), m);
    }

    #[test]
    fn pick_or_choices_need_name_or_path() {
        let m = "shortcut \"a\": pick or choices need {name} or {path}";
        assert_eq!(err(&sc("a", "run = \"x\"
pick = \"C:/p\"")), m);
        assert_eq!(err(&sc("a", "run = \"x\"
choices = [\"q\"]")), m);
    }

    #[test]
    fn unknown_placeholder() {
        assert_eq!(
            err(&sc("a", "run = \"x {foo}\"")),
            "shortcut \"a\": unknown placeholder {foo}"
        );
        assert_eq!(
            err(&sc("a", "run = \"x {Arg}\"")),
            "shortcut \"a\": unknown placeholder {Arg}"
        );
    }

    #[test]
    fn other_braces_are_left_alone() {
        let c = parse(&sc("a", "run = \"powershell -c 'ls | % { $_ } {}' {arg}\"")).unwrap();
        assert_eq!(c.shortcuts.len(), 1);
        assert!(parse(&sc("a", "run = \"x {\\\"k\\\": 1} {a1}\"")).is_ok());
    }

    #[test]
    fn admin_not_for_wsl() {
        assert_eq!(
            err(&sc("a", "wsl = \"x\"
admin = true")),
            "shortcut \"a\": admin is not available for wsl"
        );
        assert!(parse(&sc("a", "run = \"x\"
admin = true")).is_ok());
    }

    #[test]
    fn admin_cannot_take_placeholders() {
        assert_eq!(
            err(&sc("a", "run = \"x {arg}\"
admin = true")),
            "shortcut \"a\": admin cannot be combined with placeholders"
        );
        assert_eq!(
            err(&sc("a", "run = \"x {name}\"
choices = [\"q\"]
admin = true")),
            "shortcut \"a\": admin cannot be combined with placeholders"
        );
        assert_eq!(
            err(&sc("a", "open = \"x {arg}\"
admin = true")),
            "shortcut \"a\": admin cannot be combined with placeholders"
        );
        assert!(parse(&sc("a", "open = \"x\"
admin = true")).is_ok());
        assert!(parse(&sc("a", "run = \"x\"
admin = true")).is_ok());
    }

    #[test]
    fn question_mark_is_a_valid_name() {
        // The built-in web search is named `?`; a config shortcut may replace it.
        assert!(parse(&sc("?", "open = \"https://example.com/?q={arg}\"")).is_ok());
    }

    #[test]
    fn names_starting_with_question_mark_other_than_it_are_rejected() {
        assert_eq!(err(&sc("?docs", "open = \"x\"")), "shortcut \"?docs\": name cannot start with ?");
        assert_eq!(
            err(&sc("x", "aliases = [\"?y\"]\nopen = \"x\"")),
            "shortcut \"x\": name cannot start with ?"
        );
        assert!(parse(&sc("?", "open = \"x\"")).is_ok());
    }

    #[test]
    fn name_cannot_start_with_greater_than() {
        // `>` routes to WSL: such a shortcut could never be reached.
        assert_eq!(err(&sc(">x", "open = \"x\"")), "shortcut \">x\": name cannot start with >");
        assert_eq!(
            err(&sc("x", "aliases = [\">y\"]\nopen = \"x\"")),
            "shortcut \"x\": name cannot start with >"
        );
        assert!(parse(&sc("x>y", "open = \"x\"")).is_ok());
    }

    #[test]
    fn name_cannot_contain_colon() {
        assert_eq!(err(&sc("a:b", "open = \"x\"")), "shortcut \"a:b\": name cannot contain :");
        assert_eq!(err(&sc(":", "open = \"x\"")), "shortcut \":\": name cannot contain :");
        // Aliases never appear in ids.
        assert!(parse(&sc("a", "open = \"x\"
aliases = [\"b:c\"]")).is_ok());
    }

    #[test]
    fn path_needs_pick() {
        assert_eq!(
            err(&sc("a", "run = \"x {path}\"
choices = [\"q\"]")),
            "shortcut \"a\": {path} needs pick"
        );
        assert_eq!(
            err(&sc("a", "run = \"x {name} {path}\"
choices = [\"q\"]")),
            "shortcut \"a\": {path} needs pick"
        );
        assert!(parse(&sc("a", "run = \"x {path}\"
pick = \"C:/p\"")).is_ok());
    }

    #[test]
    fn run_template_that_cannot_bind_is_reported() {
        assert_eq!(
            err(&sc("a", "run = \"echo hi! {arg}\"")),
            "shortcut \"a\": this command cannot use ! together with a placeholder"
        );
        assert!(parse(&sc("a", "run = \"echo hi!\"")).is_ok());
    }

    #[test]
    fn wsl_template_that_cannot_bind_is_reported() {
        assert_eq!(
            err(&sc("a", "wsl = \"echo 'pre {arg} post'\"")),
            "shortcut \"a\": put {arg} outside single quotes in this command"
        );
        assert_eq!(
            err(&sc("a", "wsl = \"echo please read {arg}\"")),
            "shortcut \"a\": this command cannot use read together with a placeholder"
        );
        assert!(parse(&sc("a", "wsl = \"echo '{arg}'\"")).is_ok());
        assert!(parse(&sc("a", "wsl = \"echo 'pre' {arg}\"")).is_ok());
    }

    #[test]
    fn typo_in_shortcut_key_is_error() {
        let e = err(&sc("a", "runn = \"x\""));
        assert!(e.starts_with("line "), "{e}");
    }

    #[test]
    fn invalid_shortcut_keeps_last_good() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, sc("a", "")).unwrap();
        let good = Config { hotkey: "Ctrl+J".into(), ..Default::default() };
        let l = load(&p, Some(&good));
        assert_eq!(l.config, good);
        assert_eq!(l.error.unwrap(), "config.toml: shortcut \"a\": needs exactly one of run, wsl, open");
    }

    #[test]
    fn config_validates_new_keys() {
        let c = parse("").unwrap();
        assert!(c.online);
        assert_eq!((c.weather_city.as_str(), c.currency.as_str()), ("", ""));
        assert_eq!((c.temperature_unit.as_str(), c.search_engine.as_str()), ("", ""));
        let c = parse("online = false
weather_city = \"London\"
currency = \"usd\"
temperature_unit = \"f\"
search_engine = \"https://www.google.com/search?q={arg}\"").unwrap();
        assert!(!c.online);
        assert_eq!(c.weather_city, "London");
        assert_eq!(c.currency, "usd");
        assert_eq!(c.temperature_unit, "f");
        assert!(err("currency = \"XYZ\"").contains("currency"));
        assert!(err("temperature_unit = \"k\"").contains("temperature_unit"));
        assert!(err("search_engine = \"https://example.com/?q=\"").contains("search_engine"));
        assert!(err("search_engine = \"http://example.com/?q={arg}\"").contains("search_engine"));
    }

    #[test]
    fn full_example_sets_answer_keys() {
        let c = parse(&uncommented_default_file()).unwrap();
        assert!(c.online);
        assert_eq!(c.weather_city, "London");
        assert_eq!(c.currency, "EUR");
        assert_eq!(c.temperature_unit, "c");
        assert_eq!(c.search_engine, "https://duckduckgo.com/?q={arg}");
    }
}
