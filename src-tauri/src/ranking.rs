//! Ranking: silent pick counts persisted to `picks.json`. Counts feed `boost` only.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};

use serde::{de::DeserializeOwned, Serialize};

use crate::file_ops::{moved_id, same_item};

const MAX_COUNTED_PICKS: u32 = 20;
const BOOST_PER_PICK: u32 = 15;

pub struct Ranking {
    picks: Mutex<HashMap<String, u32>>,
    save_lock: Mutex<()>,
    path: PathBuf,
}

impl Ranking {
    /// Missing or corrupt file -> empty (a corrupt one is kept as `.corrupt`).
    pub fn load(path: PathBuf) -> Self {
        let picks = load_json::<HashMap<String, u32>>(&path).unwrap_or_default();
        Self { picks: Mutex::new(picks), save_lock: Mutex::new(()), path }
    }

    /// Increments and saves. A failed save is ignored: the in-memory count is kept.
    /// The lock is released before the disk write so `boost` (on the query path)
    /// never waits on it.
    pub fn record(&self, provider: &str, id: &str) {
        // Serialises writers so an older snapshot never overwrites a newer one.
        let _saving = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot = {
            let mut picks = self.picks.lock().unwrap_or_else(|e| e.into_inner());
            let count = picks.entry(key(provider, id)).or_insert(0);
            *count = count.saturating_add(1);
            picks.clone()
        };
        let _ = save_json(&self.path, &snapshot);
    }

    /// The item `old` (and anything below it, for a folder) is now at `new`:
    /// the picks of every provider move with it, added up where `new` already
    /// had some. Saved like `record`.
    pub fn rename(&self, old: &str, new: &str) {
        let _saving = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot = {
            let mut picks = self.picks.lock().unwrap_or_else(|e| e.into_inner());
            let moves: Vec<(String, String)> = picks
                .keys()
                .filter_map(|k| {
                    let (provider, id) = k.split_once(':')?;
                    Some((k.clone(), key(provider, &moved_id(id, old, new)?)))
                })
                .collect();
            if moves.is_empty() {
                return;
            }
            for (from, to) in moves {
                let count = picks.remove(&from).unwrap_or(0);
                let slot = picks.entry(to).or_insert(0);
                *slot = slot.saturating_add(count);
            }
            picks.clone()
        };
        let _ = save_json(&self.path, &snapshot);
    }

    /// Drops the picks of the item `id`, of every provider. Saved like `record`.
    pub fn forget(&self, id: &str) {
        let _saving = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot = {
            let mut picks = self.picks.lock().unwrap_or_else(|e| e.into_inner());
            let before = picks.len();
            picks.retain(|k, _| !k.split_once(':').is_some_and(|(_, i)| same_item(i, id)));
            if picks.len() == before {
                return;
            }
            picks.clone()
        };
        let _ = save_json(&self.path, &snapshot);
    }

    /// `min(picks, 20) * 15`.
    pub fn boost(&self, provider: &str, id: &str) -> u32 {
        let picks = self.picks.lock().unwrap_or_else(|e| e.into_inner());
        picks.get(&key(provider, id)).copied().unwrap_or(0).min(MAX_COUNTED_PICKS) * BOOST_PER_PICK
    }

}

/// Serialise `value` and write it to a temp file next to `path`, then rename over it.
/// Each write has its own temp name, so two saves of the same file at once
/// cannot mix. Not flushed to disk: for files that are cheap to lose (picks,
/// history, caches), as these saves run on every Enter. A power cut right after
/// one can leave an empty file, which [`load_json`] moves aside.
pub(crate) fn save_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    write_json(path, value, false)
}

/// Like [`save_json`], but the temp file is flushed to disk before the rename,
/// so a power cut right after a save leaves the old file or the new one, never
/// an empty one. For what the user set by hand: reminders and pins.
pub(crate) fn save_json_durable(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    write_json(path, value, true)
}

