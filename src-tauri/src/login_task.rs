//! Start at login through a per-user Task Scheduler task "at sign-in".
//!
//! Windows holds programs in the Run list back for about a minute after
//! sign-in; a sign-in task starts within seconds. The old Run value
//! (`HKCU\...\Run`, `Flick`, written by tauri-plugin-autostart) is still
//! read, moved over to the task, and used as the fallback when the task
//! can't be registered. Never both at once.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows::Win32::System::TaskScheduler::{
    TASK_CREATE_OR_UPDATE, TASK_INSTANCES_IGNORE_NEW, TASK_INSTANCES_POLICY, TASK_LOGON_INTERACTIVE_TOKEN,
    TASK_LOGON_TYPE, TASK_RUNLEVEL_LUA, TASK_RUNLEVEL_TYPE,
};

// The task definition (see the design): normal priority (the task default 7
// is below normal), no time limit, runs on battery, a second start is
// ignored while Flick runs, visible in the user's task list, no catch-up run.
const TASK_PRIORITY: i32 = 5;
const EXECUTION_TIME_LIMIT: &str = "PT0S";
const DISALLOW_START_IF_ON_BATTERIES: bool = false;
const STOP_IF_GOING_ON_BATTERIES: bool = false;
const HIDDEN: bool = false;
const START_WHEN_AVAILABLE: bool = false;
const MULTIPLE_INSTANCES: TASK_INSTANCES_POLICY = TASK_INSTANCES_IGNORE_NEW;
const LOGON_TYPE: TASK_LOGON_TYPE = TASK_LOGON_INTERACTIVE_TOKEN;
const RUN_LEVEL: TASK_RUNLEVEL_TYPE = TASK_RUNLEVEL_LUA;
const CREATION_FLAGS: i32 = TASK_CREATE_OR_UPDATE.0;

/// The old Run value tauri-plugin-autostart wrote.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE_NAME: &str = "Flick";
/// Where Windows records "disabled in Task Manager's Startup apps" for the
/// Run list (REG_BINARY, same value name; the Run value itself stays).
const STARTUP_APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// Serialises the changes (`set`, `migrate`): the start-up thread, the
/// Settings command and the tray toggle each run on their own background
/// thread, and two interleaved register/delete sequences could leave both
/// the task and the Run value. Safe because it is only ever taken on
/// background threads (never the main thread), no other lock is held when
/// it is taken, and nothing done while holding it waits on another thread.
static CHANGE: Mutex<()> = Mutex::new(());

/// The task's name in the root folder: one per user of the PC.
fn task_name(sid: &str) -> String {
    format!("Flick (start at sign-in) [{sid}]")
}

