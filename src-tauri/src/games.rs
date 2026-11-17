//! Installed games found in the launchers' own files: Steam (its VDF files,
//! read by a small parser here), Epic (manifest JSON), Rockstar (the uninstall
//! list) and Minecraft (its Store package). Everything that touches disk or
//! the registry sits behind [`GameFiles`] and [`GameSource`]; scanning runs on
//! a background thread, never on the query path.

use crate::uninstall::{split_command, Program, ProgramSource, RegistryPrograms};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Store {
    Steam,
    Epic,
    Rockstar,
    Minecraft,
}

impl Store {
    pub fn label(self) -> &'static str {
        match self {
            Store::Steam => "Steam",
            Store::Epic => "Epic",
            Store::Rockstar => "Rockstar",
            Store::Minecraft => "Minecraft Launcher",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Start {
    /// Handed to the shell (`steam://…`, `shell:AppsFolder\…`).
    Uri(String),
    Exe { exe: String, params: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Game {
    pub id: String,
    pub name: String,
    pub store: Store,
    pub start: Start,
    pub folder: Option<PathBuf>,
    pub icon: String,
}

// ---------------------------------------------------------------- VDF

#[derive(Clone, Debug, PartialEq)]
pub enum Vdf {
    Str(String),
    Map(Vec<(String, Vdf)>),
}

impl Vdf {
    /// The value under `key` (case-insensitive) of a map.
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        self.entries().iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s),
            Vdf::Map(_) => None,
        }
    }

    pub fn entries(&self) -> &[(String, Vdf)] {
        match self {
            Vdf::Map(entries) => entries,
            Vdf::Str(_) => &[],
        }
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Text(String),
    Open,
    Close,
}

fn tokenize(text: &str) -> Option<Vec<Token>> {
    let mut out = Vec::new();
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '{' => {
                chars.next();
                out.push(Token::Open);
            }
            '}' => {
                chars.next();
                out.push(Token::Close);
            }
            '/' => {
                chars.next();
                if chars.peek() != Some(&'/') {
                    return None;
                }
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            '\\' => s.push('\\'),
                            '"' => s.push('"'),
                            other => {
                                s.push('\\');
                                s.push(other);
                            }
                        },
                        c => s.push(c),
                    }
                }
                out.push(Token::Text(s));
            }
            _ => {
                // An unquoted word.
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || matches!(c, '{' | '}' | '"') {
                        break;
                    }
                    s.push(c);
                    chars.next();
                }
                out.push(Token::Text(s));
            }
        }
    }
    Some(out)
}

/// Pairs up to the closing brace (or the end at the top level).
fn parse_map(tokens: &[Token], pos: &mut usize, nested: bool) -> Option<Vec<(String, Vdf)>> {
    let mut entries = Vec::new();
    loop {
        match tokens.get(*pos) {
            None => return (!nested).then_some(entries),
            Some(Token::Close) => {
                if !nested {
                    return None;
                }
                *pos += 1;
                return Some(entries);
            }
            Some(Token::Open) => return None,
            Some(Token::Text(key)) => {
                *pos += 1;
                let value = match tokens.get(*pos)? {
                    Token::Text(s) => {
                        *pos += 1;
                        Vdf::Str(s.clone())
                    }
                    Token::Open => {
                        *pos += 1;
                        Vdf::Map(parse_map(tokens, pos, true)?)
                    }
                    Token::Close => return None,
                };
                entries.push((key.clone(), value));
            }
        }
    }
}

/// Valve's text format as a map of the top-level pairs; `None` when it is empty or broken.
pub fn parse_vdf(text: &str) -> Option<Vdf> {
    let tokens = tokenize(text)?;
    if tokens.is_empty() {
        return None;
    }
    let mut pos = 0;
    let entries = parse_map(&tokens, &mut pos, false)?;
    Some(Vdf::Map(entries))
}

// ---------------------------------------------------------------- files

pub trait GameFiles: Send + Sync {
    fn read(&self, path: &Path) -> Option<String>;
    /// Files directly in `dir` whose name starts with `prefix` and ends in `.ext`.
    fn list(&self, dir: &Path, prefix: &str, ext: &str) -> Vec<PathBuf>;
}