/// [`save_json`] and [`save_json_durable`]; `durable` flushes before the rename.
fn write_json(path: &Path, value: &impl Serialize, durable: bool) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(value).map_err(std::io::Error::other)?;
    let tmp = temp_path(path);
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(json.as_bytes())?;
        if durable {
            f.sync_all()?;
        }
        Ok(())
    });
    written.and_then(|()| std::fs::rename(&tmp, path)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// A temp name next to `path` that no other write uses: `<path>.<pid>-<n>.tmp`.
fn temp_path(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut tmp = path.to_path_buf().into_os_string();
    tmp.push(format!(".{}-{n}.tmp", std::process::id()));
    PathBuf::from(tmp)
}

/// Reads the JSON file at `path`; None when it is missing, unreadable or does
/// not parse. A file that was read but does not parse is renamed to
/// `<name>.corrupt` first, so the next save cannot write over what is left of it.
pub(crate) fn load_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    let parsed = serde_json::from_slice::<T>(&bytes).ok();
    if parsed.is_none() {
        let mut aside = path.to_path_buf().into_os_string();
        aside.push(".corrupt");
        let _ = std::fs::rename(path, PathBuf::from(aside));
    }
    parsed
}

fn key(provider: &str, id: &str) -> String {
    format!("{provider}:{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranking_in(dir: &tempfile::TempDir) -> Ranking {
        Ranking::load(dir.path().join("picks.json"))
    }

    #[test]
    fn boost_is_zero_when_unseen() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(ranking_in(&dir).boost("apps", "chrome"), 0);
    }

    #[test]
    fn boost_grows_15_per_pick_and_caps_at_300() {
        let dir = tempfile::tempdir().unwrap();
        let r = ranking_in(&dir);
        r.record("apps", "chrome");
        assert_eq!(r.boost("apps", "chrome"), 15);
        for _ in 0..24 {
            r.record("apps", "chrome");
        }
        assert_eq!(r.boost("apps", "chrome"), 300);
        assert_eq!(r.boost("apps", "other"), 0);
        assert_eq!(r.boost("files", "chrome"), 0);
    }

    #[test]
    fn survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let r = ranking_in(&dir);
        r.record("apps", "chrome");
        r.record("apps", "chrome");
        drop(r);
        assert_eq!(ranking_in(&dir).boost("apps", "chrome"), 30);
    }

    #[test]
    fn corrupt_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("picks.json"), "not json").unwrap();
        let r = ranking_in(&dir);
        assert_eq!(r.boost("apps", "chrome"), 0);
        r.record("apps", "chrome");
        assert_eq!(r.boost("apps", "chrome"), 15);
    }

    #[test]
    fn a_corrupt_file_is_kept_aside_before_the_next_save() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("picks.json"), "{\"apps:chrome\": 4").unwrap();
        ranking_in(&dir).record("apps", "code");
        let aside = std::fs::read_to_string(dir.path().join("picks.json.corrupt")).unwrap();
        assert_eq!(aside, "{\"apps:chrome\": 4");
        assert_eq!(ranking_in(&dir).boost("apps", "code"), 15);
    }

    #[test]
    fn a_missing_file_leaves_nothing_aside() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_json::<HashMap<String, u32>>(&dir.path().join("picks.json")), None);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn each_save_has_its_own_temp_file_and_leaves_none_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rates.json");
        assert_ne!(temp_path(&path), temp_path(&path));
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        let _ = save_json(&path, &vec![i; 500]);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let saved: Vec<u32> = load_json(&path).expect("the file is whole");
        assert_eq!(saved.len(), 500);
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["rates.json"]);
    }

    #[test]
    fn record_creates_missing_parent_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Flick").join("picks.json");
        Ranking::load(path.clone()).record("apps", "chrome");
        assert!(path.exists());
    }

    #[test]
    fn save_failure_keeps_in_memory_count() {
        let dir = tempfile::tempdir().unwrap();
        // Parent "folder" is a file, so creating it and saving must fail.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let r = Ranking::load(blocker.join("picks.json"));
        r.record("apps", "chrome");
        assert_eq!(r.boost("apps", "chrome"), 15);
    }
}
