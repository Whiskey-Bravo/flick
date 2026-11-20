//! Keeps Flick's environment variables in step with Windows' while it runs.
//!
//! Programs started from Flick get Flick's environment (`launch.rs`). Without
//! this, a PATH or other variable changed in Windows' settings, or by an
//! installer, would reach them only after Flick restarts. A thread waits on
//! the two registry keys Windows keeps them in (the user's and the system's);
//! on a change it asks Windows for a fresh environment, built the way Explorer
//! builds one for new programs, and brings Flick's own up to date.

use std::collections::BTreeMap;

/// Environment variables by name, compared without case as Windows does:
/// upper-cased name -> (name as written, value).
pub type Env = BTreeMap<String, (String, String)>;

/// An [`Env`] from (name, value) pairs.
pub fn env_of<I: IntoIterator<Item = (String, String)>>(pairs: I) -> Env {
    pairs.into_iter().map(|(name, value)| (name.to_uppercase(), (name, value))).collect()
}

/// What to change in Flick's environment `current` now that Windows' fresh
/// environment is `fresh` (it was `before` at the last look): every variable
/// of `fresh` whose value differs is set, and a variable that was in `before`
/// but is gone from `fresh` (the user deleted it) is removed. Variables that
/// never came from Windows' settings (Flick was started with them) stay.
pub fn changes(current: &Env, before: &Env, fresh: &Env) -> (Vec<(String, String)>, Vec<String>) {
    let set = fresh
        .iter()
        .filter(|(key, (_, value))| current.get(*key).map(|(_, v)| v) != Some(value))
        .map(|(_, (name, value))| (name.clone(), value.clone()))
        .collect();
    let remove = before
        .iter()
        .filter(|(key, _)| !fresh.contains_key(*key))
        .filter_map(|(key, _)| current.get(key).map(|(name, _)| name.clone()))
        .collect();
    (set, remove)
}

/// Starts watching Windows' environment variables, once at start. If the
/// keys cannot be opened, Flick simply keeps the environment it started with.
pub fn start() {
    let _ = std::thread::Builder::new().name("flick-env-watch".into()).spawn(sys::watch);
}

