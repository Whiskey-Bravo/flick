//! History: past searches and recent launches, persisted to `history.json`.
//! When disabled it records nothing and the file is deleted.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};

use crate::ranking::save_json;

const MAX_SEARCHES: usize = 50;
const MAX_RECENTS: usize = 5;

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
struct Recent {
    provider: String,
    id: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct HistoryFile {
    version: u32,
    searches: Vec<String>,
    recents: Vec<Recent>,
}

struct State {
    enabled: bool,
    data: HistoryFile,
}

pub struct History {
    state: Mutex<State>,
    save_lock: Mutex<()>,
    path: PathBuf,
}

impl History {
    /// Missing or corrupt file -> empty. Disabled: starts empty and deletes the file.
    pub fn load(path: PathBuf, enabled: bool) -> Self {
        let data = if enabled {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<HistoryFile>(&text).ok())
                .unwrap_or_default()
        } else {
            let _ = std::fs::remove_file(&path);
            HistoryFile::default()
        };
        Self { state: Mutex::new(State { enabled, data }), save_lock: Mutex::new(()), path }
    }

    pub fn record_search(&self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.change(|data| {
            data.searches.retain(|s| s != text);
            data.searches.insert(0, text.to_string());
            data.searches.truncate(MAX_SEARCHES);
        });
    }

    pub fn record_launch(&self, provider: &str, id: &str) {
        self.change(|data| {
            let r = Recent { provider: provider.into(), id: id.into() };
            data.recents.retain(|x| *x != r);
            data.recents.insert(0, r);
            data.recents.truncate(MAX_RECENTS);
        });
    }

    /// Newest first.
    pub fn searches(&self) -> Vec<String> {
        self.lock_state().data.searches.clone()
    }

    /// `(provider, id)` pairs, newest first.
    pub fn recents(&self) -> Vec<(String, String)> {
        let state = self.lock_state();
        state.data.recents.iter().map(|r| (r.provider.clone(), r.id.clone())).collect()
    }

    pub fn forget_launch(&self, provider: &str, id: &str) {
        self.change(|data| {
            data.recents.retain(|r| !(r.provider == provider && r.id == id));
        });
    }

    /// Drops the pairs from memory now and saves on a spawned thread, for the
    /// query path, which must not touch the disk. The thread saves the state as
    /// it is when it gets the save lock, so an older snapshot never overwrites
    /// a newer one.
    pub fn forget_launches(self: &Arc<Self>, pairs: &[(String, String)]) {
        let changed = {
            let mut state = self.lock_state();
            if !state.enabled {
                return;
            }
            let before = state.data.recents.len();
            state.data.recents.retain(|r| !pairs.iter().any(|(p, i)| r.provider == *p && r.id == *i));
            state.data.recents.len() != before
        };
        if changed {
            let this = Arc::clone(self);
            std::thread::spawn(move || this.save_current());
        }
    }

    /// Saves the current state (no-op while disabled), serialised with writers.
    fn save_current(&self) {
        let _saving = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot = {
            let mut state = self.lock_state();
            if !state.enabled {
                return;
            }
            state.data.version = 1;
            state.data.clone()
        };
        let _ = save_json(&self.path, &snapshot);
    }

    pub fn clear(&self) {
        self.change(|data| {
            data.searches.clear();
            data.recents.clear();
        });
    }

    pub fn is_enabled(&self) -> bool {
        self.lock_state().enabled
    }

