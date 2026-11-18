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
## win_key: the Win key alone opens Flick instead of the Start menu; Win combinations keep working, Ctrl+Esc still opens Start (true: on).
# win_key = false
##
## show_taskbar: a taskbar that hides by itself comes up while Flick is open, with a Flick button on it (true: on).
# show_taskbar = false
##
## theme: dark, light, midnight, frost, ember, ocean, forest, grape, rose, sand, mint, sakura, glass or custom (your own colours, below).
# theme = \"dark\"
## custom_theme: the colours of theme = \"custom\". Any you leave out are taken from dark.
# custom_theme = { background = \"#101418\", surface = \"#1c232b\", border = \"#26303a\", text = \"#e8eef4\", muted = \"#9fb0c0\", accent = \"#4fd1c5\", error = \"#f0717a\", warning = \"#f2b441\" }
## animations: minimal, medium or high.
# animations = \"minimal\"
##
## Shortcuts: type the name (or an alias), then press Space for arguments or choices.
## Each needs exactly one of run (Windows command), wsl (command in the distro) or open (path or URL).
## Placeholders: {arg} is the text typed after the name; with pick (folder whose sub-folders
## are the choices) or choices (a list of texts), {name} is the picked entry and {path} its full path.
## icon (optional): a program, shortcut or .ico file (the row shows its icon) or a .png file (its picture).
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
##
## Workspaces: type the name and press Enter to open every item in order. Set them up in Settings.
# [[workspace]]
# name = \"work\"
# items = [
#     { provider = \"apps\", id = \"C:/ProgramData/Microsoft/Windows/Start Menu/Programs/Outlook.lnk\", title = \"Outlook\" },
#     { provider = \"bookmarks\", id = \"https://example.com/\", title = \"Example\" },
# ]
##
## Direct hotkeys: keys that open one item from anywhere, without the bar. Set them up in Settings.
# [[direct_hotkey]]
# keys = \"Ctrl+Alt+T\"
# item = { provider = \"apps\", id = \"C:/ProgramData/Microsoft/Windows/Start Menu/Programs/Windows Terminal.lnk\", title = \"Windows Terminal\" }
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
    /// The Win key pressed alone opens the bar instead of the Start menu.
    pub win_key: bool,
    /// An auto-hidden taskbar is brought up while the bar is open.
    pub show_taskbar: bool,
    /// The bar's look: one of [`crate::theme::THEMES`]; empty means dark.
    pub theme: String,
    /// The colours of `theme = "custom"`.
    #[serde(skip_serializing_if = "crate::theme::CustomTheme::is_empty")]
    pub custom_theme: crate::theme::CustomTheme,
    /// How much moves: one of [`crate::theme::ANIMATIONS`]; empty means minimal.
    pub animations: String,
    /// User-defined shortcuts, validated by `validate`.
    #[serde(rename = "shortcut")]
    pub shortcuts: Vec<ShortcutDef>,
    /// Named sets of items opened together, validated by `validate`.
    #[serde(rename = "workspace")]
    pub workspaces: Vec<WorkspaceDef>,
    /// Global key combos that activate one item each (`hotkey` is the bar's own key).
    #[serde(rename = "direct_hotkey")]
    pub direct_hotkeys: Vec<DirectHotkeyDef>,
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
            win_key: false,
            show_taskbar: false,
            theme: String::new(),
            custom_theme: crate::theme::CustomTheme::default(),
            animations: String::new(),
            shortcuts: Vec::new(),
            workspaces: Vec::new(),
            direct_hotkeys: Vec::new(),
        }
    }
}

/// A reference to something Flick can show: the pin key (provider + id) plus the title
/// shown when it cannot be resolved.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ItemRef {
    pub provider: String,
    pub id: String,
    #[serde(default)]
    pub title: String,
}

/// One `[[workspace]]` table.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDef {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub items: Vec<ItemRef>,
}

/// One `[[direct_hotkey]]` table.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct DirectHotkeyDef {
    pub keys: String,
    pub item: ItemRef,
}

pub const WEAK_COMBO: &str = "Use two modifiers (like Ctrl+Alt) or Win";
pub const BAR_COMBO: &str = "This is Flick's own hotkey";
pub const DUPLICATE_COMBO: &str = "Already used by another hotkey";

const MODIFIERS: [&str; 4] = ["ctrl", "alt", "shift", "super"];

/// The modifier `part` (lower case) stands for, if any.
fn modifier(part: &str) -> Option<&'static str> {
    match part {
        "ctrl" | "control" => Some("ctrl"),
        "alt" | "option" => Some("alt"),
        "shift" => Some("shift"),
        "super" | "win" | "meta" | "cmd" | "command" => Some("super"),
        // What global-hotkey reads these as on Windows.
        "cmdorctrl" | "cmdorcontrol" | "commandorctrl" | "commandorcontrol" => Some("ctrl"),
        _ => None,
    }
}

