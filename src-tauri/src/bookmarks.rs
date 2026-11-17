//! Firefox bookmarks, read from a copy of its database.
//!
//! Firefox keeps `places.sqlite` locked while it runs and holds the newest
//! changes in `places.sqlite-wal`. Both files are copied to the temp folder,
//! the copy is opened read-write so SQLite replays the log into it, and the
//! copy is deleted afterwards. Firefox's own files are only ever read.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    pub title: String,
    pub url: String,
}

/// The profile folder `profiles.ini` points at: the first `[Install…]`
/// section's `Default=` wins, else the `[Profile…]` marked `Default=1`.
/// Relative paths are joined to `firefox_dir`.
pub fn default_profile(ini: &str, firefox_dir: &Path) -> Option<PathBuf> {
    struct Section {
        name: String,
        keys: Vec<(String, String)>,
    }
    let mut sections: Vec<Section> = Vec::new();
    for line in ini.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.push(Section { name: name.to_string(), keys: Vec::new() });
        } else if let Some((key, value)) = line.split_once('=') {
            if let Some(section) = sections.last_mut() {
                section.keys.push((key.trim().to_string(), value.trim().to_string()));
            }
        }
    }
    let get = |s: &Section, key: &str| s.keys.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let join = |path: &str, relative: bool| {
        let path = path.replace('/', "\\");
        if relative {
            firefox_dir.join(path)
        } else {
            PathBuf::from(path)
        }
    };

    for s in sections.iter().filter(|s| s.name.starts_with("Install")) {
        if let Some(path) = get(s, "Default").filter(|p| !p.is_empty()) {
            // An install path is relative unless it is a drive or UNC path.
            let absolute = path.contains(':') || path.starts_with("\\\\") || path.starts_with("//");
            return Some(join(&path, !absolute));
        }
    }
    for s in sections.iter().filter(|s| s.name.starts_with("Profile")) {
        if get(s, "Default").as_deref() == Some("1") {
            if let Some(path) = get(s, "Path").filter(|p| !p.is_empty()) {
                return Some(join(&path, get(s, "IsRelative").as_deref() == Some("1")));
            }
        }
    }
    None
}

/// The host of a URL: between `://` and the next `/`, `?` or `#`, without
/// `user@`, `:port` and a leading `www.`.
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = if host.starts_with('[') {
        host.find(']').map_or(host, |end| &host[..=end])
    } else {
        host.split(':').next().unwrap_or("")
    };
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

pub const PLACES_SQL: &str = "SELECT b.title, p.url FROM moz_bookmarks b JOIN moz_places p ON p.id = b.fk WHERE b.type = 1 ORDER BY b.lastModified DESC";

/// Deletes the copy, its `-wal` and its `-shm` when dropped, on every path.
struct CopyGuard(PathBuf);

impl CopyGuard {
    fn with_suffix(&self, suffix: &str) -> PathBuf {
        let mut name = self.0.clone().into_os_string();
        name.push(suffix);
        PathBuf::from(name)
    }
}

impl Drop for CopyGuard {
    fn drop(&mut self) {
        for path in [self.0.clone(), self.with_suffix("-wal"), self.with_suffix("-shm")] {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The bookmarks of the profile, newest first, one per URL, http(s) only.
pub fn read_places(profile: &Path, temp_dir: &Path) -> Result<Vec<Bookmark>, String> {
    const FAILED: &str = "could not read Firefox bookmarks";
    let source = profile.join("places.sqlite");
    let copy = CopyGuard(temp_dir.join(format!("flick-places-{}.sqlite", std::process::id())));
    std::fs::copy(&source, &copy.0).map_err(|_| FAILED.to_string())?;
    let wal = profile.join("places.sqlite-wal");
    if wal.exists() {
        std::fs::copy(&wal, copy.with_suffix("-wal")).map_err(|_| FAILED.to_string())?;
    }

    let fail = |_| FAILED.to_string();
    let conn = rusqlite::Connection::open_with_flags(
        &copy.0,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(fail)?;
    // Replays the copied log into the copy, then nothing can change it.
    conn.execute_batch("PRAGMA query_only = ON").map_err(fail)?;
    let mut statement = conn.prepare(PLACES_SQL).map_err(fail)?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?)))
        .map_err(fail)?;

    let mut seen = std::collections::HashSet::new();
    let mut found = Vec::new();
    for row in rows {
        let (title, url) = row.map_err(fail)?;
        let Some(url) = url else { continue };
        if !(url.starts_with("http://") || url.starts_with("https://")) || !seen.insert(url.clone()) {
            continue;
        }
        let title = title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| url.clone());
        found.push(Bookmark { title, url });
    }
    drop(statement);
    drop(conn);
    Ok(found)
}

pub trait BookmarkSource: Send + Sync {
    fn read(&self) -> Result<Vec<Bookmark>, String>;
}

/// The default Firefox profile of this user.
pub struct Firefox;