pub struct SystemGameFiles;

impl GameFiles for SystemGameFiles {
    fn read(&self, path: &Path) -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn list(&self, dir: &Path, prefix: &str, ext: &str) -> Vec<PathBuf> {
        let prefix = prefix.to_lowercase();
        let ext = format!(".{}", ext.trim_start_matches('.').to_lowercase());
        let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
        read.filter_map(Result::ok)
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_lowercase();
                name.starts_with(&prefix) && name.ends_with(&ext)
            })
            .map(|e| e.path())
            .collect()
    }
}

// ---------------------------------------------------------------- Steam

const STEAMWORKS_REDIST: &str = "228980";
/// `StateFlags` bit: fully installed. A game that needs an update (bit 2 as well) keeps it.
const STATE_FULLY_INSTALLED: u32 = 4;

fn lib_key(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

fn is_tool(appid: &str, name: &str) -> bool {
    let lower = name.to_lowercase();
    appid == STEAMWORKS_REDIST
        || lower == "proton"
        || lower.starts_with("proton ")
        || lower.starts_with("steam linux runtime")
        || lower.starts_with("steamworks")
        || lower.ends_with(" sdk")
}

fn steam_libraries(steam: &Path, files: &dyn GameFiles) -> Vec<PathBuf> {
    let mut libs = vec![steam.to_path_buf()];
    let vdf_path = steam.join("steamapps").join("libraryfolders.vdf");
    if let Some(root) = files.read(&vdf_path).and_then(|t| parse_vdf(&t)) {
        if let Some(folders) = root.get("libraryfolders") {
            for (key, value) in folders.entries() {
                let path = match value {
                    // Current format: "<n>" { "path" "…" … }.
                    Vdf::Map(_) => value.get("path").and_then(Vdf::str),
                    // Old format: "<n>" "<path>"; the other keys are statistics.
                    Vdf::Str(s) => (!key.is_empty() && key.chars().all(|c| c.is_ascii_digit())).then_some(s.as_str()),
                };
                if let Some(path) = path.filter(|p| !p.trim().is_empty()) {
                    libs.push(PathBuf::from(path.replace('/', "\\")));
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    libs.retain(|l| seen.insert(lib_key(l)));
    libs
}

pub fn steam_games(steam: &Path, files: &dyn GameFiles) -> Vec<Game> {
    let icon = format!("file:{}", steam.join("steam.exe").display());
    let mut out: Vec<Game> = Vec::new();
    for lib in steam_libraries(steam, files) {
        let apps = lib.join("steamapps");
        for manifest in files.list(&apps, "appmanifest_", "acf") {
            let Some(state) = files.read(&manifest).and_then(|t| parse_vdf(&t)) else { continue };
            let Some(state) = state.get("AppState") else { continue };
            let text = |k: &str| state.get(k).and_then(Vdf::str).map(str::trim).filter(|s| !s.is_empty());
            let (Some(appid), Some(name)) = (text("appid"), text("name")) else { continue };
            if !appid.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let flags = text("StateFlags").and_then(|f| f.parse::<u32>().ok()).unwrap_or(0);
            if flags & STATE_FULLY_INSTALLED == 0 || is_tool(appid, name) {
                continue;
            }
            let id = format!("steam:{appid}");
            if out.iter().any(|g| g.id == id) {
                continue;
            }
            out.push(Game {
                id,
                name: name.to_string(),
                store: Store::Steam,
                start: Start::Uri(format!("steam://rungameid/{appid}")),
                folder: text("installdir").map(|d| apps.join("common").join(d)),
                icon: icon.clone(),
            });
        }
    }
    out
}

// ---------------------------------------------------------------- Epic

pub fn epic_games(manifests: &Path, files: &dyn GameFiles) -> Vec<Game> {
    let mut out: Vec<Game> = Vec::new();
    for item in files.list(manifests, "", "item") {
        let Some(json) = files.read(&item).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else {
            continue;
        };
        let text = |k: &str| json.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
        if json.get("bIsIncompleteInstall").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        let Some(app) = text("AppName") else { continue };
        if text("MainGameAppName").is_some_and(|main| main != app) {
            continue;
        }
        if let Some(categories) = json.get("AppCategories").and_then(|v| v.as_array()) {
            if !categories.iter().filter_map(|c| c.as_str()).any(|c| c.eq_ignore_ascii_case("games")) {
                continue;
            }
        }
        let Some(name) = text("DisplayName") else { continue };
        let id = format!("epic:{app}");
        if out.iter().any(|g| g.id == id) {
            continue;
        }
        let location = text("InstallLocation").map(PathBuf::from);
        let icon = match (&location, text("LaunchExecutable")) {
            (Some(dir), Some(exe)) => format!("file:{}", dir.join(exe).display()),
            _ => "folder".to_string(),
        };
        out.push(Game {
            id,
            name: name.to_string(),
            store: Store::Epic,
            start: Start::Uri(format!("com.epicgames.launcher://apps/{app}?action=launch&silent=true")),
            folder: location,
            icon,
        });
    }
    out
}

// ---------------------------------------------------------------- Rockstar

/// Titles registered in Windows' uninstall list with the Rockstar launcher's uninstaller:
/// `"…\Rockstar Games\Launcher\uninstall.exe" -uninstall=<titleId>`.
pub fn rockstar_games(programs: &[Program]) -> Vec<Game> {
    const UNINSTALLER: &str = r"\rockstar games\launcher\uninstall.exe";
    let mut out: Vec<Game> = Vec::new();
    for p in programs {
        let (Some(command), Some(location)) = (&p.uninstall, &p.install_location) else { continue };
        let Some((program, params)) = split_command(command) else { continue };
        if !program.replace('/', "\\").to_lowercase().ends_with(UNINSTALLER) {
            continue;
        }
        let title = params
            .split_whitespace()
            .find_map(|a| a.strip_prefix("-uninstall="))
            .map(|t| t.trim_matches('"'))
            .filter(|t| !t.is_empty());
        let Some(title) = title else { continue };
        if title.eq_ignore_ascii_case("launcher") || title.eq_ignore_ascii_case("socialclub") {
            continue;
        }
        let id = format!("rockstar:{title}");
        if out.iter().any(|g| g.id == id) {
            continue;
        }
        let Some(folder) = Path::new(&program).parent() else { continue };
        let exe = folder.join("Launcher.exe").to_string_lossy().into_owned();
        out.push(Game {
            id,
            name: p.name.clone(),
            store: Store::Rockstar,
            start: Start::Exe { exe: exe.clone(), params: format!("-launchTitleInFolder \"{}\"", location.display()) },
            folder: Some(location.clone()),
            icon: format!("file:{exe}"),
        });
    }
    out
}

// ---------------------------------------------------------------- Minecraft

pub const MINECRAFT_FAMILY: &str = "Microsoft.4297127D64EC6_8wekyb3d8bbwe";

pub fn minecraft() -> Game {
    let target = format!(r"shell:AppsFolder\{MINECRAFT_FAMILY}!Minecraft");
    Game {
        id: "minecraft".into(),
        name: "Minecraft".into(),
        store: Store::Minecraft,
        icon: format!("file:{target}"),
        start: Start::Uri(target),
        folder: None,
    }
}

// ---------------------------------------------------------------- the real scan

pub trait GameSource: Send + Sync {
    fn scan(&self) -> Vec<Game>;
}

/// Reads the registry and disk: background thread only, never in tests.
pub struct SystemGames;

impl GameSource for SystemGames {
    fn scan(&self) -> Vec<Game> {
        let files = SystemGameFiles;
        let mut games = Vec::new();
        if let Some(steam) = sys::steam_path() {
            games.extend(steam_games(&steam, &files));
        }
        if let Some(manifests) = sys::epic_manifests() {
            games.extend(epic_games(&manifests, &files));
        }
        games.extend(rockstar_games(&RegistryPrograms.programs()));
        if sys::minecraft_installed() {
            games.push(minecraft());
        }
        games.sort_by_cached_key(|g| (g.name.to_lowercase(), g.id.clone()));
        let mut seen = std::collections::HashSet::new();
        games.retain(|g| seen.insert(g.id.clone()));
        games
    }
}

mod sys {
    use super::MINECRAFT_FAMILY;
    use std::path::PathBuf;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows::Win32::Storage::Packaging::Appx::GetPackagesByPackageFamily;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Steam's install folder, `/` turned into `\`.
    pub fn steam_path() -> Option<PathBuf> {
        let key = wide(r"Software\Valve\Steam");
        let name = wide("SteamPath");
        let mut bytes = 0u32;
        // SAFETY: the size query passes no buffer; the read passes the buffer it sized.
        let text = unsafe {
            RegGetValueW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), PCWSTR(name.as_ptr()), RRF_RT_REG_SZ, None, None, Some(&mut bytes))
                .ok()
                .ok()?;
            let mut buf = vec![0u16; (bytes as usize).div_ceil(2) + 1];
            let mut bytes = (buf.len() * 2) as u32;
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
                Some(&mut bytes),
            )
            .ok()
            .ok()?;
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..len])
        };
        let text = text.trim().replace('/', "\\");
        (!text.is_empty()).then(|| PathBuf::from(text))
    }

    pub fn epic_manifests() -> Option<PathBuf> {
        let data = std::env::var_os("ProgramData")?;
        Some(PathBuf::from(data).join(r"Epic\EpicGamesLauncher\Data\Manifests"))
    }

    /// At least one package of the Minecraft Launcher's family is installed.
    pub fn minecraft_installed() -> bool {
        let family = wide(MINECRAFT_FAMILY);
        let mut count = 0u32;
        let mut length = 0u32;
        // SAFETY: a size-only query: no output buffers are passed.
        let status =
            unsafe { GetPackagesByPackageFamily(PCWSTR(family.as_ptr()), &mut count, None, &mut length, None) };
        (status == ERROR_INSUFFICIENT_BUFFER || status.0 == 0) && count > 0
    }
}