mod sys {
    use super::{changes, env_of, Env};
    use std::time::Duration;
    use windows::core::w;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_NOTIFY,
        REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME,
    };
    use windows::Win32::Security::{TOKEN_DUPLICATE, TOKEN_QUERY};
    use windows::Win32::System::Threading::{CreateEventW, GetCurrentProcess, OpenProcessToken, WaitForMultipleObjects, INFINITE};

    /// Installers often write several variables one after another: one look
    /// after they are done instead of one per write.
    const SETTLE: Duration = Duration::from_millis(500);

    /// Flick's own environment now.
    fn current() -> Env {
        env_of(std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())))
    }

    /// The environment Windows gives a new program of this user now (system
    /// and user variables, expanded, PATH joined), as Explorer builds it.
    fn fresh() -> Option<Env> {
        let mut token = HANDLE::default();
        let mut block: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: the token is our own process's, closed below; the block is
        // read only up to its double NUL and freed with DestroyEnvironmentBlock.
        unsafe {
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_DUPLICATE, &mut token).ok()?;
            let made = CreateEnvironmentBlock(&mut block, Some(token), false);
            let _ = CloseHandle(token);
            made.ok()?;
            let mut pairs = Vec::new();
            let mut at = block as *const u16;
            loop {
                let len = (0..).take_while(|&i| *at.add(i) != 0).count();
                if len == 0 {
                    break;
                }
                let entry = String::from_utf16_lossy(std::slice::from_raw_parts(at, len));
                // Entries like `=C:=C:\` are per-drive folders, not variables.
                if let Some((name, value)) = entry.split_once('=').filter(|(name, _)| !name.is_empty()) {
                    pairs.push((name.to_string(), value.to_string()));
                }
                at = at.add(len + 1);
            }
            let _ = DestroyEnvironmentBlock(block);
            Some(env_of(pairs))
        }
    }

    /// Asks Windows to signal `event` at the next change under `key`.
    fn arm(key: HKEY, event: HANDLE) -> bool {
        // SAFETY: an open key and a live event; asynchronous, so it returns at once.
        unsafe { RegNotifyChangeKeyValue(key, false, REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET, Some(event), true).is_ok() }
    }

    pub fn watch() {
        let mut keys = [HKEY::default(); 2];
        // SAFETY: plain opens for notification only; the keys live as long as
        // this thread (Flick's lifetime) and are closed if setup fails.
        let opened = unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, w!("Environment"), None, KEY_NOTIFY, &mut keys[0]).is_ok()
                && RegOpenKeyExW(
                    HKEY_LOCAL_MACHINE,
                    w!(r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"),
                    None,
                    KEY_NOTIFY,
                    &mut keys[1],
                )
                .is_ok()
        };
        // SAFETY: auto-reset events, unnamed, owned by this thread.
        let events = unsafe { [CreateEventW(None, false, false, None), CreateEventW(None, false, false, None)] };
        let (Some(mut before), true, [Ok(e0), Ok(e1)]) = (fresh(), opened, events) else {
            // SAFETY: closing what was opened; a default key is skipped.
            unsafe { keys.iter().filter(|k| !k.is_invalid()).for_each(|k| { let _ = RegCloseKey(*k); }) };
            return;
        };
        let events = [e0, e1];
        if !(arm(keys[0], events[0]) && arm(keys[1], events[1])) {
            return;
        }
        loop {
            // SAFETY: two live events.
            let woke = unsafe { WaitForMultipleObjects(&events, false, INFINITE) };
            let Some(which) = woke.0.checked_sub(WAIT_OBJECT_0.0).map(|i| i as usize).filter(|&i| i < 2) else {
                return;
            };
            // Re-armed first, so a change while reading is not missed. The
            // other key, if it changed meanwhile, wakes the next wait.
            if !arm(keys[which], events[which]) {
                return;
            }
            std::thread::sleep(SETTLE);
            let Some(now) = fresh() else { continue };
            let (set, remove) = changes(&current(), &before, &now);
            for (name, value) in set {
                std::env::set_var(name, value);
            }
            for name in remove {
                std::env::remove_var(name);
            }
            before = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        env_of(pairs.iter().map(|(n, v)| (n.to_string(), v.to_string())))
    }

    #[test]
    fn a_new_or_changed_variable_is_set_and_an_unchanged_one_left_alone() {
        let current = env(&[("Path", r"C:\Windows"), ("TEMP", r"C:\T")]);
        let before = current.clone();
        let fresh = env(&[("Path", r"C:\Windows;C:\Tools"), ("TEMP", r"C:\T"), ("JAVA_HOME", r"C:\Java")]);
        let (set, remove) = changes(&current, &before, &fresh);
        assert_eq!(set, [("JAVA_HOME".to_string(), r"C:\Java".to_string()), ("Path".to_string(), r"C:\Windows;C:\Tools".to_string())]);
        assert!(remove.is_empty());
    }

    #[test]
    fn a_variable_deleted_in_windows_is_removed_under_the_name_flick_has() {
        let current = env(&[("Path", "p"), ("OLD_TOOL", "x")]);
        let before = env(&[("Path", "p"), ("Old_Tool", "x")]);
        let fresh = env(&[("Path", "p")]);
        assert_eq!(changes(&current, &before, &fresh), (vec![], vec!["OLD_TOOL".to_string()]));
    }

    #[test]
    fn a_variable_flick_was_started_with_stays() {
        let current = env(&[("Path", "p"), ("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--x")]);
        let before = env(&[("Path", "p")]);
        let fresh = env(&[("Path", "p")]);
        assert_eq!(changes(&current, &before, &fresh), (vec![], vec![]));
    }

    #[test]
    fn names_differing_only_in_case_are_one_variable() {
        let current = env(&[("PATH", "p")]);
        let fresh = env(&[("Path", "p")]);
        assert_eq!(changes(&current, &fresh, &fresh), (vec![], vec![]));
    }

    #[test]
    fn a_stale_value_flick_started_with_is_brought_up_to_date() {
        // Started by an installer that had an older PATH: corrected at the
        // first change, even though PATH itself did not change in Windows.
        let current = env(&[("Path", "old")]);
        let before = env(&[("Path", "new")]);
        let fresh = env(&[("Path", "new"), ("X", "1")]);
        let (set, _) = changes(&current, &before, &fresh);
        assert!(set.contains(&("Path".to_string(), "new".to_string())));
    }
}
