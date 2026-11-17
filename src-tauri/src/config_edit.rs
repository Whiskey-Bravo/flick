//! Edits `config.toml` text in place with `toml_edit`: comments, order and untouched
//! shortcuts survive, and nothing is returned that `config::parse` would reject.

use std::path::{Path, PathBuf};

use toml_edit::{value, Array, ArrayOfTables, Decor, DocumentMut, Item, Table, Value};

use crate::config::{self, ShortcutDef};

/// One setting of the Settings window. An empty string or list means "unset" (remove the line).
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "key", content = "value", rename_all = "snake_case")]
pub enum Setting {
    Hotkey(String),
    WslDistro(String),
    FileRoots(Vec<PathBuf>),
    History(bool),
    Online(bool),
    WeatherCity(String),
    Currency(String),
    TemperatureUnit(String),
    SearchEngine(String),
    CheckUpdates(bool),
}

/// Parses `text` (refusing a file that is already broken) and returns the editable document
/// together with the original text. `toml_edit` works on LF; `finish` puts the original line
/// ends back, so lines the edit did not touch keep their exact bytes.
fn open(text: &str) -> Result<(DocumentMut, String), String> {
    config::parse(text)?;
    let doc = text.replace("\r\n", "\n").parse::<DocumentMut>().map_err(|e| e.to_string())?;
    Ok((doc, text.to_string()))
}

/// Renders the edited document, restores line ends, and checks that Flick would accept it.
fn finish(doc: &DocumentMut, original: &str) -> Result<String, String> {
    let out = restore_line_ends(original, &doc.to_string());
    config::parse(&out)?;
    Ok(out)
}

/// `(content, line end)` per line; the end is "" for a last line without one.
fn split_lines(s: &str) -> Vec<(&str, &str)> {
    s.split_inclusive('\n')
        .map(|l| match l.strip_suffix("\r\n") {
            Some(c) => (c, "\r\n"),
            None => match l.strip_suffix('\n') {
                Some(c) => (c, "\n"),
                None => (l, ""),
            },
        })
        .collect()
}

/// Gives the LF text `edited` the line ends of `original`: a line that survived keeps its own
/// end, a changed or added line takes the end of the line it replaces (else of the line above),
/// and a file without a final newline stays that way.
fn restore_line_ends(original: &str, edited: &str) -> String {
    let old = split_lines(original);
    let new = split_lines(edited);
    let (n, m) = (old.len(), new.len());
    // lcs[i][j]: longest common run of lines in old[i..] and new[j..].
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i].0 == new[j].0 {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let no_final_newline = !original.is_empty() && !original.ends_with('\n');
    let mut prev = old.first().map_or("\n", |l| if l.1.is_empty() { "\n" } else { l.1 });
    let mut out = String::with_capacity(edited.len() + m);
    let (mut i, mut j) = (0, 0);
    let mut removed: Vec<&str> = Vec::new();
    let mut added = 0;
    while j < m {
        let mut end;
        if i < n && old[i].0 == new[j].0 && lcs[i][j] == lcs[i + 1][j + 1] + 1 {
            end = old[i].1;
            i += 1;
            removed.clear();
            added = 0;
        } else if i < n && lcs[i + 1][j] >= lcs[i][j + 1] {
            removed.push(old[i].1);
            i += 1;
            continue;
        } else {
            end = removed.get(added).copied().filter(|e| !e.is_empty()).unwrap_or(prev);
            added += 1;
        }
        if j == m - 1 {
            end = if no_final_newline { "" } else if end.is_empty() { prev } else { end };
        } else if end.is_empty() {
            end = prev;
        }
        out.push_str(new[j].0);
        out.push_str(end);
        if !end.is_empty() {
            prev = end;
        }
        j += 1;
    }
    out
}

fn text_value(s: &str) -> Option<Value> {
    (!s.is_empty()).then(|| Value::from(s))
}

fn prepend_prefix(decor: &mut Decor, add: &str) {
    let old = decor.prefix().and_then(|p| p.as_str()).unwrap_or("").to_string();
    decor.set_prefix(format!("{add}{old}"));
}