#[cfg(test)]
pub struct FakeGameFiles {
    files: std::collections::BTreeMap<String, String>,
}

#[cfg(test)]
impl FakeGameFiles {
    pub fn new() -> Self {
        Self { files: Default::default() }
    }

    pub fn with(mut self, path: &str, text: &str) -> Self {
        self.files.insert(fake_key(path), text.to_string());
        self
    }
}

#[cfg(test)]
fn fake_key(path: &str) -> String {
    path.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

#[cfg(test)]
impl GameFiles for FakeGameFiles {
    fn read(&self, path: &Path) -> Option<String> {
        self.files.get(&fake_key(&path.to_string_lossy())).cloned()
    }

    fn list(&self, dir: &Path, prefix: &str, ext: &str) -> Vec<PathBuf> {
        let dir = fake_key(&dir.to_string_lossy());
        let prefix = prefix.to_lowercase();
        let ext = format!(".{}", ext.trim_start_matches('.').to_lowercase());
        self.files
            .keys()
            .filter_map(|k| {
                let (parent, name) = k.rsplit_once('\\')?;
                (parent == dir && name.starts_with(&prefix) && name.ends_with(&ext)).then(|| PathBuf::from(k))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEAM: &str = r"C:\Program Files (x86)\Steam";

    fn manifest(appid: &str, name: &str, flags: u32, dir: &str) -> String {
        format!(
            "\"AppState\"\n{{\n\t\"appid\"\t\t\"{appid}\"\n\t\"name\"\t\t\"{name}\"\n\t\"StateFlags\"\t\t\"{flags}\"\n\t\"installdir\"\t\t\"{dir}\"\n}}\n"
        )
    }

    fn acf(lib: &str, appid: &str) -> String {
        format!(r"{lib}\steamapps\appmanifest_{appid}.acf")
    }

    fn library_file() -> String {
        format!(r"{STEAM}\steamapps\libraryfolders.vdf")
    }

    #[test]
    fn vdf_parses_nested_maps_and_escapes() {
        let text = "// leading comment\n\"Root\"\n{\n\t\"Path\"\t\"D:\\\\Steam Library\"\n\t\"quote\" \"say \\\"hi\\\"\\n\\tbye\"\n\t\"inner\" { \"a\" \"1\" // trailing\n \"b\" { \"c\" \"2\" } }\n}\n";
        let root = parse_vdf(text).unwrap();
        let r = root.get("root").unwrap();
        assert_eq!(r.get("PATH").and_then(Vdf::str), Some(r"D:\Steam Library"));
        assert_eq!(r.get("quote").and_then(Vdf::str), Some("say \"hi\"\n\tbye"));
        let inner = r.get("inner").unwrap();
        assert_eq!(inner.get("a").and_then(Vdf::str), Some("1"));
        assert_eq!(inner.get("b").unwrap().get("c").and_then(Vdf::str), Some("2"));
        assert_eq!(inner.entries().len(), 2);
        assert!(r.get("missing").is_none());
        assert!(parse_vdf("\"a\" {").is_none());
        assert!(parse_vdf("").is_none());
    }

    #[test]
    fn libraries_on_other_drives_are_read() {
        let files = FakeGameFiles::new()
            .with(
                &library_file(),
                "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t\t\"label\"\t\t\"\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t}\n}\n",
            )
            .with(&acf(STEAM, "620"), &manifest("620", "Portal 2", 4, "Portal 2"))
            .with(&acf(r"D:\SteamLibrary", "1145360"), &manifest("1145360", "Hades", 4, "Hades"));
        let games = steam_games(Path::new(STEAM), &files);
        let names: Vec<&str> = games.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(games.len(), 2, "{names:?}");
        let hades = games.iter().find(|g| g.name == "Hades").unwrap();
        assert_eq!(hades.id, "steam:1145360");
        assert_eq!(hades.store, Store::Steam);
        assert_eq!(hades.start, Start::Uri("steam://rungameid/1145360".into()));
        assert_eq!(hades.folder, Some(PathBuf::from(r"D:\SteamLibrary\steamapps\common\Hades")));
        assert_eq!(hades.icon, format!(r"file:{STEAM}\steam.exe"));
        let portal = games.iter().find(|g| g.name == "Portal 2").unwrap();
        assert_eq!(portal.folder, Some(PathBuf::from(format!(r"{STEAM}\steamapps\common\Portal 2"))));
    }

    #[test]
    fn old_libraryfolders_format_is_read() {
        let files = FakeGameFiles::new()
            .with(
                &library_file(),
                "\"LibraryFolders\"\n{\n\t\"TimeNextStatsReport\"\t\"1700000000\"\n\t\"ContentStatsID\"\t\"123\"\n\t\"1\"\t\t\"E:\\\\Games\"\n}\n",
            )
            .with(&acf(r"E:\Games", "70"), &manifest("70", "Half-Life", 4, "Half-Life"));
        let games = steam_games(Path::new(STEAM), &files);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].folder, Some(PathBuf::from(r"E:\Games\steamapps\common\Half-Life")));
    }

    #[test]
    fn steam_itself_is_a_library_and_duplicates_collapse() {
        let files = FakeGameFiles::new()
            .with(
                &library_file(),
                "\"libraryfolders\" { \"0\" { \"path\" \"c:\\\\program files (x86)\\\\steam\" } \"1\" { \"path\" \"c:\\\\program files (x86)\\\\steam\\\\\" } }",
            )
            .with(&acf(STEAM, "620"), &manifest("620", "Portal 2", 4, "Portal 2"));
        assert_eq!(steam_games(Path::new(STEAM), &files).len(), 1);
        // No libraryfolders.vdf at all: Steam's own folder still counts.
        let files = FakeGameFiles::new().with(&acf(STEAM, "620"), &manifest("620", "Portal 2", 4, "Portal 2"));
        assert_eq!(steam_games(Path::new(STEAM), &files).len(), 1);
    }

    #[test]
    fn only_fully_installed_games_are_listed() {
        let files = FakeGameFiles::new()
            .with(&acf(STEAM, "1"), &manifest("1", "Installed", 4, "a"))
            .with(&acf(STEAM, "2"), &manifest("2", "Updating", 6, "b"))
            .with(&acf(STEAM, "3"), &manifest("3", "Downloading", 1026, "c"))
            .with(&acf(STEAM, "4"), &manifest("4", "Just Queued", 1, "d"));
        let mut names: Vec<String> = steam_games(Path::new(STEAM), &files).into_iter().map(|g| g.name).collect();
        names.sort();
        assert_eq!(names, ["Installed", "Updating"]);
    }

    #[test]
    fn tools_and_redistributables_are_skipped() {
        let files = FakeGameFiles::new()
            .with(&acf(STEAM, "228980"), &manifest("228980", "Steamworks Common Redistributables", 4, "x"))
            .with(&acf(STEAM, "2348590"), &manifest("2348590", "Proton 9.0", 4, "p"))
            .with(&acf(STEAM, "1628350"), &manifest("1628350", "Steam Linux Runtime 3.0 (sniper)", 4, "s"))
            .with(&acf(STEAM, "5"), &manifest("5", "Some SDK", 4, "k"))
            .with(&acf(STEAM, "6"), &manifest("6", "Steamworks Shared", 4, "w"))
            .with(&acf(STEAM, "620"), &manifest("620", "Portal 2", 4, "Portal 2"));
        let games = steam_games(Path::new(STEAM), &files);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].name, "Portal 2");
    }

    fn item(app: &str, main: &str, extra: &str) -> String {
        format!(
            "{{\"FormatVersion\":0,\"DisplayName\":\"{app} Name\",\"AppName\":\"{app}\",\"MainGameAppName\":\"{main}\",\"InstallLocation\":\"D:\\\\Epic\\\\{app}\",\"LaunchExecutable\":\"{app}.exe\"{extra}}}"
        )
    }

    #[test]
    fn epic_lists_installed_games_only() {
        let dir = r"C:\ProgramData\Epic\EpicGamesLauncher\Data\Manifests";
        let files = FakeGameFiles::new()
            .with(&format!(r"{dir}\a.item"), &item("Fortnite", "Fortnite", ",\"AppCategories\":[\"public\",\"games\",\"applications\"]"))
            .with(&format!(r"{dir}\b.item"), &item("Half", "Half", ",\"bIsIncompleteInstall\":true"))
            .with(&format!(r"{dir}\c.item"), &item("Addon", "Fortnite", ""))
            .with(&format!(r"{dir}\d.item"), &item("Plugin", "Plugin", ",\"AppCategories\":[\"plugins\",\"plugins/engine\"]"))
            .with(&format!(r"{dir}\e.item"), &item("Rocket", "Rocket", ",\"bIsIncompleteInstall\":false"))
            .with(&format!(r"{dir}\f.item"), "not json")
            .with(&format!(r"{dir}\g.txt"), &item("Other", "Other", ""));
        let games = epic_games(Path::new(dir), &files);
        let mut ids: Vec<&str> = games.iter().map(|g| g.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["epic:Fortnite", "epic:Rocket"]);
        let f = games.iter().find(|g| g.id == "epic:Fortnite").unwrap();
        assert_eq!(f.name, "Fortnite Name");
        assert_eq!(f.store, Store::Epic);
        assert_eq!(f.start, Start::Uri("com.epicgames.launcher://apps/Fortnite?action=launch&silent=true".into()));
        assert_eq!(f.folder, Some(PathBuf::from(r"D:\Epic\Fortnite")));
        assert_eq!(f.icon, r"file:D:\Epic\Fortnite\Fortnite.exe");
    }

    fn program(name: &str, location: Option<&str>, uninstall: &str) -> Program {
        Program {
            name: name.into(),
            install_location: location.map(PathBuf::from),
            uninstall: Some(uninstall.into()),
            system: false,
        }
    }

    #[test]
    fn rockstar_titles_start_through_the_launcher() {
        let un = r#""C:\Program Files\Rockstar Games\Launcher\uninstall.exe" -uninstall=gta5"#;
        let programs = vec![
            program("Grand Theft Auto V", Some(r"D:\Games\GTAV"), r#""C:\Program Files\Rockstar Games\Launcher\uninstall.exe" -enableFullMode -uninstall=gta5"#),
            program(
                "Rockstar Games Launcher",
                Some(r"C:\Program Files\Rockstar Games\Launcher"),
                r#""C:\Program Files\Rockstar Games\Launcher\uninstall.exe" -uninstall=launcher"#,
            ),
            program(
                "Rockstar Games Social Club",
                Some(r"C:\Program Files\Rockstar Games\Social Club"),
                r#""C:\Program Files\Rockstar Games\Launcher\uninstall.exe" -uninstall=socialclub"#,
            ),
            program("Grand Theft Auto V", Some(r"D:\Games\GTAV"), un),
            program("No Location", None, r#""C:\Program Files\Rockstar Games\Launcher\uninstall.exe" -uninstall=rdr2"#),
            program("Something Else", Some(r"D:\Else"), r#""C:\Program Files\Else\uninstall.exe" -uninstall=else"#),
        ];
        let games = rockstar_games(&programs);
        assert_eq!(games.len(), 1, "{games:?}");
        let g = &games[0];
        assert_eq!(g.id, "rockstar:gta5");
        assert_eq!(g.name, "Grand Theft Auto V");
        assert_eq!(g.store, Store::Rockstar);
        assert_eq!(
            g.start,
            Start::Exe {
                exe: r"C:\Program Files\Rockstar Games\Launcher\Launcher.exe".into(),
                params: r#"-launchTitleInFolder "D:\Games\GTAV""#.into(),
            }
        );
        assert_eq!(g.folder, Some(PathBuf::from(r"D:\Games\GTAV")));
        assert_eq!(g.icon, r"file:C:\Program Files\Rockstar Games\Launcher\Launcher.exe");
    }

    #[test]
    fn minecraft_starts_through_the_apps_folder() {
        let g = minecraft();
        assert_eq!(g.id, "minecraft");
        assert_eq!(g.name, "Minecraft");
        assert_eq!(g.store.label(), "Minecraft Launcher");
        assert_eq!(g.start, Start::Uri(r"shell:AppsFolder\Microsoft.4297127D64EC6_8wekyb3d8bbwe!Minecraft".into()));
        assert!(g.folder.is_none());
        assert_eq!(g.icon, r"file:shell:AppsFolder\Microsoft.4297127D64EC6_8wekyb3d8bbwe!Minecraft");
        assert_eq!(Store::Steam.label(), "Steam");
        assert_eq!(Store::Epic.label(), "Epic");
        assert_eq!(Store::Rockstar.label(), "Rockstar");
    }

    #[test]
    fn missing_launchers_mean_no_games() {
        let files = FakeGameFiles::new();
        assert!(steam_games(Path::new(STEAM), &files).is_empty());
        assert!(epic_games(Path::new(r"C:\ProgramData\Epic\Nope"), &files).is_empty());
        assert!(rockstar_games(&[]).is_empty());
    }

    /// By hand: `cargo test --lib real_games_list -- --ignored --nocapture`.
    /// Reads only; prints counts, never names or paths.
    #[test]
    #[ignore]
    fn real_games_list() {
        let files = SystemGameFiles;
        let steam = sys::steam_path().map(|p| steam_games(&p, &files).len());
        let epic = sys::epic_manifests().map(|p| epic_games(&p, &files).len());
        let programs = crate::uninstall::RegistryPrograms.programs();
        let rockstar = rockstar_games(&programs).len();
        let rockstar_entries =
            programs.iter().filter(|p| p.uninstall.as_deref().is_some_and(|u| u.to_lowercase().contains("rockstar games"))).count();
        let minecraft = sys::minecraft_installed();
        let total = SystemGames.scan().len();
        println!("steam: {steam:?}, epic: {epic:?}, rockstar: {rockstar} (of {rockstar_entries} entries), minecraft: {minecraft}, total: {total}");
    }
}