    /// Off: empties the lists and deletes the file; records become no-ops.
    pub fn set_enabled(&self, on: bool) {
        // Serialised with writers so a late save cannot recreate the file.
        let _saving = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut state = self.lock_state();
            state.enabled = on;
            if !on {
                state.data = HistoryFile::default();
            }
        }
        if !on {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Applies `edit` and saves. No-op while disabled. The state lock is released
    /// before the disk write so readers never wait on it; writers are serialised.
    fn change(&self, edit: impl FnOnce(&mut HistoryFile)) {
        let _saving = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot = {
            let mut state = self.lock_state();
            if !state.enabled {
                return;
            }
            edit(&mut state.data);
            state.data.version = 1;
            state.data.clone()
        };
        let _ = save_json(&self.path, &snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(p: &str, i: &str) -> (String, String) {
        (p.to_string(), i.to_string())
    }

    fn history_in(dir: &tempfile::TempDir) -> History {
        History::load(dir.path().join("history.json"), true)
    }

    fn recent_ids(h: &History) -> Vec<String> {
        h.recents().into_iter().map(|(_, i)| i).collect()
    }

    #[test]
    fn history_keeps_50_newest_searches_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let h = history_in(&dir);
        h.record_search("a");
        h.record_search("b");
        h.record_search("a");
        assert_eq!(h.searches(), vec!["a", "b"]);
        for i in 0..60 {
            h.record_search(&format!("q{i}"));
        }
        let s = h.searches();
        assert_eq!(s.len(), 50);
        assert_eq!(s[0], "q59");
        assert_eq!(s[49], "q10");
    }

    #[test]
    fn history_keeps_5_recent_launches() {
        let dir = tempfile::tempdir().unwrap();
        let h = history_in(&dir);
        for i in 0..7 {
            h.record_launch("apps", &format!("id{i}"));
        }
        assert_eq!(recent_ids(&h), vec!["id6", "id5", "id4", "id3", "id2"]);
        h.record_launch("apps", "id2");
        assert_eq!(recent_ids(&h), vec!["id2", "id6", "id5", "id4", "id3"]);
    }

    #[test]
    fn forget_launches_drops_from_memory_and_saves_off_thread() {
        let dir = tempfile::tempdir().unwrap();
        let h = Arc::new(history_in(&dir));
        h.record_launch("apps", "a");
        h.record_launch("apps", "b");
        h.forget_launches(&[pair("apps", "a"), pair("nowhere", "z")]);
        assert_eq!(recent_ids(&h), vec!["b"]);
        let path = dir.path().join("history.json");
        let saved = |p: &std::path::Path| std::fs::read_to_string(p).map_or(false, |t| !t.contains("\"a\""));
        for _ in 0..200 {
            if saved(&path) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(saved(&path));
        assert!(History::load(path, true).recents().iter().all(|(_, i)| i == "b"));
    }

    #[test]
    fn history_ignores_blank_searches() {
        let dir = tempfile::tempdir().unwrap();
        let h = history_in(&dir);
        h.record_search("  ");
        assert!(h.searches().is_empty());
        assert!(!dir.path().join("history.json").exists());
    }

    #[test]
    fn history_trims_searches() {
        let dir = tempfile::tempdir().unwrap();
        let h = history_in(&dir);
        h.record_search(" a ");
        h.record_search("a");
        assert_eq!(h.searches(), vec!["a"]);
    }

    #[test]
    fn history_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let h = history_in(&dir);
        h.record_search("a");
        h.record_search("b");
        h.record_launch("apps", "x");
        h.record_launch("files", "y");
        drop(h);
        let h = history_in(&dir);
        assert_eq!(h.searches(), vec!["b", "a"]);
        assert_eq!(h.recents(), vec![pair("files", "y"), pair("apps", "x")]);
    }

    #[test]
    fn corrupt_or_empty_files_load_as_empty() {
        for content in ["{not json", ""] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.json");
            std::fs::write(&path, content).unwrap();
            let h = History::load(path.clone(), true);
            assert!(h.searches().is_empty());
            assert!(h.recents().is_empty());
            h.record_search("a");
            assert_eq!(History::load(path, true).searches(), vec!["a"]);
        }
    }

    #[test]
    fn disabled_history_records_nothing_and_deletes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        std::fs::write(&path, r#"{"version":1,"searches":["a"],"recents":[]}"#).unwrap();
        let h = History::load(path.clone(), false);
        assert!(!path.exists());
        h.record_search("b");
        h.record_launch("apps", "x");
        assert!(h.searches().is_empty());
        assert!(h.recents().is_empty());
        assert!(!path.exists());

        let h = history_in(&dir);
        h.record_search("a");
        assert!(path.exists());
        h.set_enabled(false);
        assert!(!path.exists());
        assert!(h.searches().is_empty());
        h.record_search("c");
        assert!(!path.exists());
        h.set_enabled(true);
        h.record_search("d");
        assert_eq!(h.searches(), vec!["d"]);
    }

    #[test]
    fn clear_empties_searches_and_recents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let h = History::load(path.clone(), true);
        h.record_search("a");
        h.record_launch("apps", "x");
        h.clear();
        assert!(h.searches().is_empty());
        assert!(h.recents().is_empty());
        let h = History::load(path, true);
        assert!(h.searches().is_empty());
        assert!(h.recents().is_empty());
    }

    #[test]
    fn forget_launch_removes_one_recent() {
        let dir = tempfile::tempdir().unwrap();
        let h = history_in(&dir);
        h.record_launch("apps", "x");
        h.record_launch("apps", "y");
        h.forget_launch("apps", "x");
        assert_eq!(h.recents(), vec![pair("apps", "y")]);
    }
}