/// One spelling for every name global-hotkey accepts for the same key (`part` is lower
/// case): `KeyT` is `t`, `Digit1` is `1`, `Esc` is `escape`, `ArrowDown` is `down`,
/// `Num0` is `numpad0`, `-` is `minus`, and so on.
fn canonical_key(part: &str) -> String {
    if let Some(rest) = part.strip_prefix("key").filter(|r| r.len() == 1 && r.chars().all(|c| c.is_ascii_lowercase())) {
        return rest.to_string();
    }
    if let Some(rest) = part.strip_prefix("digit").filter(|r| r.len() == 1 && r.chars().all(|c| c.is_ascii_digit())) {
        return rest.to_string();
    }
    if let Some(rest) = part.strip_prefix("arrow").filter(|r| matches!(*r, "up" | "down" | "left" | "right")) {
        return rest.to_string();
    }
    let numpad = |rest: &str| match rest {
        "plus" => Some("add".to_string()),
        r if r.len() == 1 && r.chars().all(|c| c.is_ascii_digit()) => Some(r.to_string()),
        "add" | "decimal" | "divide" | "enter" | "equal" | "multiply" | "subtract" => Some(rest.to_string()),
        _ => None,
    };
    if let Some(rest) = part.strip_prefix("numpad").or_else(|| part.strip_prefix("num")).and_then(numpad) {
        return format!("numpad{rest}");
    }
    let same = match part {
        "`" => "backquote",
        "\\" => "backslash",
        "[" => "bracketleft",
        "]" => "bracketright",
        "," => "comma",
        "=" => "equal",
        "-" => "minus",
        "." => "period",
        "'" => "quote",
        ";" => "semicolon",
        "/" => "slash",
        "pausebreak" => "pause",
        "esc" => "escape",
        "volumedown" => "audiovolumedown",
        "volumeup" => "audiovolumeup",
        "volumemute" => "audiovolumemute",
        "mediatrackprev" => "mediatrackprevious",
        other => other,
    };
    same.to_string()
}

/// One spelling per combo: lower case, `win`/`meta`/`cmd` as `super`, `CmdOrCtrl` as
/// `ctrl`, every key in one spelling (`canonical_key`), modifiers in the order ctrl, alt,
/// shift, super, then the key(s).
pub fn normalize_combo(keys: &str) -> String {
    let mut mods = [false; 4];
    let mut rest = Vec::new();
    for part in keys.split('+').map(|p| p.trim().to_lowercase()).filter(|p| !p.is_empty()) {
        match modifier(&part) {
            Some(m) => mods[MODIFIERS.iter().position(|x| *x == m).unwrap_or(0)] = true,
            None => rest.push(canonical_key(&part)),
        }
    }
    let mut out: Vec<String> = MODIFIERS.iter().zip(mods).filter(|(_, on)| *on).map(|(m, _)| m.to_string()).collect();
    out.extend(rest);
    out.join("+")
}

/// One non-modifier key plus at least two of Ctrl/Alt/Shift, or Win with anything.
pub fn combo_allowed(keys: &str) -> bool {
    let normal = normalize_combo(keys);
    let parts: Vec<&str> = normal.split('+').filter(|p| !p.is_empty()).collect();
    let keys_count = parts.iter().filter(|p| !MODIFIERS.contains(p)).count();
    let plain = parts.iter().filter(|p| matches!(**p, "ctrl" | "alt" | "shift")).count();
    keys_count == 1 && (parts.contains(&"super") || plain >= 2)
}

/// Said when a workspace name or alias is a word Flick answers to itself.
pub const COMMAND_NAME: &str = "name is a Flick command";

/// True when `word` is something Flick answers to itself, so a workspace by
/// that name could never be opened by typing it: `help`, a route (`v`, `qr`,
/// `dl`, `recent`, anything starting with `>`), a word a provider claims
/// (`timer`, `remind`, `tr`, `def`, `connect`, `wifi`, `eject`, a path such as
/// `~\x`, …) or one that shows its own row (`snip`, `color`, …). Taken from the
/// providers' own lists.
pub fn is_command_word(word: &str) -> bool {
    use crate::providers::{chores, clips, devices, path, qr, recent, screen, timers, words, wsl};
    let word = word.trim();
    if path::looks_like_path(word) {
        return true;
    }
    let routes = [clips::ROUTE, qr::ROUTE, recent::DOWNLOADS_ROUTE, recent::RECENT_ROUTE, wsl::ROUTE];
    let routed = routes.iter().any(|r| {
        if r.needs_space {
            r.prefix.eq_ignore_ascii_case(word)
        } else {
            word.starts_with(r.prefix)
        }
    });
    routed
        || std::iter::once(crate::engine::HELP)
            .chain(timers::WORDS)
            .chain(words::WORDS)
            .chain(devices::WORDS)
            .chain([chores::EJECT])
            .chain(screen::SNIP_WORDS)
            .chain(screen::COLOR_WORDS)
            .any(|w| w.eq_ignore_ascii_case(word))
}