/// Removes `key` from `table`. A comment above it moves to whatever follows; when nothing
/// follows in this table, the comment is returned for the caller to place.
fn remove_in(table: &mut Table, key: &str) -> Option<String> {
    let position = table.iter().position(|(k, _)| k == key)?;
    let next = table.iter().nth(position + 1).map(|(k, _)| k.to_string());
    let prefix = table
        .key(key)
        .and_then(|k| k.leaf_decor().prefix())
        .and_then(|p| p.as_str())
        .filter(|p| p.contains('#'))
        .map(str::to_string);
    table.remove(key);
    let prefix = prefix?;
    let Some(next) = next else { return Some(prefix) };
    if matches!(table.get(&next), Some(Item::Value(_))) {
        if let Some(mut k) = table.key_mut(&next) {
            prepend_prefix(k.leaf_decor_mut(), &prefix);
        }
    } else {
        match table.get_mut(&next) {
            Some(Item::ArrayOfTables(tables)) => {
                if let Some(first) = tables.get_mut(0) {
                    prepend_prefix(first.decor_mut(), &prefix);
                }
            }
            Some(Item::Table(t)) => prepend_prefix(t.decor_mut(), &prefix),
            _ => {}
        }
    }
    None
}

/// Removes a root key's line, keeping the comment above it.
fn remove_key(doc: &mut DocumentMut, key: &str) {
    if let Some(prefix) = remove_in(doc.as_table_mut(), key) {
        let rest = doc.trailing().as_str().unwrap_or("").to_string();
        doc.set_trailing(format!("{prefix}{rest}"));
    }
}

/// Changes one setting. The value of an existing line is replaced in place (its comments stay);
/// an unset value removes the line; a new key goes after the last top-level key.
pub fn set(text: &str, setting: &Setting) -> Result<String, String> {
    let (mut doc, original) = open(text)?;
    let (key, new): (&str, Option<Value>) = match setting {
        Setting::Hotkey(s) => ("hotkey", text_value(s)),
        Setting::WslDistro(s) => ("wsl_distro", text_value(s)),
        Setting::FileRoots(roots) => (
            "file_roots",
            (!roots.is_empty())
                .then(|| Value::Array(roots.iter().map(|p| p.to_string_lossy().into_owned()).collect())),
        ),
        Setting::History(b) => ("history", Some(Value::from(*b))),
        Setting::Online(b) => ("online", Some(Value::from(*b))),
        Setting::WeatherCity(s) => ("weather_city", text_value(s)),
        Setting::Currency(s) => ("currency", text_value(s)),
        Setting::TemperatureUnit(s) => ("temperature_unit", text_value(s)),
        Setting::SearchEngine(s) => ("search_engine", text_value(s)),
        Setting::CheckUpdates(b) => ("check_updates", Some(Value::from(*b))),
    };
    match new {
        Some(mut value) => {
            let root = doc.as_table_mut();
            match root.get_mut(key) {
                Some(Item::Value(old)) => {
                    *value.decor_mut() = old.decor().clone();
                    *old = value;
                }
                _ => {
                    root.insert(key, Item::Value(value));
                }
            }
        }
        None => remove_key(&mut doc, key),
    }
    finish(&doc, &original)
}

fn build_shortcut(def: &ShortcutDef) -> Table {
    let mut t = Table::new();
    t.insert("name", value(def.name.as_str()));
    if !def.aliases.is_empty() {
        t.insert("aliases", value(def.aliases.iter().map(String::as_str).collect::<Array>()));
    }
    if !def.about.is_empty() {
        t.insert("about", value(def.about.as_str()));
    }
    for (key, action) in [("run", &def.run), ("wsl", &def.wsl), ("open", &def.open)] {
        if let Some(a) = action {
            t.insert(key, value(a.as_str()));
        }
    }
    if let Some(terminal) = def.terminal {
        t.insert("terminal", value(terminal));
    }
    if def.admin {
        t.insert("admin", value(true));
    }
    if let Some(pick) = &def.pick {
        t.insert("pick", value(pick.to_string_lossy().into_owned()));
    }
    if !def.choices.is_empty() {
        t.insert("choices", value(def.choices.iter().map(String::as_str).collect::<Array>()));
    }
    t
}

fn position_of(tables: &ArrayOfTables, name: &str) -> Result<usize, String> {
    tables
        .iter()
        .position(|t| t.get("name").and_then(Item::as_str) == Some(name))
        .ok_or_else(|| format!("no shortcut named \"{name}\""))
}

fn same_value(old: &Value, new: &Value) -> bool {
    match (old, new) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Boolean(a), Value::Boolean(b)) => a.value() == b.value(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| same_value(x, y))
        }
        _ => false,
    }
}

