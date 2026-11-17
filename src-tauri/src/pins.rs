//! Pins: items the user pinned, in pin order, persisted to `pins.json`.

use std::{path::PathBuf, sync::Mutex};

use serde::{Deserialize, Serialize};

use crate::ranking::save_json;

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
struct Pin {
    provider: String,
    id: String,
}

#[derive(Serialize, Deserialize)]
struct PinsFile {
    version: u32,
    pins: Vec<Pin>,
}

pub struct Pins {
    pins: Mutex<Vec<Pin>>,
    save_lock: Mutex<()>,
    path: PathBuf,
}

impl Pins {
    /// Missing or corrupt file -> empty.
    pub fn load(path: PathBuf) -> Self {
        let pins = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<PinsFile>(&text).ok())
            .map(|f| f.pins)
            .unwrap_or_default();
        Self { pins: Mutex::new(pins), save_lock: Mutex::new(()), path }
    }

    /// Pins or unpins; returns the new pinned state. New pins go last.
    /// A failed save is ignored: the in-memory state is kept. The lock is
    /// released before the disk write; writers are serialised.
    pub fn toggle(&self, provider: &str, id: &str) -> bool {
        let _saving = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let (now_pinned, snapshot) = {
            let mut pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
            let pin = Pin { provider: provider.into(), id: id.into() };
            let now_pinned = match pins.iter().position(|p| *p == pin) {
                Some(i) => {
                    pins.remove(i);
                    false
                }
                None => {
                    pins.push(pin);
                    true
                }
            };
            (now_pinned, pins.clone())
        };
        let _ = save_json(&self.path, &PinsFile { version: 1, pins: snapshot });
        now_pinned
    }

    /// Moves a pin one place up or down; a no-op at the ends or for an unknown
    /// pin. Saved like `toggle`.
    pub fn move_pin(&self, provider: &str, id: &str, up: bool) {
        let _saving = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot = {
            let mut pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
            let Some(i) = pins.iter().position(|p| p.provider == provider && p.id == id) else {
                return;
            };
            let j = if up { i.checked_sub(1) } else { Some(i + 1).filter(|&j| j < pins.len()) };
            let Some(j) = j else { return };
            pins.swap(i, j);
            pins.clone()
        };
        let _ = save_json(&self.path, &PinsFile { version: 1, pins: snapshot });
    }

    pub fn is_pinned(&self, provider: &str, id: &str) -> bool {
        let pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
        pins.iter().any(|p| p.provider == provider && p.id == id)
    }

    /// `(provider, id)` pairs in pin order.
    pub fn list(&self) -> Vec<(String, String)> {
        let pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
        pins.iter().map(|p| (p.provider.clone(), p.id.clone())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(p: &str, i: &str) -> (String, String) {
        (p.to_string(), i.to_string())
    }

    #[test]
    fn pins_toggle_keeps_pin_order() {
        let dir = tempfile::tempdir().unwrap();
        let p = Pins::load(dir.path().join("pins.json"));
        assert!(p.toggle("apps", "a"));
        assert!(p.toggle("apps", "b"));
        assert!(p.toggle("apps", "c"));
        assert!(!p.toggle("apps", "b"));
        assert_eq!(p.list(), vec![pair("apps", "a"), pair("apps", "c")]);
        assert!(p.is_pinned("apps", "a"));
        assert!(!p.is_pinned("apps", "b"));
    }

    #[test]
    fn pins_move_up_and_down() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pins.json");
        let p = Pins::load(path.clone());
        for id in ["a", "b", "c"] {
            p.toggle("apps", id);
        }
        p.move_pin("apps", "b", true);
        assert_eq!(p.list(), vec![pair("apps", "b"), pair("apps", "a"), pair("apps", "c")]);
        p.move_pin("apps", "b", true); // first: no-op
        p.move_pin("apps", "c", false); // last: no-op
        p.move_pin("apps", "zzz", true); // unknown: no-op
        assert_eq!(p.list(), vec![pair("apps", "b"), pair("apps", "a"), pair("apps", "c")]);
        p.move_pin("apps", "b", false);
        assert_eq!(p.list(), vec![pair("apps", "a"), pair("apps", "b"), pair("apps", "c")]);
        assert_eq!(Pins::load(path).list(), vec![pair("apps", "a"), pair("apps", "b"), pair("apps", "c")]);
    }

    #[test]
    fn pins_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pins.json");
        let p = Pins::load(path.clone());
        p.toggle("apps", "a");
        p.toggle("files", "b");
        drop(p);
        assert_eq!(Pins::load(path).list(), vec![pair("apps", "a"), pair("files", "b")]);
    }

    #[test]
    fn corrupt_or_empty_pins_file_loads_empty_and_rewrites() {
        for content in ["{not json", ""] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("pins.json");
            std::fs::write(&path, content).unwrap();
            let p = Pins::load(path.clone());
            assert!(p.list().is_empty());
            p.toggle("apps", "a");
            assert_eq!(Pins::load(path).list(), vec![pair("apps", "a")]);
        }
    }
}