/// Checks one workspace; `used` holds the lowercase names and aliases taken so far and
/// grows with this workspace's own.
fn check_workspace(def: &WorkspaceDef, used: &mut Vec<String>) -> Result<(), String> {
    if !one_word(&def.name) || def.aliases.iter().any(|a| !one_word(a)) {
        return Err("name must be one word".into());
    }
    if std::iter::once(&def.name).chain(&def.aliases).any(|w| w.contains(':')) {
        return Err("name cannot contain :".into());
    }
    if std::iter::once(&def.name).chain(&def.aliases).any(|w| is_command_word(w)) {
        return Err(COMMAND_NAME.into());
    }
    for word in std::iter::once(&def.name).chain(&def.aliases) {
        let lower = word.to_lowercase();
        if used.contains(&lower) {
            return Err("name is already used".into());
        }
        used.push(lower);
    }
    if def.items.is_empty() {
        return Err("needs at least one item".into());
    }
    if def.items.iter().any(|i| !item_ok(i)) {
        return Err("an item needs a provider and an id".into());
    }
    Ok(())
}

fn item_ok(item: &ItemRef) -> bool {
    !item.provider.trim().is_empty() && !item.id.trim().is_empty()
}

/// Checks `def` against the other workspaces of `config` (the one named `original` is
/// left out, as it is being replaced) and Flick's own command words.
pub fn check_workspace_in(config: &Config, original: Option<&str>, def: &WorkspaceDef) -> Result<(), String> {
    let mut used: Vec<String> = Vec::new();
    for w in config.workspaces.iter().filter(|w| Some(w.name.as_str()) != original) {
        used.extend(std::iter::once(&w.name).chain(&w.aliases).map(|n| n.to_lowercase()));
    }
    check_workspace(def, &mut used)
}

/// Checks one direct hotkey; `used` holds the normalized combos taken so far and grows
/// with this one's. A weak combo is not an error here (Settings reports it).
fn check_direct_hotkey(def: &DirectHotkeyDef, used: &mut Vec<String>) -> Result<(), String> {
    if def.keys.trim().is_empty() {
        return Err("keys are empty".into());
    }
    if !item_ok(&def.item) {
        return Err("an item needs a provider and an id".into());
    }
    let combo = normalize_combo(&def.keys);
    if used.contains(&combo) {
        return Err(DUPLICATE_COMBO.into());
    }
    used.push(combo);
    Ok(())
}

/// Checks `def` against the other direct hotkeys of `config` (the one whose keys equal
/// `original`, in any spelling, is left out).
pub fn check_direct_hotkey_in(config: &Config, original: Option<&str>, def: &DirectHotkeyDef) -> Result<(), String> {
    let original = original.map(normalize_combo);
    let mut used: Vec<String> = config
        .direct_hotkeys
        .iter()
        .map(|h| normalize_combo(&h.keys))
        .filter(|c| Some(c) != original.as_ref())
        .collect();
    check_direct_hotkey(def, &mut used)
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
    /// The row's icon: a file (a program, shortcut or `.ico` shows its icon,
    /// a `.png` the picture) or a Store app, `shell:AppsFolder\<id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
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
    crate::theme::validate(&c.theme, &c.custom_theme, &c.animations)?;
    let engine = &c.search_engine;
    let engine_ok = engine.is_empty() || (engine.starts_with("https://") && engine.contains("{arg}"));
    if !engine_ok {
        return Err("search_engine must start with https:// and contain {arg}".into());
    }
    let mut used: Vec<String> = vec!["help".to_string()];
    for s in &c.shortcuts {
        check_shortcut(s, &mut used)
            .map_err(|e| format!("shortcut \"{}\": {}", s.name, e.message))?;
    }
    let mut used: Vec<String> = Vec::new();
    for w in &c.workspaces {
        check_workspace(w, &mut used).map_err(|e| format!("workspace \"{}\": {e}", w.name))?;
    }
    let mut used = Vec::new();
    for h in &c.direct_hotkeys {
        check_direct_hotkey(h, &mut used).map_err(|e| format!("direct_hotkey \"{}\": {e}", h.keys))?;
    }
    Ok(())
}