impl BookmarkSource for Firefox {
    fn read(&self) -> Result<Vec<Bookmark>, String> {
        let Some(appdata) = std::env::var_os("APPDATA") else { return Ok(vec![]) };
        let firefox_dir = PathBuf::from(appdata).join("Mozilla").join("Firefox");
        let Ok(ini) = std::fs::read_to_string(firefox_dir.join("profiles.ini")) else { return Ok(vec![]) };
        match default_profile(&ini, &firefox_dir) {
            Some(profile) => read_places(&profile, &std::env::temp_dir()),
            None => Ok(vec![]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn profile_from_install_section_wins() {
        let ini = "[Profile0]\nName=old\nIsRelative=1\nPath=Profiles/old.default\nDefault=1\n\n\
                   [Install4F96D1932A9F858E]\nDefault=Profiles/abc.default-release\nLocked=1\n\n\
                   [Install2222]\nDefault=Profiles/other\n";
        assert_eq!(
            default_profile(ini, Path::new("C:\\fx")),
            Some(PathBuf::from("C:\\fx").join("Profiles\\abc.default-release"))
        );
    }

    #[test]
    fn profile_marked_default_is_used_without_install_section() {
        let ini = "[Profile0]\nName=a\nIsRelative=1\nPath=Profiles/a.one\n\n\
                   [Profile1]\nName=b\nIsRelative=0\nPath=D:/ff/b\nDefault=1\n";
        assert_eq!(default_profile(ini, Path::new("C:\\fx")), Some(PathBuf::from("D:\\ff\\b")));
        let relative = "[Profile0]\nIsRelative=1\nPath=Profiles/a.one\nDefault=1\n";
        assert_eq!(
            default_profile(relative, Path::new("C:\\fx")),
            Some(PathBuf::from("C:\\fx").join("Profiles\\a.one"))
        );
        assert_eq!(default_profile("[General]\nStartWithLastProfile=1\n", Path::new("C:\\fx")), None);
    }

    #[test]
    fn host_drops_scheme_www_port_and_path() {
        assert_eq!(host_of("https://user@www.example.com:8443/a?b"), "example.com");
        assert_eq!(host_of("http://doc.rust-lang.org#x"), "doc.rust-lang.org");
        assert_eq!(host_of("https://example.com?q=1"), "example.com");
    }

    const SCHEMA: &str = "CREATE TABLE moz_places(id INTEGER PRIMARY KEY, url TEXT, title TEXT);
        CREATE TABLE moz_bookmarks(id INTEGER PRIMARY KEY, type INTEGER, fk INTEGER, title TEXT, lastModified INTEGER);";

    fn add(conn: &Connection, id: i64, kind: i64, url: Option<&str>, title: &str, modified: i64) {
        if let Some(url) = url {
            conn.execute("INSERT INTO moz_places(id, url, title) VALUES (?1, ?2, ?3)", (id, url, title)).unwrap();
        }
        conn.execute(
            "INSERT INTO moz_bookmarks(id, type, fk, title, lastModified) VALUES (?1, ?2, ?3, ?4, ?5)",
            (id, kind, url.map(|_| id), title, modified),
        )
        .unwrap();
    }

    fn temp_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("flick-places-"))
            .collect()
    }

    #[test]
    fn bookmarks_are_read_from_a_copy() {
        let profile = tempfile::tempdir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let db = profile.path().join("places.sqlite");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            add(&conn, 1, 1, Some("https://old.example.com/"), "Old", 10);
            add(&conn, 2, 1, Some("https://new.example.com/"), "", 20);
            add(&conn, 3, 2, None, "A folder", 30);
            add(&conn, 4, 3, None, "", 40);
            add(&conn, 5, 1, Some("place:type=6"), "Recent", 50);
            add(&conn, 6, 1, Some("javascript:alert(1)"), "Script", 60);
        }
        let before = std::fs::read(&db).unwrap();
        let found = read_places(profile.path(), temp.path()).unwrap();
        assert_eq!(
            found,
            vec![
                Bookmark { title: "https://new.example.com/".into(), url: "https://new.example.com/".into() },
                Bookmark { title: "Old".into(), url: "https://old.example.com/".into() },
            ]
        );
        assert_eq!(temp_files(temp.path()), Vec::<String>::new());
        assert_eq!(std::fs::read(&db).unwrap(), before);
    }

    #[test]
    fn bookmarks_in_the_wal_are_read_from_the_copy() {
        let profile = tempfile::tempdir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let db = profile.path().join("places.sqlite");
        let conn = Connection::open(&db).unwrap();
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(())).unwrap();
        conn.execute_batch("PRAGMA wal_autocheckpoint = 0").unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        add(&conn, 1, 1, Some("https://old.example.com/"), "Old", 10);
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        add(&conn, 2, 1, Some("https://fresh.example.com/"), "Fresh", 20);
        assert!(profile.path().join("places.sqlite-wal").metadata().unwrap().len() > 0);

        let found = read_places(profile.path(), temp.path()).unwrap();
        assert_eq!(found.iter().map(|b| b.title.as_str()).collect::<Vec<_>>(), ["Fresh", "Old"]);
        assert_eq!(temp_files(temp.path()), Vec::<String>::new());
        drop(conn);
    }

    #[test]
    fn missing_firefox_is_no_bookmarks() {
        let profile = tempfile::tempdir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        assert!(read_places(profile.path(), temp.path()).is_err());
        assert_eq!(temp_files(temp.path()), Vec::<String>::new());
        // A file that is not a database fails after the copy and still cleans up.
        std::fs::write(profile.path().join("places.sqlite"), b"not a database at all").unwrap();
        assert!(read_places(profile.path(), temp.path()).is_err());
        assert_eq!(temp_files(temp.path()), Vec::<String>::new());
    }

    #[test]
    #[ignore = "reads the real Firefox profile through a copy; run by hand"]
    fn real_bookmarks_count() {
        match Firefox.read() {
            Ok(list) => println!("bookmarks: {}", list.len()),
            Err(e) => println!("error: {e}"),
        }
    }
}
