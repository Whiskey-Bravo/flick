//! Ranking: silent pick counts persisted to `picks.json`. Counts feed `boost` only.

use std::{collections::HashMap, path::PathBuf, sync::Mutex};

const MAX_COUNTED_PICKS: u32 = 20;
const BOOST_PER_PICK: u32 = 15;

pub struct Ranking {
    picks: Mutex<HashMap<String, u32>>,
    save_lock: Mutex<()>,
    path: PathBuf,
}

impl Ranking {
    /// Missing or corrupt file -> empty.
    pub fn load(path: PathBuf) -> Self {
        let picks = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<HashMap<String, u32>>(&text).ok())
            .unwrap_or_default();
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
        let _ = self.save(&snapshot);
    }

    /// `min(picks, 20) * 15`.
    pub fn boost(&self, provider: &str, id: &str) -> u32 {
        let picks = self.picks.lock().unwrap_or_else(|e| e.into_inner());
        picks.get(&key(provider, id)).copied().unwrap_or(0).min(MAX_COUNTED_PICKS) * BOOST_PER_PICK
    }

    /// Write a temp file next to the target, then rename over it.
    fn save(&self, picks: &HashMap<String, u32>) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string(picks).map_err(std::io::Error::other)?;
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }
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