/// The field of a shortcut a problem is about (`Target` is whichever of run/wsl/open is set).
#[derive(serde::Serialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Name,
    Aliases,
    Target,
    /// No rule reports it yet; kept so the Settings form has a name for the field.
    #[allow(dead_code)]
    Terminal,
    Admin,
    Pick,
    Choices,
}

#[derive(serde::Serialize, Debug, PartialEq)]
pub struct FieldError {
    pub field: Field,
    pub message: String,
}

/// Checks one shortcut; `used` holds the lowercase names and aliases taken so far and
/// grows with this shortcut's own. The first broken rule is reported.
pub fn check_shortcut(s: &ShortcutDef, used: &mut Vec<String>) -> Result<(), FieldError> {
    let n = &s.name;
    let fail = |field: Field, msg: &str| Err(FieldError { field, message: msg.to_string() });
    // Which field a rule about the name and aliases concerns.
    let word_field = |bad: &dyn Fn(&str) -> bool| if bad(n) { Field::Name } else { Field::Aliases };

    if !one_word(n) {
        return fail(Field::Name, "name must be one word");
    }
    if s.aliases.iter().any(|a| !one_word(a)) {
        return fail(Field::Aliases, "name must be one word");
    }
    // Result ids are `kind:<name>:<value>`; aliases never appear in them.
    if n.contains(':') {
        return fail(Field::Name, "name cannot contain :");
    }
    // `?x` is read as `? x`: only the one-character name `?` can be typed.
    let bad_q = |w: &str| w.starts_with('?') && w.chars().count() > 1;
    if std::iter::once(n).chain(&s.aliases).any(|w| bad_q(w)) {
        return fail(word_field(&bad_q), "name cannot start with ?");
    }
    // `>` routes to WSL commands: such a name could never be reached.
    let bad_gt = |w: &str| w.starts_with('>');
    if std::iter::once(n).chain(&s.aliases).any(|w| bad_gt(w)) {
        return fail(word_field(&bad_gt), "name cannot start with >");
    }
    for (i, word) in std::iter::once(n).chain(&s.aliases).enumerate() {
        let lower = word.to_lowercase();
        if used.contains(&lower) {
            return fail(if i == 0 { Field::Name } else { Field::Aliases }, "name is already used");
        }
        used.push(lower);
    }

    let actions = [s.run.as_ref(), s.wsl.as_ref(), s.open.as_ref()];
    if actions.iter().filter(|a| a.is_some()).count() != 1 {
        return fail(Field::Target, "needs exactly one of run, wsl, open");
    }
    let picks = s.pick.is_some() || !s.choices.is_empty();
    if s.pick.is_some() && !s.choices.is_empty() {
        return fail(Field::Choices, "use pick or choices, not both");
    }

    let command = actions.iter().flatten().next().map_or("", |a| a.as_str());
    let found = placeholders(command);
    if picks && found.contains(&"arg") {
        return fail(Field::Target, "{arg} cannot be combined with pick or choices");
    }
    let uses_entry = found.iter().any(|p| matches!(*p, "name" | "path"));
    if !picks && uses_entry {
        return fail(Field::Target, "{name} and {path} need pick or choices");
    }
    if picks && !uses_entry {
        let field = if s.pick.is_some() { Field::Pick } else { Field::Choices };
        return fail(field, "pick or choices need {name} or {path}");
    }
    if s.pick.is_none() && found.contains(&"path") {
        return fail(Field::Target, "{path} needs pick");
    }
    if let Some(p) = found.iter().find(|p| !matches!(**p, "arg" | "name" | "path")) {
        return fail(Field::Target, &format!("unknown placeholder {{{p}}}"));
    }
    if s.admin && s.wsl.is_some() {
        return fail(Field::Admin, "admin is not available for wsl");
    }

    // Report a template that cannot bind its values now, not when it is used.
    let names: Vec<&str> = ["arg", "name", "path"].into_iter().filter(|n| found.contains(n)).collect();
    if s.admin && !names.is_empty() {
        return fail(Field::Admin, "admin cannot be combined with placeholders");
    }
    if let Some(run) = &s.run {
        if let Err(e) = crate::launch::bind_windows(run, &names) {
            return fail(Field::Target, &e);
        }
    }
    if let Some(wsl) = &s.wsl {
        let dummy: Vec<(&str, &str)> = names.iter().map(|n| (*n, "x")).collect();
        if let Err(e) = crate::launch::bind_bash(wsl, &dummy) {
            return fail(Field::Target, &e);
        }
    }
    Ok(())
}

