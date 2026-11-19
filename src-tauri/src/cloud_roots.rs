//! The folders a cloud provider (OneDrive and the like) syncs, so a row can
//! offer "Keep on this device" / "Free up space". The list is read from the
//! registry in the background only; a query takes a read lock and compares
//! strings.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// How old the list may get before the bar showing reads it again.
const MAX_AGE: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
struct State {
    /// Lowercased, no trailing backslash.
    roots: Vec<String>,
    read_at: Option<Instant>,
    reading: bool,
}

#[derive(Default)]
pub struct CloudRoots {
    state: RwLock<State>,
}

fn normalized(path: &str) -> String {
    path.trim_end_matches('\\').to_lowercase()
}

impl CloudRoots {
    /// Whether `path` is a sync root or lies under one (at a folder boundary:
    /// `...\OneDriveBackup` is not under `...\OneDrive`). Memory only.
    pub fn contains(&self, path: &str) -> bool {
        let path = path.to_lowercase();
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state
            .roots
            .iter()
            .any(|root| path.strip_prefix(root.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with('\\')))
    }

    pub fn set(&self, roots: Vec<PathBuf>) {
        let roots = roots.iter().map(|r| normalized(&r.to_string_lossy())).filter(|r| !r.is_empty()).collect();
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.roots = roots;
        state.read_at = Some(Instant::now());
    }

    /// Reads the roots on a background thread when the list is older than
    /// five minutes (or never read). Returns at once.
    pub fn refresh_if_due(self: &Arc<Self>) {
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            if state.reading || state.read_at.is_some_and(|t| t.elapsed() < MAX_AGE) {
                return;
            }
            state.reading = true;
        }
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            if let Ok(sid) = crate::login_task::user_sid() {
                me.set(read_sync_roots(&sid));
            }
            me.state.write().unwrap_or_else(|e| e.into_inner()).reading = false;
        });
    }
}

/// The process-wide list the rows consult.
pub fn shared() -> &'static Arc<CloudRoots> {
    static ROOTS: OnceLock<Arc<CloudRoots>> = OnceLock::new();
    ROOTS.get_or_init(Default::default)
}

/// The user's sync roots: `...\SyncRootManager\*\UserSyncRoots`, the value
/// named by the user's SID. Reads the registry: background threads only.
pub fn read_sync_roots(sid: &str) -> Vec<PathBuf> {
    sys::sync_roots(sid)
}

mod sys {
    use std::path::PathBuf;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ,
    };

    const MANAGER: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SyncRootManager";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The string value `name` of the subkey `sub` of `key`.
    fn string_value(key: HKEY, sub: &str, name: &str) -> Option<String> {
        let (sub, name) = (wide(sub), wide(name));
        let mut bytes = 0u32;
        // SAFETY: the size query passes no buffer; the read passes the buffer it sized.
        unsafe {
            RegGetValueW(key, PCWSTR(sub.as_ptr()), PCWSTR(name.as_ptr()), RRF_RT_REG_SZ, None, None, Some(&mut bytes)).ok().ok()?;
            let mut buf = vec![0u16; (bytes as usize).div_ceil(2) + 1];
            let mut bytes = (buf.len() * 2) as u32;
            RegGetValueW(
                key,
                PCWSTR(sub.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
                Some(&mut bytes),
            )
            .ok()
            .ok()?;
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let s = String::from_utf16_lossy(&buf[..len]).trim().to_string();
            (!s.is_empty()).then_some(s)
        }
    }

    pub fn sync_roots(sid: &str) -> Vec<PathBuf> {
        let path = wide(MANAGER);
        let mut parent = HKEY::default();
        let mut out = Vec::new();
        // SAFETY: the key opened here is closed before leaving; buffers are sized.
        unsafe {
            if RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr()), None, KEY_READ, &mut parent).is_err() {
                return out;
            }
            let mut index = 0u32;
            loop {
                let mut name = [0u16; 256];
                let mut len = name.len() as u32;
                let status = RegEnumKeyExW(parent, index, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None);
                if status.is_err() {
                    break;
                }
                index += 1;
                let id = String::from_utf16_lossy(&name[..len as usize]);
                if let Some(root) = string_value(parent, &format!(r"{id}\UserSyncRoots"), sid) {
                    out.push(PathBuf::from(root));
                }
            }
            let _ = RegCloseKey(parent);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_roots_match_by_folder_boundary() {
        let roots = CloudRoots::default();
        roots.set(vec![PathBuf::from(r"C:\Users\u\OneDrive")]);
        assert!(roots.contains(r"C:\Users\u\OneDrive\a.txt"));
        assert!(roots.contains(r"C:\Users\u\onedrive\"));
        assert!(roots.contains(r"C:\Users\u\OneDrive"));
        assert!(!roots.contains(r"C:\Users\u\OneDriveBackup\x"));
        assert!(!roots.contains(r"C:\Users\u\x.txt"));
    }

    #[test]
    fn a_root_with_a_trailing_backslash_still_matches() {
        let roots = CloudRoots::default();
        roots.set(vec![PathBuf::from(r"D:\Cloud\")]);
        assert!(roots.contains(r"D:\Cloud\f\"));
        assert!(!roots.contains(r"D:\Clouds\f"));
    }

    #[test]
    fn no_roots_match_nothing() {
        assert!(!CloudRoots::default().contains(r"C:\a"));
    }
}