/// Brings an existing shortcut table in line with `def` without rewriting it: changed values
/// keep their old decor, unset keys go (their comment moves on), new keys are appended in the
/// canonical order, everything else stays byte for byte. Returns a comment that had nowhere to go.
fn edit_shortcut(table: &mut Table, def: &ShortcutDef) -> Option<String> {
    let wanted = build_shortcut(def);
    let mut leftover = String::new();
    for key in ["name", "aliases", "about", "run", "wsl", "open", "terminal", "admin", "pick", "choices"] {
        match wanted.get(key) {
            Some(Item::Value(new)) => match table.get_mut(key) {
                Some(Item::Value(old)) => {
                    if !same_value(old, new) {
                        let mut v = new.clone();
                        *v.decor_mut() = old.decor().clone();
                        *old = v;
                    }
                }
                _ => {
                    table.insert(key, Item::Value(new.clone()));
                }
            },
            _ => {
                if let Some(p) = remove_in(table, key) {
                    leftover.push_str(&p);
                }
            }
        }
    }
    (!leftover.is_empty()).then_some(leftover)
}

/// Adds a shortcut at the end (`original` is `None`) or replaces the one named `original`
/// where it stands, keeping the comments above it.
pub fn upsert_shortcut(text: &str, original: Option<&str>, def: &ShortcutDef) -> Result<String, String> {
    let (mut doc, text_before) = open(text)?;
    let root = doc.as_table_mut();
    if !root.contains_key("shortcut") {
        root.insert("shortcut", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    let Some(Item::ArrayOfTables(tables)) = root.get_mut("shortcut") else {
        return Err("shortcut must be a list of [[shortcut]] tables".into());
    };
    let mut leftover = None;
    match original {
        Some(name) => {
            let at = position_of(tables, name)?;
            if let Some(table) = tables.get_mut(at) {
                leftover = edit_shortcut(table, def).map(|p| (at, p));
            }
        }
        None => {
            let mut table = build_shortcut(def);
            if !text.trim().is_empty() {
                table.decor_mut().set_prefix("\n");
            }
            tables.push(table);
        }
    }
    if let Some((at, prefix)) = leftover {
        let next = match doc.as_table_mut().get_mut("shortcut") {
            Some(Item::ArrayOfTables(t)) => t.get_mut(at + 1),
            _ => None,
        };
        match next {
            Some(t) => prepend_prefix(t.decor_mut(), &prefix),
            None => {
                let rest = doc.trailing().as_str().unwrap_or("").to_string();
                doc.set_trailing(format!("{prefix}{rest}"));
            }
        }
    }
    finish(&doc, &text_before)
}

pub fn delete_shortcut(text: &str, name: &str) -> Result<String, String> {
    let (mut doc, original) = open(text)?;
    let root = doc.as_table_mut();
    let Some(Item::ArrayOfTables(tables)) = root.get_mut("shortcut") else {
        return Err(format!("no shortcut named \"{name}\""));
    };
    let at = position_of(tables, name)?;
    tables.remove(at);
    if tables.is_empty() {
        root.remove("shortcut");
    }
    finish(&doc, &original)
}

/// Writes to a temp file next to `path`, then renames it over, so a reader never sees half a file.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str, open: &str) -> ShortcutDef {
        ShortcutDef {
            name: name.into(), aliases: vec![], about: String::new(), run: None, wsl: None,
            open: Some(open.into()), terminal: None, admin: false, pick: None, choices: vec![],
        }
    }

    const TWO: &str = "# top\r\n\r\nhotkey = \"Alt+Space\" # keep\r\n\r\n# docs link\r\n[[shortcut]]\r\nname = \"s1\"\r\nopen = \"https://a.example\"\r\n\r\n[[shortcut]]\r\nname = \"s2\"\r\nopen = \"https://b.example\"\r\n";

    #[test]
    fn set_changes_an_existing_line_in_place() {
        let out = set("# my hotkey\nhotkey = \"Ctrl+Space\" # mine\n", &Setting::Hotkey("Alt+Q".into())).unwrap();
        assert_eq!(out, "# my hotkey\nhotkey = \"Alt+Q\" # mine\n");
    }

    #[test]
    fn set_adds_a_missing_key_before_the_first_shortcut() {
        let text = "hotkey = \"Alt+Space\"\n\n[[shortcut]]\nname = \"a\"\nopen = \"x\"\n";
        let out = set(text, &Setting::Online(false)).unwrap();
        assert!(out.find("online = false").unwrap() < out.find("[[shortcut]]").unwrap(), "{out}");
        let c = config::parse(&out).unwrap();
        assert!(!c.online);
        assert_eq!(c.shortcuts, config::parse(text).unwrap().shortcuts);
    }

    #[test]
    fn unset_removes_the_line_but_keeps_its_comment() {
        let out = set("## region\ncurrency = \"USD\"\nonline = true\n", &Setting::Currency(String::new())).unwrap();
        assert_eq!(out, "## region\nonline = true\n");
    }

    #[test]
    fn unset_of_the_last_key_keeps_its_comment_too() {
        let out = set("online = true\n## region\ncurrency = \"USD\"\n", &Setting::Currency(String::new())).unwrap();
        assert_eq!(out, "online = true\n## region\n");
        let text = "## region\ncurrency = \"USD\"\n\n[[shortcut]]\nname = \"a\"\nopen = \"x\"\n";
        let out = set(text, &Setting::Currency(String::new())).unwrap();
        assert!(out.contains("## region\n"), "{out}");
        assert!(!out.contains("currency"), "{out}");
    }

    #[test]
    fn file_roots_round_trip() {
        let roots = vec![PathBuf::from("C:/a"), PathBuf::from("D:/b")];
        let out = set("", &Setting::FileRoots(roots.clone())).unwrap();
        assert_eq!(config::parse(&out).unwrap().file_roots, roots);
        let out = set(&out, &Setting::FileRoots(vec![])).unwrap();
        assert!(config::parse(&out).unwrap().file_roots.is_empty());
    }

    #[test]
    fn crlf_and_untouched_parts_survive() {
        let out = set(TWO, &Setting::WeatherCity("Paris".into())).unwrap();
        assert!(out.contains("\r\n") && !out.replace("\r\n", "").contains('\n'), "{out:?}");
        let before: Vec<&str> = TWO.lines().collect();
        let after: Vec<&str> = out.lines().collect();
        assert_eq!(after.len(), before.len() + 1);
        for line in &before {
            assert!(after.contains(line), "lost {line:?}");
        }
        let added: Vec<&&str> = after.iter().filter(|l| !before.contains(l)).collect();
        assert_eq!(added, [&"weather_city = \"Paris\""]);
        assert_eq!(config::parse(&out).unwrap().weather_city, "Paris");
    }

    #[test]
    fn the_commented_help_block_is_left_alone() {
        let out = set(config::DEFAULT_FILE, &Setting::History(false)).unwrap();
        let comments = |t: &str| t.lines().filter(|l| l.starts_with('#')).map(String::from).collect::<Vec<_>>();
        assert_eq!(comments(&out), comments(config::DEFAULT_FILE));
        let active: Vec<&str> = out.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
        assert_eq!(active, ["history = false"]);
    }

    #[test]
    fn broken_file_is_refused() {
        let e = set("hotkey = ", &Setting::Online(false)).unwrap_err();
        assert!(e.contains("line 1"), "{e}");
    }

    #[test]
    fn invalid_result_is_refused() {
        let e = set("", &Setting::TemperatureUnit("k".into())).unwrap_err();
        assert!(e.contains("temperature_unit"), "{e}");
    }

    #[test]
    fn upsert_adds_at_the_end_and_edits_in_place() {
        let added = upsert_shortcut(TWO, None, &def("s3", "https://c.example")).unwrap();
        let c = config::parse(&added).unwrap();
        assert_eq!(c.shortcuts.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["s1", "s2", "s3"]);
        assert!(added.contains("# docs link"));

        let mut edited = def("s9", "https://new.example");
        edited.about = "About".into();
        let out = upsert_shortcut(TWO, Some("s1"), &edited).unwrap();
        let c = config::parse(&out).unwrap();
        assert_eq!(c.shortcuts.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["s9", "s2"]);
        assert_eq!(c.shortcuts[0], edited);
        assert!(out.find("# docs link").unwrap() < out.find("name = \"s9\"").unwrap());
        assert!(out.contains("hotkey = \"Alt+Space\" # keep"));
        assert!(!out.contains("admin") && !out.contains("aliases") && !out.contains("terminal"), "{out}");
    }

    #[test]
    fn upsert_writes_fields_in_a_fixed_order() {
        let d = ShortcutDef {
            name: "p".into(), aliases: vec!["q".into()], about: "A".into(), run: Some("code {path}".into()),
            wsl: None, open: None, terminal: Some(true), admin: false, pick: Some(PathBuf::from("C:/p")), choices: vec![],
        };
        let out = upsert_shortcut("", None, &d).unwrap();
        let pos = |k: &str| out.find(k).unwrap_or_else(|| panic!("{k} in {out}"));
        assert!(pos("name") < pos("aliases") && pos("aliases") < pos("about") && pos("about") < pos("run"));
        assert!(pos("run") < pos("terminal") && pos("terminal") < pos("pick"), "{out}");
        assert_eq!(config::parse(&out).unwrap().shortcuts, [d]);
    }

    #[test]
    fn upsert_rename_to_a_used_name_is_refused() {
        let e = upsert_shortcut(TWO, Some("s1"), &def("s2", "x")).unwrap_err();
        assert!(e.contains("already used"), "{e}");
        let e = upsert_shortcut(TWO, None, &def("s1", "x")).unwrap_err();
        assert!(e.contains("already used"), "{e}");
        assert!(upsert_shortcut(TWO, Some("nope"), &def("z", "x")).is_err());
    }

    #[test]
    fn delete_removes_only_that_table() {
        let out = delete_shortcut(TWO, "s1").unwrap();
        let c = config::parse(&out).unwrap();
        assert_eq!(c.shortcuts.len(), 1);
        assert_eq!(c.shortcuts[0].name, "s2");
        assert!(out.contains("hotkey = \"Alt+Space\" # keep"));
        let none = delete_shortcut(&out, "s2").unwrap();
        assert!(config::parse(&none).unwrap().shortcuts.is_empty());
        assert!(!none.contains("[[shortcut]]"));
        assert!(delete_shortcut(TWO, "nope").is_err());
    }

    #[test]
    fn mixed_line_endings_survive() {
        let text = "# a\r\nhotkey = \"Alt+Space\"\nonline = true\r\n\n[[shortcut]]\nname = \"s\"\r\nopen = \"x\"\n";
        let out = set(text, &Setting::Online(false)).unwrap();
        assert_eq!(out, text.replace("online = true", "online = false"));
        let out = set(text, &Setting::WeatherCity("Paris".into())).unwrap();
        assert!(out.starts_with("# a\r\nhotkey = \"Alt+Space\"\nonline = true\r\n"), "{out:?}");
        assert!(out.ends_with("\n\n[[shortcut]]\nname = \"s\"\r\nopen = \"x\"\n") || out.contains("[[shortcut]]\nname = \"s\"\r\nopen = \"x\"\n"), "{out:?}");
    }

    #[test]
    fn missing_final_newline_is_kept() {
        let out = set("online = true\nhotkey = \"Alt+Space\"", &Setting::Online(false)).unwrap();
        assert_eq!(out, "online = false\nhotkey = \"Alt+Space\"");
        let crlf = set("online = true\r\nhotkey = \"Alt+Space\"", &Setting::Online(false)).unwrap();
        assert_eq!(crlf, "online = false\r\nhotkey = \"Alt+Space\"");
    }

    #[test]
    fn editing_a_shortcut_keeps_its_inner_comments_and_order() {
        let text = "[[shortcut]]\nopen = \"https://a.example\" # where\nname = \"s1\"\n# why\nabout = \"Old\"\n\n[[shortcut]]\nname = \"s2\"\nopen = \"x\"\n";
        let mut d = def("s1", "https://a.example");
        d.about = "New".into();
        let out = upsert_shortcut(text, Some("s1"), &d).unwrap();
        assert_eq!(out, text.replace("\"Old\"", "\"New\""));

        d.about = String::new();
        d.aliases = vec!["q".into()];
        let out = upsert_shortcut(text, Some("s1"), &d).unwrap();
        let c = config::parse(&out).unwrap();
        assert_eq!(c.shortcuts[0], d);
        assert!(out.contains("# why\n"), "{out}");
        assert!(out.contains("open = \"https://a.example\" # where\nname = \"s1\"\n"), "{out}");
    }

    #[test]
    fn write_atomic_replaces_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "old").unwrap();
        write_atomic(&p, "new = 1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new = 1\n");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn setting_deserializes_from_the_window_shape() {
        let s: Setting = serde_json::from_str(r#"{"key":"check_updates","value":false}"#).unwrap();
        assert!(matches!(s, Setting::CheckUpdates(false)));
        let s: Setting = serde_json::from_str(r#"{"key":"file_roots","value":["C:/a"]}"#).unwrap();
        assert!(matches!(s, Setting::FileRoots(v) if v == [PathBuf::from("C:/a")]));
    }
}