/// Checks `def` against the other shortcuts of `config` (the one named `original` is
/// left out, as it is being replaced) and the built-in `help`.
pub fn check_shortcut_in(config: &Config, original: Option<&str>, def: &ShortcutDef) -> Result<(), FieldError> {
    let mut used: Vec<String> = vec!["help".to_string()];
    for s in config.shortcuts.iter().filter(|s| Some(s.name.as_str()) != original) {
        used.extend(std::iter::once(&s.name).chain(&s.aliases).map(|w| w.to_lowercase()));
    }
    check_shortcut(def, &mut used)
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
    fn check_shortcut_in_rejects_the_builtin_help_name() {
        let def: ShortcutDef = toml::from_str("name = \"Help\"
run = \"notepad\"").unwrap();
        let err = check_shortcut_in(&Config::default(), None, &def).unwrap_err();
        assert_eq!(err.field, Field::Name);
        assert_eq!(err.message, "name is already used");
    }

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
    fn show_taskbar_defaults_off_and_parses_true() {
        assert!(!Config::default().show_taskbar);
        assert!(!parse("").unwrap().show_taskbar);
        assert!(parse("show_taskbar = true").unwrap().show_taskbar);
    }

    #[test]
    fn theme_settings_parse_and_wrong_ones_are_errors() {
        let c = parse("").unwrap();
        assert_eq!((c.theme.as_str(), c.animations.as_str()), ("", ""));
        assert!(c.custom_theme.is_empty());
        let c = parse("theme = \"frost\"\nanimations = \"high\"").unwrap();
        assert_eq!((c.theme.as_str(), c.animations.as_str()), ("frost", "high"));
        assert!(validate(&c).is_ok());
        // Both ways of writing the custom colours.
        let inline = parse("theme = \"custom\"\ncustom_theme = { background = \"#101418\", accent = \"#4fc\" }").unwrap();
        let table = parse("theme = \"custom\"\n[custom_theme]\nbackground = \"#101418\"\naccent = \"#4fc\"").unwrap();
        assert_eq!(inline.custom_theme, table.custom_theme);
        assert_eq!(inline.custom_theme.background.as_deref(), Some("#101418"));
        assert!(validate(&inline).is_ok());
        for bad in ["theme = \"neon\"", "animations = \"wild\"", "custom_theme = { text = \"white\" }"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(parse("custom_theme = { colour = \"#fff\" }").is_err(), "an unknown colour name");
        // The default file's examples are valid once uncommented.
        let on: String = DEFAULT_FILE
            .lines()
            .filter(|l| l.starts_with("# theme") || l.starts_with("# custom_theme") || l.starts_with("# animations"))
            .map(|l| format!("{}\n", &l[2..]))
            .collect();
        let c = parse(&on).unwrap();
        assert!(validate(&c).is_ok());
        assert_eq!(c.custom_theme.entries().iter().filter(|(_, v)| v.is_some()).count(), 8);
    }

    #[test]
    fn win_key_defaults_off_and_parses_true() {
        assert!(!Config::default().win_key);
        assert!(!parse("").unwrap().win_key);
        assert!(parse("win_key = true").unwrap().win_key);
    }

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
        assert_eq!(c.workspaces.len(), 1);
        assert_eq!(c.direct_hotkeys.len(), 1);
        let base = ShortcutDef {
            name: String::new(), aliases: vec![], about: String::new(), run: None, wsl: None,
            open: None, terminal: None, admin: false, pick: None, choices: vec![], icon: None,
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

    fn def(name: &str) -> ShortcutDef {
        ShortcutDef {
            name: name.into(), aliases: vec![], about: String::new(), run: None, wsl: None,
            open: Some("x".into()), terminal: None, admin: false, pick: None, choices: vec![], icon: None,
        }
    }

    #[test]
    fn field_for_each_rule() {
        let cases: Vec<(ShortcutDef, Field, &str)> = vec![
            (def(""), Field::Name, "name must be one word"),
            (ShortcutDef { aliases: vec!["b c".into()], ..def("a") }, Field::Aliases, "name must be one word"),
            (ShortcutDef { run: Some("y".into()), ..def("a") }, Field::Target, "needs exactly one of run, wsl, open"),
            (
                ShortcutDef { open: Some("x {name}".into()), pick: Some("C:/p".into()), choices: vec!["q".into()], ..def("a") },
                Field::Choices,
                "use pick or choices, not both",
            ),
            (ShortcutDef { open: Some("x {path}".into()), ..def("a") }, Field::Target, "{name} and {path} need pick or choices"),
            (
                ShortcutDef { open: Some("x {arg} {name}".into()), choices: vec!["q".into()], ..def("a") },
                Field::Target,
                "{arg} cannot be combined with pick or choices",
            ),
            (
                ShortcutDef { open: None, wsl: Some("x".into()), admin: true, ..def("a") },
                Field::Admin,
                "admin is not available for wsl",
            ),
            (
                ShortcutDef { open: Some("x {arg}".into()), admin: true, ..def("a") },
                Field::Admin,
                "admin cannot be combined with placeholders",
            ),
            (def("help"), Field::Name, "name is already used"),
        ];
        for (d, field, message) in cases {
            let e = check_shortcut(&d, &mut vec!["help".to_string()]).unwrap_err();
            assert_eq!((e.field, e.message.as_str()), (field, message), "{d:?}");
        }
    }

    #[test]
    fn check_in_excludes_the_original() {
        let c = Config { shortcuts: vec![def("s1"), def("s2")], ..Default::default() };
        assert!(check_shortcut_in(&c, Some("s1"), &def("s1")).is_ok());
        assert!(check_shortcut_in(&c, None, &def("s3")).is_ok());
        let e = check_shortcut_in(&c, Some("s2"), &def("s1")).unwrap_err();
        assert_eq!(e.field, Field::Name);
        assert_eq!(e.message, "name is already used");
        let c = Config { shortcuts: vec![ShortcutDef { aliases: vec!["z".into()], ..def("s1") }], ..Default::default() };
        let e = check_shortcut_in(&c, None, &def("z")).unwrap_err();
        assert_eq!(e.field, Field::Name);
    }

    fn item(provider: &str, id: &str, title: &str) -> ItemRef {
        ItemRef { provider: provider.into(), id: id.into(), title: title.into() }
    }

    fn ws(name: &str) -> WorkspaceDef {
        WorkspaceDef { name: name.into(), aliases: vec![], items: vec![item("apps", "a.lnk", "A")] }
    }

    fn hk(keys: &str) -> DirectHotkeyDef {
        DirectHotkeyDef { keys: keys.into(), item: item("apps", "a.lnk", "A") }
    }

    #[test]
    fn workspaces_and_direct_hotkeys_parse() {
        let c = parse("[[workspace]]
name = \"work\"
aliases = [\"w\"]
items = [
    { provider = \"apps\", id = \"C:/x/Mail.lnk\", title = \"Mail\" },
    { provider = \"bookmarks\", id = \"https://example.com/\" },
]

[[direct_hotkey]]
keys = \"Ctrl+Alt+T\"
item = { provider = \"apps\", id = \"C:/x/Term.lnk\", title = \"Term\" }
").unwrap();
        assert_eq!(c.workspaces, [WorkspaceDef {
            name: "work".into(),
            aliases: vec!["w".into()],
            items: vec![item("apps", "C:/x/Mail.lnk", "Mail"), item("bookmarks", "https://example.com/", "")],
        }]);
        assert_eq!(c.direct_hotkeys, [DirectHotkeyDef { keys: "Ctrl+Alt+T".into(), item: item("apps", "C:/x/Term.lnk", "Term") }]);
        assert!(Config::default().workspaces.is_empty() && Config::default().direct_hotkeys.is_empty());
        assert!(parse("[[workspace]]\nname = \"a\"\nitems = [{ provider = \"apps\", id = \"x\", tilte = \"t\" }]").is_err());
    }

    #[test]
    fn default_file_examples_parse() {
        assert_eq!(parse(DEFAULT_FILE).unwrap(), Config::default());
        let c = parse(&uncommented_default_file()).unwrap();
        assert_eq!(c.workspaces.len(), 1);
        assert_eq!(c.workspaces[0].name, "work");
        assert_eq!(c.workspaces[0].items.len(), 2);
        assert_eq!(c.workspaces[0].items[0].provider, "apps");
        assert_eq!(c.workspaces[0].items[1].provider, "bookmarks");
        assert_eq!(c.workspaces[0].items[1].id, "https://example.com/");
        assert_eq!(c.direct_hotkeys.len(), 1);
        assert_eq!(c.direct_hotkeys[0].keys, "Ctrl+Alt+T");
        assert!(DEFAULT_FILE.matches("Set them up in Settings.").count() == 2);
    }

    #[test]
    fn workspace_rules() {
        let none = Config::default();
        let check = |d: &WorkspaceDef| check_workspace_in(&none, None, d).unwrap_err();
        assert_eq!(check(&ws("two words")), "name must be one word");
        assert_eq!(check(&WorkspaceDef { aliases: vec!["b c".into()], ..ws("a") }), "name must be one word");
        assert_eq!(check(&ws("a:b")), "name cannot contain :");
        assert_eq!(check(&ws("HELP")), "name is a Flick command");
        assert_eq!(check(&WorkspaceDef { aliases: vec!["help".into()], ..ws("a") }), "name is a Flick command");
        for word in [
            "timer", "Timers", "remind", "reminders", "tr", "DEF", "connect", "disconnect", "wifi", "eject", "v", "dl",
            "recent", "qr", "snip", "screenshot", "color", "colour", "picker", ">", ">top", r"~\x", "//srv",
        ] {
            assert_eq!(check(&ws(word)), "name is a Flick command", "{word}");
            assert_eq!(check(&WorkspaceDef { aliases: vec![word.into()], ..ws("a") }), "name is a Flick command", "{word}");
        }
        for word in ["work", "timerx", "vv", "trip", "define", "wifi2", "ejector", "a>b"] {
            assert!(check_workspace_in(&none, None, &ws(word)).is_ok(), "{word}");
        }
        assert_eq!(check(&WorkspaceDef { items: vec![], ..ws("a") }), "needs at least one item");
        assert_eq!(check(&WorkspaceDef { items: vec![item("apps", "", "")], ..ws("a") }), "an item needs a provider and an id");
        assert_eq!(check(&WorkspaceDef { items: vec![item("", "x", "")], ..ws("a") }), "an item needs a provider and an id");

        let c = Config { workspaces: vec![ws("work"), WorkspaceDef { aliases: vec!["w".into()], ..ws("other") }], ..Default::default() };
        assert_eq!(check_workspace_in(&c, None, &ws("Work")).unwrap_err(), "name is already used");
        assert_eq!(check_workspace_in(&c, None, &ws("W")).unwrap_err(), "name is already used");
        assert!(check_workspace_in(&c, Some("work"), &ws("Work")).is_ok());
        assert_eq!(check_workspace_in(&c, Some("other"), &ws("work")).unwrap_err(), "name is already used");
        assert!(check_workspace_in(&c, None, &ws("home")).is_ok());

        let text = "[[workspace]]\nname = \"x\"\nitems = []\n";
        assert_eq!(err(text), "workspace \"x\": needs at least one item");
        let two = "[[workspace]]\nname = \"work\"\nitems = [{ provider = \"a\", id = \"b\" }]\n\n[[workspace]]\nname = \"Work\"\nitems = [{ provider = \"a\", id = \"b\" }]\n";
        assert_eq!(err(two), "workspace \"Work\": name is already used");
    }

    #[test]
    fn direct_hotkey_rules() {
        let c = Config { direct_hotkeys: vec![hk("ctrl+alt+t")], ..Default::default() };
        assert_eq!(check_direct_hotkey_in(&c, None, &hk("")).unwrap_err(), "keys are empty");
        assert_eq!(check_direct_hotkey_in(&c, None, &hk("  ")).unwrap_err(), "keys are empty");
        assert_eq!(check_direct_hotkey_in(&c, None, &DirectHotkeyDef { item: item("apps", "", ""), ..hk("Ctrl+Alt+K") }).unwrap_err(), "an item needs a provider and an id");
        assert_eq!(check_direct_hotkey_in(&c, None, &hk("Alt+Ctrl+T")).unwrap_err(), DUPLICATE_COMBO);
        assert!(check_direct_hotkey_in(&c, Some("ctrl+alt+t"), &hk("Alt+Ctrl+T")).is_ok());
        assert!(check_direct_hotkey_in(&c, Some("Ctrl+Alt+T"), &hk("Ctrl+Alt+T")).is_ok());
        assert!(check_direct_hotkey_in(&c, None, &hk("Ctrl+Alt+K")).is_ok());

        let text = "[[direct_hotkey]]\nkeys = \"Ctrl+T\"\nitem = { provider = \"a\", id = \"b\" }\n\n[[direct_hotkey]]\nkeys = \"t+CTRL\"\nitem = { provider = \"a\", id = \"c\" }\n";
        assert_eq!(err(text), "direct_hotkey \"t+CTRL\": Already used by another hotkey");
        assert_eq!(
            err("[[direct_hotkey]]\nkeys = \"Ctrl+T\"\nitem = { provider = \"a\", id = \"\" }\n"),
            "direct_hotkey \"Ctrl+T\": an item needs a provider and an id"
        );
    }

    #[test]
    fn weak_combos_are_not_config_errors() {
        let c = parse("[[direct_hotkey]]\nkeys = \"Ctrl+T\"\nitem = { provider = \"a\", id = \"b\" }\n").unwrap();
        assert_eq!(c.direct_hotkeys[0].keys, "Ctrl+T");
        assert!(!combo_allowed("Ctrl+T"));
    }

    #[test]
    fn combo_rule() {
        for ok in ["Ctrl+Alt+T", "Ctrl+Shift+K", "Alt+Shift+1", "Super+N", "Win+N"] {
            assert!(combo_allowed(ok), "{ok}");
        }
        for bad in ["Ctrl+T", "Alt+F4", "F5", "Shift+F5", "Ctrl+Alt", "", "Ctrl+Alt+T+K"] {
            assert!(!combo_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn normalize_combo_ignores_case_order_and_win() {
        assert_eq!(normalize_combo("alt+space"), "alt+space");
        assert_eq!(normalize_combo("Space+Alt"), "alt+space");
        assert_eq!(normalize_combo(" Alt + Space "), "alt+space");
        assert_eq!(normalize_combo("Win+E"), normalize_combo("Super+E"));
        assert_eq!(normalize_combo("meta+e"), "super+e");
        assert_eq!(normalize_combo("Cmd+E"), "super+e");
        assert_eq!(normalize_combo("Control+Option+X"), normalize_combo("Ctrl+Alt+X"));
        assert_eq!(normalize_combo("Win+Shift+Alt+Ctrl+K"), "ctrl+alt+shift+super+k");
    }

    #[test]
    fn normalize_combo_folds_every_spelling_of_a_key() {
        let same = [
            ("Win+N", "Super+N"),
            ("Command+N", "Win+N"),
            ("CmdOrCtrl+Alt+Space", "Ctrl+Alt+Space"),
            ("CommandOrControl+Alt+X", "Ctrl+Alt+X"),
            ("CmdOrControl+Alt+X", "Ctrl+Alt+X"),
            ("CommandOrCtrl+Alt+X", "Ctrl+Alt+X"),
            ("Ctrl+Alt+KeyT", "Ctrl+Alt+T"),
            ("Ctrl+Alt+Digit1", "Ctrl+Alt+1"),
            ("Ctrl+Alt+Esc", "Ctrl+Alt+Escape"),
            ("Ctrl+Alt+Down", "Ctrl+Alt+ArrowDown"),
            ("Ctrl+Alt+Up", "Ctrl+Alt+ArrowUp"),
            ("Ctrl+Alt+Left", "Ctrl+Alt+ArrowLeft"),
            ("Ctrl+Alt+Right", "Ctrl+Alt+ArrowRight"),
            ("Ctrl+Alt+Num0", "Ctrl+Alt+Numpad0"),
            ("Ctrl+Alt+NumPlus", "Ctrl+Alt+NumpadAdd"),
            ("Ctrl+Alt+NumpadPlus", "Ctrl+Alt+NumAdd"),
            ("Ctrl+Alt+NumEnter", "Ctrl+Alt+NumpadEnter"),
            ("Ctrl+Alt+`", "Ctrl+Alt+Backquote"),
            ("Ctrl+Alt+-", "Ctrl+Alt+Minus"),
            ("Ctrl+Alt+\\", "Ctrl+Alt+Backslash"),
            ("Ctrl+Alt+[", "Ctrl+Alt+BracketLeft"),
            ("Ctrl+Alt+]", "Ctrl+Alt+BracketRight"),
            ("Ctrl+Alt+,", "Ctrl+Alt+Comma"),
            ("Ctrl+Alt+=", "Ctrl+Alt+Equal"),
            ("Ctrl+Alt+.", "Ctrl+Alt+Period"),
            ("Ctrl+Alt+'", "Ctrl+Alt+Quote"),
            ("Ctrl+Alt+;", "Ctrl+Alt+Semicolon"),
            ("Ctrl+Alt+/", "Ctrl+Alt+Slash"),
            ("Ctrl+Alt+PauseBreak", "Ctrl+Alt+Pause"),
            ("Ctrl+Alt+VolumeUp", "Ctrl+Alt+AudioVolumeUp"),
            ("Ctrl+Alt+MediaTrackPrev", "Ctrl+Alt+MediaTrackPrevious"),
        ];
        for (a, b) in same {
            assert_eq!(normalize_combo(a), normalize_combo(b), "{a} vs {b}");
        }
        // Different keys stay different.
        assert_ne!(normalize_combo("Ctrl+Alt+NumLock"), normalize_combo("Ctrl+Alt+Numpad0"));
        assert_ne!(normalize_combo("Ctrl+Alt+1"), normalize_combo("Ctrl+Alt+Num1"));
        assert!(combo_allowed("CmdOrCtrl+Alt+Space"));
    }

    #[test]
    fn same_key_in_another_spelling_is_a_duplicate() {
        let c = Config { direct_hotkeys: vec![hk("Ctrl+Alt+T")], ..Default::default() };
        assert_eq!(check_direct_hotkey_in(&c, None, &hk("Ctrl+Alt+KeyT")).unwrap_err(), DUPLICATE_COMBO);
    }

    #[test]
    fn workspace_aliases_follow_the_name_rules() {
        let none = Config::default();
        let e = check_workspace_in(&none, None, &WorkspaceDef { aliases: vec!["a:b".into()], ..ws("w") }).unwrap_err();
        assert_eq!(e, "name cannot contain :");
        let e = check_workspace_in(&none, None, &WorkspaceDef { aliases: vec!["two words".into()], ..ws("w") }).unwrap_err();
        assert_eq!(e, "name must be one word");
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