/// The Run value's text: the exe path, quoted.
fn run_value_text(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// Whether this process was started to remove the task and the Run value.
pub fn wants_remove_login_task(args: &[String]) -> bool {
    args.iter().skip(1).any(|a| a == "--remove-login-task")
}

/// The two ways Flick can start at sign-in. The real one talks to the Task
/// Scheduler and the registry; tests use a fake.
pub trait LoginStart: Send + Sync {
    /// The exe the task starts; `None` when there is no task.
    fn task_path(&self) -> Result<Option<PathBuf>, String>;
    /// Creates the task, or updates it to start `exe`.
    fn register_task(&self, exe: &Path) -> Result<(), String>;
    /// Removes the task; Ok when there is none.
    fn delete_task(&self) -> Result<(), String>;
    /// Whether the old Run value is present and not switched off in Task
    /// Manager's Startup apps.
    fn run_value(&self) -> bool;
    /// Whether a file exists at `p`.
    fn exe_exists(&self, p: &Path) -> bool;
    /// Writes the Run value pointing at `exe` (quoted).
    fn set_run_value(&self, exe: &Path) -> Result<(), String>;
    /// Removes the Run value; Ok when there is none.
    fn delete_run_value(&self) -> Result<(), String>;
}

/// Start at login is on: the task or the old Run value is present. A
/// scheduler error reads as off: Settings then offers to switch it on, which
/// registers the task again.
pub fn is_enabled(s: &dyn LoginStart) -> bool {
    matches!(s.task_path(), Ok(Some(_))) || s.run_value()
}

/// Switches Start at login on (task, Run value removed; the Run value when
/// the task can't be registered) or off (both removed). Returns whether it
/// is on afterwards.
pub fn set(s: &dyn LoginStart, on: bool, exe: &Path) -> bool {
    let _change = CHANGE.lock().unwrap_or_else(|e| e.into_inner());
    set_locked(s, on, exe)
}

/// `set` with `CHANGE` already held.
fn set_locked(s: &dyn LoginStart, on: bool, exe: &Path) -> bool {
    if !on {
        remove_all(s);
    } else if s.register_task(exe).is_ok() {
        // Never both: a Run value that won't go would start Flick twice, so
        // then the Run value stays and the task goes.
        if s.delete_run_value().is_err() {
            let _ = s.delete_task();
        }
    } else if s.set_run_value(exe).is_ok() {
        // A task left from before may start an old exe, and with the Run
        // value Flick would start twice.
        let _ = s.delete_task();
    }
    is_enabled(s)
}

/// Moves an existing install over: a Run value, or a task whose exe is gone,
/// becomes a task for `exe`. With neither present nothing happens, so a user
/// who had Start at login off keeps it off; a Run value switched off in Task
/// Manager doesn't count as present (and is left alone: removing it would
/// make the entry vanish from Task Manager's list). A task for another exe
/// that still exists is left alone: another copy of Flick doesn't take it
/// over, only Settings or the tray do.
pub fn migrate(s: &dyn LoginStart, exe: &Path) {
    let _change = CHANGE.lock().unwrap_or_else(|e| e.into_inner());
    let stale_task = match s.task_path() {
        Ok(Some(path)) => !same_path(&path, exe) && (path.as_os_str().is_empty() || !s.exe_exists(&path)),
        _ => false,
    };
    if stale_task || s.run_value() {
        set_locked(s, true, exe);
    }
}

/// What a start does: the first run (or a restore after a manual upgrade,
/// see `restore_marker`) turns Start at login on, any other start moves an
/// old install over (`migrate`). Returns whether it is on afterwards.
pub fn at_start(s: &dyn LoginStart, turn_on: bool, exe: &Path) -> bool {
    if turn_on {
        return set(s, true, exe);
    }
    migrate(s, exe);
    is_enabled(s)
}

/// Whether the StartupApproved value (None: there is none) leaves the entry
/// on. Windows writes 02 00 00 00 plus eight zero bytes for enabled, and 03
/// plus a non-zero timestamp when it was disabled in Task Manager. Same rule
/// as auto-launch 0.6.0 (`src/windows.rs`, `last_eight_bytes_all_zeros`, used
/// by `is_task_manager_enabled`): on when the last 8 bytes are all zero; a
/// value shorter than 8 bytes counts as on there too.
fn startup_approved_enabled(bytes: Option<&[u8]>) -> bool {
    match bytes {
        Some(b) if b.len() >= 8 => b.iter().rev().take(8).all(|v| *v == 0),
        _ => true,
    }
}

/// Windows paths compare without regard to case.
fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// The uninstaller's removal: like `remove_all`, and says whether Start at
/// login was on, so the caller can leave `restore_marker`.
pub fn remove_for_uninstall(s: &dyn LoginStart) -> bool {
    let was_on = is_enabled(s);
    remove_all(s);
    was_on
}

/// A file next to the config left by the uninstaller when Start at login was
/// on. A manual upgrade runs the old version's uninstaller first (the
/// installer's default "Uninstall before installing"), which removes Start at
/// login; the new version's next start finds this file, turns it back on and
/// deletes the file. In-app updates skip the uninstall hook and need none.
pub fn restore_marker(config: &Path) -> PathBuf {
    config.with_file_name("start-at-login.restore")
}

/// Removes the task and the Run value; absent ones are fine.
pub fn remove_all(s: &dyn LoginStart) {
    let _ = s.delete_task();
    let _ = s.delete_run_value();
}

/// The real Task Scheduler and registry. Blocking COM and registry calls:
/// never on the main thread.
pub struct WindowsLoginStart;

impl LoginStart for WindowsLoginStart {
    fn task_path(&self) -> Result<Option<PathBuf>, String> {
        real::task_path()
    }
    fn register_task(&self, exe: &Path) -> Result<(), String> {
        real::register_task(exe)
    }
    fn delete_task(&self) -> Result<(), String> {
        real::delete_task()
    }
    fn run_value(&self) -> bool {
        real::run_value()
    }
    fn exe_exists(&self, p: &Path) -> bool {
        p.exists()
    }
    fn set_run_value(&self, exe: &Path) -> Result<(), String> {
        real::set_run_value(exe)
    }
    fn delete_run_value(&self) -> Result<(), String> {
        real::delete_run_value()
    }
}

pub(crate) use real::user_sid;

mod real {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    use windows::core::{Interface, BSTR, HRESULT, HSTRING, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, LocalFree, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, HANDLE, HLOCAL, VARIANT_BOOL,
        VARIANT_FALSE, VARIANT_TRUE,
    };
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
        COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_ANY, RRF_RT_REG_BINARY,
    };
    use windows::Win32::System::TaskScheduler::{
        IExecAction, ILogonTrigger, ITaskFolder, ITaskService, TaskScheduler, TASK_ACTION_EXEC, TASK_TRIGGER_LOGON,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::System::Variant::VARIANT;

    use super::*;

    /// COM on the calling thread for the life of the guard. A thread already
    /// in another apartment keeps it (and COM still works there).
    struct Com(bool);

    impl Com {
        fn init() -> Com {
            // SAFETY: no pointers; balanced by Drop when it succeeded.
            Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_ok())
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: pairs a successful CoInitializeEx on this thread.
                unsafe { CoUninitialize() };
            }
        }
    }

    fn err(what: &str, e: windows::core::Error) -> String {
        format!("{what}: {}", e.message())
    }

    fn is_not_found(e: &windows::core::Error) -> bool {
        e.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) || e.code() == HRESULT::from_win32(ERROR_PATH_NOT_FOUND.0)
    }

    fn vbool(b: bool) -> VARIANT_BOOL {
        if b {
            VARIANT_TRUE
        } else {
            VARIANT_FALSE
        }
    }

    /// The signed-in user's SID as text (`S-1-5-21-…`), from the process token.
    pub fn user_sid() -> Result<String, String> {
        // SAFETY: the token handle is closed below; the buffer is sized by the
        // first call and u64-aligned for TOKEN_USER; the SID string is freed
        // with LocalFree as ConvertSidToStringSidW requires.
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(|e| err("token", e))?;
            let mut len = 0u32;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            let got = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len);
            let _ = CloseHandle(token);
            got.map_err(|e| err("token user", e))?;
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut text = PWSTR::null();
            ConvertSidToStringSidW(user.User.Sid, &mut text).map_err(|e| err("SID", e))?;
            let sid = text.to_string();
            LocalFree(Some(HLOCAL(text.0.cast())));
            sid.map_err(|e| format!("SID: {e}"))
        }
    }

    fn name() -> Result<BSTR, String> {
        Ok(BSTR::from(task_name(&user_sid()?)))
    }

    /// The Task Scheduler on this PC, connected as this user.
    fn service() -> Result<ITaskService, String> {
        // SAFETY: plain COM calls on a thread with COM initialised (callers
        // hold a Com guard); empty VARIANTs mean "this PC, this user".
        unsafe {
            let service: ITaskService =
                CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER).map_err(|e| err("Task Scheduler", e))?;
            let empty = VARIANT::default();
            service.Connect(&empty, &empty, &empty, &empty).map_err(|e| err("Task Scheduler", e))?;
            Ok(service)
        }
    }

    fn root(service: &ITaskService) -> Result<ITaskFolder, String> {
        // SAFETY: a plain COM call on a connected service.
        unsafe { service.GetFolder(&BSTR::from("\\")) }.map_err(|e| err("task folder", e))
    }

    pub fn task_path() -> Result<Option<PathBuf>, String> {
        let _com = Com::init();
        let name = name()?;
        let root = root(&service()?)?;
        // SAFETY: plain COM calls on an initialised thread; collections are 1-based.
        unsafe {
            let task = match root.GetTask(&name) {
                Ok(task) => task,
                Err(e) if is_not_found(&e) => return Ok(None),
                Err(e) => return Err(err("task", e)),
            };
            let actions = task.Definition().and_then(|d| d.Actions()).map_err(|e| err("task", e))?;
            let mut count = 0;
            actions.Count(&mut count).map_err(|e| err("task", e))?;
            for i in 1..=count {
                let Ok(exec) = actions.get_Item(i).and_then(|a| a.cast::<IExecAction>()) else { continue };
                let mut path = BSTR::new();
                if exec.Path(&mut path).is_ok() {
                    return Ok(Some(PathBuf::from(OsString::from_wide(&path))));
                }
            }
            // A task of that name that starts nothing: report it so it gets replaced.
            Ok(Some(PathBuf::new()))
        }
    }

    pub fn register_task(exe: &Path) -> Result<(), String> {
        let _com = Com::init();
        let sid = BSTR::from(user_sid()?);
        let name = BSTR::from(task_name(&sid.to_string()));
        let service = service()?;
        let root = root(&service)?;
        let fail = |e| err("couldn't create the sign-in task", e);
        let empty = VARIANT::default();
        // SAFETY: plain COM calls on an initialised thread with live interfaces.
        unsafe {
            let def = service.NewTask(0).map_err(fail)?;

            let trigger: ILogonTrigger =
                def.Triggers().and_then(|t| t.Create(TASK_TRIGGER_LOGON)).and_then(|t| t.cast()).map_err(fail)?;
            trigger.SetUserId(&sid).map_err(fail)?;

            let action: IExecAction =
                def.Actions().and_then(|a| a.Create(TASK_ACTION_EXEC)).and_then(|a| a.cast()).map_err(fail)?;
            let wide: Vec<u16> = exe.as_os_str().encode_wide().collect();
            action.SetPath(&BSTR::from_wide(&wide)).map_err(fail)?;

            let principal = def.Principal().map_err(fail)?;
            principal.SetUserId(&sid).map_err(fail)?;
            principal.SetLogonType(LOGON_TYPE).map_err(fail)?;
            principal.SetRunLevel(RUN_LEVEL).map_err(fail)?;

            let settings = def.Settings().map_err(fail)?;
            settings.SetPriority(TASK_PRIORITY).map_err(fail)?;
            settings.SetExecutionTimeLimit(&BSTR::from(EXECUTION_TIME_LIMIT)).map_err(fail)?;
            settings.SetDisallowStartIfOnBatteries(vbool(DISALLOW_START_IF_ON_BATTERIES)).map_err(fail)?;
            settings.SetStopIfGoingOnBatteries(vbool(STOP_IF_GOING_ON_BATTERIES)).map_err(fail)?;
            settings.SetMultipleInstances(MULTIPLE_INSTANCES).map_err(fail)?;
            settings.SetHidden(vbool(HIDDEN)).map_err(fail)?;
            settings.SetStartWhenAvailable(vbool(START_WHEN_AVAILABLE)).map_err(fail)?;

            root.RegisterTaskDefinition(&name, &def, CREATION_FLAGS, &empty, &empty, LOGON_TYPE, &empty)
                .map_err(fail)?;
        }
        Ok(())
    }

    pub fn delete_task() -> Result<(), String> {
        let _com = Com::init();
        let name = name()?;
        let root = root(&service()?)?;
        // SAFETY: a plain COM call on an initialised thread.
        match unsafe { root.DeleteTask(&name, 0) } {
            Ok(()) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(err("couldn't remove the sign-in task", e)),
        }
    }

    /// The Run value is present and not switched off in Task Manager.
    pub fn run_value() -> bool {
        // SAFETY: a presence check: no buffers passed.
        let present = unsafe {
            RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), &HSTRING::from(RUN_VALUE_NAME), RRF_RT_ANY, None, None, None)
        }
        .is_ok();
        present && startup_approved_enabled(startup_approved().as_deref())
    }

    /// The StartupApproved binary for the Run value; None when missing or
    /// unreadable (then the entry counts as on, like the old plugin).
    fn startup_approved() -> Option<Vec<u8>> {
        let key = HSTRING::from(STARTUP_APPROVED_KEY);
        let value = HSTRING::from(RUN_VALUE_NAME);
        let mut size = 0u32;
        // SAFETY: asks only for the size; no data buffer passed.
        let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, &key, &value, RRF_RT_REG_BINARY, None, None, Some(&mut size)) };
        if r.is_err() {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        // SAFETY: `buf` has `size` bytes and the size pointer says so.
        let r = unsafe {
            RegGetValueW(HKEY_CURRENT_USER, &key, &value, RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
        };
        if r.is_err() {
            return None;
        }
        buf.truncate(size as usize);
        Some(buf)
    }

    pub fn set_run_value(exe: &Path) -> Result<(), String> {
        let text: Vec<u16> = run_value_text(exe).encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: the data pointer and byte length describe `text`, NUL included.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(RUN_KEY),
                &HSTRING::from(RUN_VALUE_NAME),
                REG_SZ.0,
                Some(text.as_ptr().cast()),
                (text.len() * 2) as u32,
            )
        }
        .ok()
        .map_err(|e| err("couldn't write the Run value", e))
    }

    /// Removes the Run value and Task Manager's on/off record for it (a
    /// leftover record would otherwise decide for a Run value written later).
    pub fn delete_run_value() -> Result<(), String> {
        let delete = |key: &str| {
            // SAFETY: plain registry call with NUL-terminated strings.
            let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(RUN_VALUE_NAME)) };
            if r == ERROR_FILE_NOT_FOUND || r == ERROR_PATH_NOT_FOUND {
                return Ok(());
            }
            r.ok()
        };
        let run = delete(RUN_KEY).map_err(|e| err("couldn't remove the Run value", e));
        let approved = delete(STARTUP_APPROVED_KEY).map_err(|e| err("couldn't remove the Startup apps record", e));
        run.and(approved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct State {
        task: Option<PathBuf>,
        run: bool,
        run_disabled: bool,
        existing: Vec<PathBuf>,
        fail_register: bool,
        fail_delete_run: bool,
        log: Vec<String>,
    }

    #[derive(Default)]
    struct FakeLoginStart(Mutex<State>);

    impl FakeLoginStart {
        fn with(task: Option<&str>, run: bool) -> Self {
            FakeLoginStart(Mutex::new(State { task: task.map(PathBuf::from), run, ..State::default() }))
        }
        fn disabled_in_task_manager(self) -> Self {
            self.0.lock().unwrap().run_disabled = true;
            self
        }
        fn existing(self, p: &str) -> Self {
            self.0.lock().unwrap().existing.push(PathBuf::from(p));
            self
        }
        fn failing_register(self) -> Self {
            self.0.lock().unwrap().fail_register = true;
            self
        }
        fn failing_delete_run(self) -> Self {
            self.0.lock().unwrap().fail_delete_run = true;
            self
        }
        fn task(&self) -> Option<PathBuf> {
            self.0.lock().unwrap().task.clone()
        }
        fn run(&self) -> bool {
            self.0.lock().unwrap().run
        }
        fn log(&self) -> Vec<String> {
            self.0.lock().unwrap().log.clone()
        }
    }

    #[test]
    fn at_start_first_run_turns_it_on() {
        let fake = FakeLoginStart::default();
        assert!(at_start(&fake, true, Path::new(r"C:\Flick\flick.exe")));
        assert_eq!(fake.task(), Some(PathBuf::from(r"C:\Flick\flick.exe")));
    }

    #[test]
    fn at_start_later_keeps_off_off_and_moves_old_installs() {
        let exe = Path::new(r"C:\Flick\flick.exe");
        let off = FakeLoginStart::default();
        assert!(!at_start(&off, false, exe));
        assert_eq!(off.task(), None);
        assert!(!off.run());
        let old = FakeLoginStart::with(None, true);
        assert!(at_start(&old, false, exe));
        assert_eq!(old.task(), Some(exe.to_path_buf()));
        assert!(!old.run());
    }

    #[test]
    fn remove_for_uninstall_says_whether_it_was_on() {
        let task = FakeLoginStart::with(Some(EXE), false);
        assert!(remove_for_uninstall(&task));
        assert_eq!(task.task(), None);
        let run = FakeLoginStart::with(None, true);
        assert!(remove_for_uninstall(&run));
        assert!(!run.run());
        let off = FakeLoginStart::default();
        assert!(!remove_for_uninstall(&off));
    }

    #[test]
    fn restore_marker_is_next_to_the_config() {
        let config = Path::new(r"C:\Users\u\AppData\Roaming\Flick\config.toml");
        assert_eq!(
            restore_marker(config),
            PathBuf::from(r"C:\Users\u\AppData\Roaming\Flick\start-at-login.restore")
        );
    }

    #[test]
    fn a_restore_after_a_manual_upgrade_turns_it_back_on() {
        // The old version's uninstaller removed the task and left the marker.
        let after_upgrade = FakeLoginStart::default();
        assert!(at_start(&after_upgrade, true, Path::new(EXE)));
        assert_eq!(after_upgrade.task(), Some(PathBuf::from(EXE)));
    }

    impl LoginStart for FakeLoginStart {
        fn task_path(&self) -> Result<Option<PathBuf>, String> {
            let mut st = self.0.lock().unwrap();
            st.log.push("task_path".into());
            Ok(st.task.clone())
        }
        fn register_task(&self, exe: &Path) -> Result<(), String> {
            let mut st = self.0.lock().unwrap();
            st.log.push(format!("register_task {}", exe.display()));
            if st.fail_register {
                return Err("denied".into());
            }
            st.task = Some(exe.to_path_buf());
            Ok(())
        }
        fn delete_task(&self) -> Result<(), String> {
            let mut st = self.0.lock().unwrap();
            st.log.push("delete_task".into());
            st.task = None;
            Ok(())
        }
        fn run_value(&self) -> bool {
            let mut st = self.0.lock().unwrap();
            st.log.push("run_value".into());
            st.run && !st.run_disabled
        }
        fn exe_exists(&self, p: &Path) -> bool {
            self.0.lock().unwrap().existing.iter().any(|e| same_path(e, p))
        }
        fn set_run_value(&self, exe: &Path) -> Result<(), String> {
            let mut st = self.0.lock().unwrap();
            st.log.push(format!("set_run_value {}", exe.display()));
            st.run = true;
            Ok(())
        }
        fn delete_run_value(&self) -> Result<(), String> {
            let mut st = self.0.lock().unwrap();
            st.log.push("delete_run_value".into());
            if st.fail_delete_run {
                return Err("denied".into());
            }
            st.run = false;
            Ok(())
        }
    }

    const EXE: &str = r"C:\Program Files\Flick\flick.exe";

    #[test]
    fn set_on_from_nothing_registers_the_task_only() {
        let f = FakeLoginStart::default();
        assert!(set(&f, true, Path::new(EXE)));
        assert_eq!(f.task(), Some(PathBuf::from(EXE)));
        assert!(!f.run());
    }

    #[test]
    fn set_on_with_run_value_moves_it_to_the_task() {
        let f = FakeLoginStart::with(None, true);
        assert!(set(&f, true, Path::new(EXE)));
        assert_eq!(f.task(), Some(PathBuf::from(EXE)));
        assert!(!f.run());
    }

    #[test]
    fn set_on_falls_back_to_the_run_value_when_registering_fails() {
        let f = FakeLoginStart::default().failing_register();
        assert!(set(&f, true, Path::new(EXE)));
        assert_eq!(f.task(), None);
        assert!(f.run());
        assert!(is_enabled(&f));
        assert!(f.log().contains(&format!("set_run_value {EXE}")));
    }

    #[test]
    fn set_on_fallback_removes_a_stale_task() {
        let f = FakeLoginStart::with(Some(r"C:\Old\flick.exe"), false).failing_register();
        assert!(set(&f, true, Path::new(EXE)));
        assert_eq!(f.task(), None);
        assert!(f.run());
    }

    #[test]
    fn set_on_keeps_only_the_run_value_when_it_cannot_be_removed() {
        // Never both: a Run value that won't go would start Flick a second time.
        let f = FakeLoginStart::with(None, true).failing_delete_run();
        assert!(set(&f, true, Path::new(EXE)));
        assert_eq!(f.task(), None);
        assert!(f.run());
    }

    #[test]
    fn set_off_removes_both() {
        let f = FakeLoginStart::with(Some(EXE), true);
        assert!(!set(&f, false, Path::new(EXE)));
        assert_eq!(f.task(), None);
        assert!(!f.run());
    }

    #[test]
    fn is_enabled_with_either_one() {
        assert!(is_enabled(&FakeLoginStart::with(Some(EXE), false)));
        assert!(is_enabled(&FakeLoginStart::with(None, true)));
        assert!(!is_enabled(&FakeLoginStart::default()));
    }

    #[test]
    fn migrate_moves_a_run_value_to_the_task() {
        let f = FakeLoginStart::with(None, true);
        migrate(&f, Path::new(EXE));
        assert_eq!(f.task(), Some(PathBuf::from(EXE)));
        assert!(!f.run());
    }

    #[test]
    fn migrate_with_both_removes_the_run_value() {
        let f = FakeLoginStart::with(Some(EXE), true);
        migrate(&f, Path::new(EXE));
        assert_eq!(f.task(), Some(PathBuf::from(EXE)));
        assert!(!f.run());
    }

    #[test]
    fn migrate_re_registers_a_task_for_another_exe() {
        let f = FakeLoginStart::with(Some(r"C:\Old\flick.exe"), false);
        migrate(&f, Path::new(EXE));
        assert_eq!(f.task(), Some(PathBuf::from(EXE)));
        assert!(!f.run());
    }

    #[test]
    fn migrate_leaves_a_task_for_another_exe_that_still_exists() {
        // A second install (or a portable copy) must not steal the task.
        let f = FakeLoginStart::with(Some(r"C:\Old\flick.exe"), false).existing(r"C:\Old\flick.exe");
        migrate(&f, Path::new(EXE));
        assert_eq!(f.task(), Some(PathBuf::from(r"C:\Old\flick.exe")));
        assert!(!f.log().iter().any(|c| c.starts_with("register_task")));
    }

    #[test]
    fn migrate_re_points_a_task_whose_exe_is_gone() {
        let f = FakeLoginStart::with(Some(r"C:\Old\flick.exe"), false);
        migrate(&f, Path::new(EXE));
        assert_eq!(f.task(), Some(PathBuf::from(EXE)));
    }

    #[test]
    fn migrate_re_points_a_task_with_an_empty_path() {
        let f = FakeLoginStart::with(Some(""), false);
        migrate(&f, Path::new(EXE));
        assert_eq!(f.task(), Some(PathBuf::from(EXE)));
    }

    #[test]
    fn migrate_leaves_a_run_value_disabled_in_task_manager_alone() {
        let f = FakeLoginStart::with(None, true).disabled_in_task_manager();
        migrate(&f, Path::new(EXE));
        assert_eq!(f.task(), None);
        assert!(f.run(), "the user's Task Manager entry stays as it was");
        assert!(!is_enabled(&f));
        assert!(!at_start(&f, false, Path::new(EXE)));
    }

    #[test]
    fn startup_approved_rule_matches_the_old_plugin() {
        // No value: enabled.
        assert!(startup_approved_enabled(None));
        // Enabled as Windows writes it: 02 00 00 00 + 8 zero bytes.
        assert!(startup_approved_enabled(Some(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])));
        // Disabled: 03 00 00 00 + a non-zero FILETIME.
        assert!(!startup_approved_enabled(Some(&[3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8])));
        // Only the last 8 bytes count.
        assert!(startup_approved_enabled(Some(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])));
        assert!(!startup_approved_enabled(Some(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9])));
        // Shorter than 8 bytes: the plugin treats it as enabled.
        assert!(startup_approved_enabled(Some(&[3, 0, 0])));
        assert!(startup_approved_enabled(Some(&[])));
    }

    #[test]
    fn migrate_leaves_a_matching_task_alone() {
        let f = FakeLoginStart::with(Some(EXE), false);
        migrate(&f, Path::new(EXE));
        assert!(!f.log().iter().any(|c| c.starts_with("register_task")));
    }

    #[test]
    fn migrate_with_nothing_creates_nothing() {
        let f = FakeLoginStart::default();
        migrate(&f, Path::new(EXE));
        assert_eq!(f.task(), None);
        assert!(!f.run());
        assert!(
            !f.log().iter().any(|c| c.starts_with("register_task") || c.starts_with("set_run_value")),
            "{:?}",
            f.log()
        );
    }

    #[test]
    fn remove_all_with_nothing_is_fine() {
        let f = FakeLoginStart::default();
        remove_all(&f);
        assert_eq!(f.task(), None);
        assert!(!f.run());
        assert!(f.log().contains(&"delete_task".to_string()));
        assert!(f.log().contains(&"delete_run_value".to_string()));
    }

    #[test]
    fn remove_all_removes_both() {
        let f = FakeLoginStart::with(Some(EXE), true);
        remove_all(&f);
        assert_eq!(f.task(), None);
        assert!(!f.run());
    }

    #[test]
    fn task_name_carries_the_sid() {
        assert_eq!(task_name("S-1-5-21-1-2-3-1001"), "Flick (start at sign-in) [S-1-5-21-1-2-3-1001]");
    }

    #[test]
    fn run_value_text_is_the_quoted_exe() {
        assert_eq!(run_value_text(Path::new(EXE)), format!("\"{EXE}\""));
    }

    #[test]
    fn task_definition_values_are_pinned() {
        assert_eq!(TASK_PRIORITY, 5, "normal priority, not the task default 7");
        assert_eq!(EXECUTION_TIME_LIMIT, "PT0S", "no time limit");
        // Starts and keeps running on battery, not hidden, no catch-up run.
        assert_eq!(
            [DISALLOW_START_IF_ON_BATTERIES, STOP_IF_GOING_ON_BATTERIES, HIDDEN, START_WHEN_AVAILABLE],
            [false; 4]
        );
        assert_eq!(MULTIPLE_INSTANCES, TASK_INSTANCES_IGNORE_NEW);
        assert_eq!(LOGON_TYPE, TASK_LOGON_INTERACTIVE_TOKEN);
        assert_eq!(RUN_LEVEL, TASK_RUNLEVEL_LUA);
        assert_eq!(CREATION_FLAGS, TASK_CREATE_OR_UPDATE.0);
        assert_eq!(RUN_KEY, r"Software\Microsoft\Windows\CurrentVersion\Run");
        assert_eq!(RUN_VALUE_NAME, "Flick");
    }

    #[test]
    fn remove_login_task_switch_is_recognised() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(wants_remove_login_task(&args(&["flick.exe", "--remove-login-task"])));
        assert!(wants_remove_login_task(&args(&["flick.exe", "x", "--remove-login-task"])));
        assert!(!wants_remove_login_task(&args(&["flick.exe"])));
        assert!(!wants_remove_login_task(&args(&["flick.exe", "--remove-login-task-x"])));
        assert!(!wants_remove_login_task(&args(&["--remove-login-taskx"])));
    }

    /// Registers the real task for an exe that doesn't exist, reads it back
    /// and deletes it. Run by hand: `cargo test -- --ignored real_task_round_trip`.
    #[test]
    #[ignore]
    fn real_task_round_trip() {
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let r = WindowsLoginStart.delete_task();
                println!("cleanup delete_task: {r:?}");
            }
        }
        let exe = Path::new(r"C:\definitely-not-here\flick-probe.exe");
        assert!(!exe.exists());
        let s = WindowsLoginStart;
        println!("task name: {}", task_name(&user_sid().unwrap()));
        println!("before: {:?}", s.task_path());
        let _cleanup = Cleanup;
        println!("register: {:?}", s.register_task(exe));
        let back = s.task_path();
        println!("read back: {back:?}");
        assert_eq!(back, Ok(Some(exe.to_path_buf())));
        println!("delete: {:?}", s.delete_task());
        let gone = s.task_path();
        println!("after delete: {gone:?}");
        assert_eq!(gone, Ok(None));
        println!("delete again (absent): {:?}", s.delete_task());
        assert_eq!(s.delete_task(), Ok(()));
    }
}
