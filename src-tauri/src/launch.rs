//! Launching via the Windows shell (`ShellExecuteExW`, shell error UI suppressed),
//! and the `Launcher` seam that providers use so their tests never start a
//! process. Quoting and command lines live in `quote` and are re-exported here.

use std::path::{Path, PathBuf};
use std::process::Command;

use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    GetLastError, SetLastError, ERROR_ACCESS_DENIED, ERROR_BAD_EXE_FORMAT, ERROR_CANCELLED,
    ERROR_DDE_FAIL, ERROR_DIRECTORY, ERROR_DLL_NOT_FOUND, ERROR_FILE_NOT_FOUND, ERROR_NOT_ENOUGH_MEMORY,
    ERROR_NO_ASSOCIATION, ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, WIN32_ERROR, CloseHandle,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, STGM_READ,
};
use windows::Win32::System::Threading::{
    CreateProcessW, CREATE_NEW_CONSOLE, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION,
    STARTUPINFOW,
};
use windows::Win32::UI::Shell::{
    IShellLinkW, ShellExecuteExW, ShellLink, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC,
    SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{SHOW_WINDOW_CMD, SW_HIDE, SW_SHOWNORMAL};

use crate::config;
use crate::quote::{system32, windows_dir, windows_env_value};
pub use crate::quote::{
    bind_bash, bind_windows, check_windows_value, command_line, to_wsl_path,
    valid_distro, CommandLine, Shell,
};

/// Error text for a dismissed UAC prompt. The command layer compares against
/// it to hide the bar silently instead of showing an error.
pub const CANCELLED: &str = "cancelled";

/// Open a file, folder or shortcut with its default action. A folder is
/// opened as a folder (see [`open_target`]).
pub fn open(target: &str) -> Result<(), String> {
    shell_execute("open", &open_target(target), "", SW_SHOWNORMAL, None)
}

/// `target` with a trailing `\` when it is an existing folder without one:
/// given `C:\x\setup` the shell may run `C:\x\setup.exe` (or `.bat`, `.cmd`,
/// `.com`) beside it instead of opening the folder; `C:\x\setup\` can only be
/// the folder. Anything else is returned as it is.
fn open_target(target: &str) -> String {
    if !target.ends_with('\\') && Path::new(target).is_dir() {
        format!(r"{target}\")
    } else {
        target.to_string()
    }
}

/// Open as administrator (shows the UAC prompt; blocks until it is answered).
pub fn open_elevated(target: &str) -> Result<(), String> {
    shell_execute("runas", target, "", SW_SHOWNORMAL, None)
}

/// Open a URI such as `ms-settings:`, `shell:AppsFolder\...` or `https:`.
pub fn open_uri(uri: &str) -> Result<(), String> {
    shell_execute("open", uri, "", SW_SHOWNORMAL, None)
}

/// Start `file` with a parameter string (passed to the program as is), as
/// administrator when `elevated`. Same flags and errors as [`open`].
pub fn open_with(file: &str, params: &str, elevated: bool) -> Result<(), String> {
    shell_execute(verb(elevated), file, params, SW_SHOWNORMAL, None)
}

fn verb(elevated: bool) -> &'static str {
    if elevated {
        "runas"
    } else {
        "open"
    }
}

/// Open the config file; with no program associated with `.toml`, open it in
/// Notepad instead. Blocks while the shell works: call it off the main thread.
pub fn open_config() -> Result<(), String> {
    let path = config::config_path();
    if open(&path.to_string_lossy()).is_ok() {
        return Ok(());
    }
    Command::new(notepad_exe())
        .arg(&path)
        .spawn()
        .map(drop)
        .map_err(|_| "could not open the config".to_string())
}

/// Notepad by absolute path, so no `notepad.exe` next to Flick or on `PATH`
/// is run instead.
fn notepad_exe() -> PathBuf {
    PathBuf::from(system32("notepad.exe"))
}

// ---- Launcher --------------------------------------------------------------

/// Everything providers start goes through this, so tests can record instead.
pub trait Launcher: Send + Sync {
    fn open(&self, target: &str) -> Result<(), String>;
    fn open_with(&self, file: &str, params: &str, elevated: bool) -> Result<(), String>;
    /// Run `command` in `shell`. `env` holds the values of the
    /// [`bind_windows`] references (`FLICK_X`); WSL commands carry theirs in
    /// the script ([`bind_bash`]) and pass an empty slice. `cwd` is the
    /// Windows folder to start in (a picked folder); `None` starts Windows
    /// commands in Flick's own directory (the user profile) and WSL commands
    /// in the Linux home.
    fn run_command(
        &self,
        shell: &Shell,
        command: &str,
        env: &[(String, String)],
        keep_open: bool,
        elevated: bool,
        cwd: Option<&str>,
    ) -> Result<(), String>;
    /// Explorer with the item selected.
    fn reveal(&self, path: &str) -> Result<(), String>;
}

/// The real thing: thin wrappers over the shell.
pub struct SystemLauncher;

impl Launcher for SystemLauncher {
    fn open(&self, target: &str) -> Result<(), String> {
        open(target)
    }

    fn open_with(&self, file: &str, params: &str, elevated: bool) -> Result<(), String> {
        open_with(file, params, elevated)
    }

    /// Without `env` (every WSL command, and Windows commands without
    /// placeholders): through the shell, `ShellExecuteExW` with `open` or
    /// `runas`, hidden or shown. With `env` (Windows, never elevated: an
    /// elevated process does not inherit this one's environment): started
    /// directly by [`create_process`] with the values added to its
    /// environment, and not waited for.
    ///
    /// `cwd` goes to the API's directory parameter (`lpDirectory`,
    /// `lpCurrentDirectory`), never into the command line; wsl.exe translates
    /// its own directory to `/mnt/...`. A WSL command without `cwd` gets
    /// `--cd ~`. Elevated commands take neither: Windows starts them in
    /// System32 regardless.
    fn run_command(
        &self,
        shell: &Shell,
        command: &str,
        env: &[(String, String)],
        keep_open: bool,
        elevated: bool,
        cwd: Option<&str>,
    ) -> Result<(), String> {
        match shell {
            Shell::Wsl { distro } if !valid_distro(distro) => return Err("invalid wsl distro name".into()),
            Shell::Wsl { .. } if !env.is_empty() => {
                return Err("internal: wsl commands carry their values in the script".into())
            }
            _ => {}
        }
        for (_, value) in env {
            check_windows_value(value)?;
        }
        let cwd = if elevated { None } else { cwd };
        if env.is_empty() {
            let home = matches!(shell, Shell::Wsl { .. }) && !elevated && cwd.is_none();
            let line = command_line(shell, command, keep_open, false, home);
            let show = if line.hidden { SW_HIDE } else { SW_SHOWNORMAL };
            return shell_execute(verb(elevated), &line.program, &line.params, show, cwd);
        }
        if elevated {
            return Err("admin commands cannot take arguments".into());
        }
        create_process(&command_line(shell, command, keep_open, true, false), env, cwd)
    }

    fn reveal(&self, path: &str) -> Result<(), String> {
        // Windows paths cannot contain `"`; one here would end the argument.
        if path.contains('"') {
            return Err("invalid path".into());
        }
        if path.trim().is_empty() {
            return Err("nothing to open".into());
        }
        let explorer = format!(r"{}\explorer.exe", windows_dir());
        open_with(&explorer, &format!("/select,\"{path}\""), false)
    }
}

/// The environment for [`create_process`]: `base` with `extra` added (an
/// `extra` name replaces a `base` entry of the same name, compared without
/// case), sorted by name without case as `CreateProcessW` expects, each entry
/// `NAME=value` and NUL, plus one more NUL at the end.
fn environment_block(base: impl Iterator<Item = (String, String)>, extra: &[(String, String)]) -> Vec<u16> {
    let mut entries: Vec<(String, String)> =
        base.filter(|(k, _)| !extra.iter().any(|(e, _)| e.to_uppercase() == k.to_uppercase())).collect();
    entries.extend(extra.iter().cloned());
    entries.sort_by_key(|(k, _)| k.to_uppercase());
    let mut block = Vec::new();
    for (k, v) in &entries {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    if entries.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// Starts `program` with `args` (each one argument, quoted for CreateProcess)
/// in `cwd`, in a console window of its own when it is a console program.
/// Not waited for.
pub(crate) fn start_in(program: &str, args: &[String], cwd: &str) -> Result<(), String> {
    let params = args.iter().map(|a| crate::quote::quote_argv(a)).collect::<Vec<_>>().join(" ");
    create_process(&CommandLine { program: program.to_string(), params, hidden: false }, &[], Some(cwd))
}

/// Start `line` directly with `env` (values through `windows_env_value`)
/// added to this process's environment. `CreateProcessW` with no inherited
/// handles and no `STARTF_USESTDHANDLES`, so a visible console
/// (`CREATE_NEW_CONSOLE`) gets its own standard handles. Started in `cwd`
/// (`lpCurrentDirectory`), else in this process's directory. Not waited for:
/// both returned handles are closed at once.
fn create_process(line: &CommandLine, env: &[(String, String)], cwd: Option<&str>) -> Result<(), String> {
    let extra: Vec<(String, String)> = env.iter().map(|(k, v)| (k.clone(), windows_env_value(v))).collect();
    let base = std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()));
    let block = environment_block(base, &extra);
    let program = wide(&line.program);
    let mut command_line = wide(&format!("\"{}\" {}", line.program, line.params));
    let dir = cwd.map(wide);
    let window = if line.hidden { CREATE_NO_WINDOW } else { CREATE_NEW_CONSOLE };
    let startup = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: every buffer is NUL-terminated (the environment block double
    // NUL-terminated, UTF-16 as CREATE_UNICODE_ENVIRONMENT says) and outlives
    // the call; the command-line buffer is writable as CreateProcessW needs.
    // The returned handles are closed exactly once.
    unsafe {
        SetLastError(WIN32_ERROR(0));
        let started = CreateProcessW(
            PCWSTR(program.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT | window,
            Some(block.as_ptr() as *const std::ffi::c_void),
            dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            &startup,
            &mut info,
        );
        if started.is_err() {
            return Err(error_message(GetLastError()));
        }
        let _ = CloseHandle(info.hThread);
        let _ = CloseHandle(info.hProcess);
    }
    Ok(())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `ShellExecuteExW` of `file` with `params`, started in `dir` when given
/// (`lpDirectory`), else in this process's directory.
fn shell_execute(verb: &str, file: &str, params: &str, show: SHOW_WINDOW_CMD, dir: Option<&str>) -> Result<(), String> {
    if file.trim().is_empty() {
        return Err("nothing to open".into());
    }
    let (verb, file, params) = (wide(verb), wide(file), wide(params));
    let dir = dir.map(wide);
    // NO_UI: errors (missing file, broken shortcut, no association) come back
    // as codes instead of shell dialogs. The UAC consent prompt for "runas"
    // is not shell error UI and still appears. NOASYNC: finish before return.
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: if params.len() > 1 { PCWSTR(params.as_ptr()) } else { PCWSTR::null() },
        lpDirectory: dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
        nShow: show.0,
        ..Default::default()
    };
    // SAFETY: `info` is fully initialised and the NUL-terminated buffers
    // outlive the call. COM is initialised as MSDN recommends for
    // ShellExecuteEx and balanced only when that init succeeded.
    unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        SetLastError(WIN32_ERROR(0));
        let result = ShellExecuteExW(&mut info);
        let last_error = GetLastError();
        // With NO_UI, a shortcut the shell cannot resolve fails as
        // ERROR_CANCELLED, same as a dismissed UAC prompt. Tell them apart.
        let broken_link = result.is_err()
            && last_error == ERROR_CANCELLED
            && shortcut_target_missing(&file);
        if com.is_ok() {
            CoUninitialize();
        }
        match result {
            Ok(()) => Ok(()),
            Err(_) if broken_link => Err("shortcut target not found".into()),
            Err(_) => Err(error_message(last_error)),
        }
    }
}

/// True when `file` (NUL-terminated) is a `.lnk` whose stored target path is
/// not on disk. Reads the link without resolving it, so no UI. Links with no
/// file-system path (advertised installs, shell items) report false.
///
/// # Safety
/// COM must be initialised on this thread.
unsafe fn shortcut_target_missing(file: &[u16]) -> bool {
    // SAFETY: the caller has initialised COM on this thread.
    unsafe { stored_link_target(file) }.is_some_and(|target| !Path::new(&target).exists())
}

/// The file-system path stored in `lnk`, read without resolving it (no UI, no
/// search for a moved target). `None` when `lnk` is not a readable `.lnk` or
/// stores no file-system path (advertised installs, shell items).
/// Call inside [`with_com`] when reading many links: the nested init here is
/// then only a reference count, not a COM load and unload per link.
pub fn shortcut_target(lnk: &str) -> Option<String> {
    let file = wide(lnk);
    // SAFETY: `with_com` initialises COM on this thread for the read.
    with_com(|| unsafe { stored_link_target(&file) })
}

/// The stored target of `lnk` and whether the link passes command-line
/// arguments (web apps, Office and other launchers do). Same rules and COM
/// advice as [`shortcut_target`]; unreadable arguments count as present.
pub fn shortcut_details(lnk: &str) -> Option<(String, bool)> {
    let file = wide(lnk);
    // SAFETY: `with_com` initialises COM on this thread for the read.
    with_com(|| unsafe { stored_link(&file) })
}

/// Run `f` with COM initialised on this thread, balanced only when that init
/// succeeded (an already-initialised thread keeps working either way).
pub fn with_com<R>(f: impl FnOnce() -> R) -> R {
    // SAFETY: plain COM init/uninit pair on the current thread.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    // Balanced on unwind too: a panic in `f` must not leak the init count.
    struct Uninit(bool);
    impl Drop for Uninit {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }
    let _uninit = Uninit(com.is_ok());
    f()
}

/// # Safety
/// COM must be initialised on this thread; `file` is NUL-terminated.
unsafe fn stored_link_target(file: &[u16]) -> Option<String> {
    unsafe { stored_link(file) }.map(|(target, _)| target)
}

/// # Safety
/// COM must be initialised on this thread; `file` is NUL-terminated.
unsafe fn stored_link(file: &[u16]) -> Option<(String, bool)> {
    let path = String::from_utf16_lossy(&file[..file.len() - 1]);
    let is_lnk = Path::new(&path)
        .extension()
        .is_some_and(|x| x.eq_ignore_ascii_case("lnk"));
    if !is_lnk {
        return None;
    }
    unsafe {
        let link = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist = link.cast::<IPersistFile>().ok()?;
        persist.Load(PCWSTR(file.as_ptr()), STGM_READ).ok()?;
        let mut buf = [0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let target = (len > 0).then(|| String::from_utf16_lossy(&buf[..len]))?;
        let mut args = [0u16; 2048];
        let has_args = link.GetArguments(&mut args).map_or(true, |_| args[0] != 0);
        Some((target, has_args))
    }
}

/// Short, human-readable text for a failed launch (shown in the bar).
fn error_message(err: WIN32_ERROR) -> String {
    match err {
        ERROR_CANCELLED => CANCELLED.into(),
        ERROR_FILE_NOT_FOUND => "file not found".into(),
        ERROR_PATH_NOT_FOUND => "path not found".into(),
        ERROR_DIRECTORY => "folder not found".into(),
        ERROR_ACCESS_DENIED => "access denied".into(),
        ERROR_NO_ASSOCIATION => "no app is associated with this file".into(),
        ERROR_BAD_EXE_FORMAT => "not a valid program".into(),
        ERROR_SHARING_VIOLATION => "file is in use".into(),
        ERROR_DDE_FAIL => "the app did not respond".into(),
        ERROR_DLL_NOT_FOUND => "a required DLL was not found".into(),
        ERROR_NOT_ENOUGH_MEMORY => "out of memory".into(),
        WIN32_ERROR(code) => format!("could not open (error {code})"),
    }
}

#[cfg(test)]
pub mod testing {
    use std::sync::Mutex;

    use super::{Launcher, Shell};

    /// Records one line per call and starts nothing.
    #[derive(Default)]
    pub struct RecordingLauncher {
        pub calls: Mutex<Vec<String>>,
        fail: Mutex<Option<String>>,
    }

    impl RecordingLauncher {
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        /// The next call is still recorded, then returns `Err(error)`.
        pub fn fail_next(&self, error: &str) {
            *self.fail.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.to_string());
        }

        fn record(&self, line: String) -> Result<(), String> {
            self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(line);
            match self.fail.lock().unwrap_or_else(|e| e.into_inner()).take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
    }

    impl Launcher for RecordingLauncher {
        fn open(&self, target: &str) -> Result<(), String> {
            self.record(format!("open {target}"))
        }

        fn open_with(&self, file: &str, params: &str, elevated: bool) -> Result<(), String> {
            self.record(format!("open_with {file} elevated={elevated}: {params}"))
        }

        fn run_command(
            &self,
            shell: &Shell,
            command: &str,
            env: &[(String, String)],
            keep_open: bool,
            elevated: bool,
            cwd: Option<&str>,
        ) -> Result<(), String> {
            let shell = match shell {
                Shell::Windows => "Windows".to_string(),
                Shell::Wsl { distro } => format!("Wsl({distro})"),
            };
            let env: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            self.record(format!(
                "run_command {shell} env=[{}] keep={keep_open} elevated={elevated} cwd={}: {command}",
                env.join(", "),
                cwd.unwrap_or("-")
            ))
        }

        fn reveal(&self, path: &str) -> Result<(), String> {
            self.record(format!("reveal {path}"))
        }
    }
}

/// Whether Windows knows what opens `<scheme>:` links (an app registered
/// the scheme). Reads the registry; never called by tests.
/// Whether the Claude app is installed (its `claude://` link is registered),
/// as last looked at by the running app (`set_claude_app`); false in tests.
static CLAUDE_APP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn claude_app() -> bool {
    CLAUDE_APP.load(std::sync::atomic::Ordering::Relaxed)
}

/// Looks again whether the Claude app is installed; the result.
pub fn set_claude_app() -> bool {
    let installed = link_scheme_registered("claude");
    CLAUDE_APP.store(installed, std::sync::atomic::Ordering::Relaxed);
    installed
}

pub fn link_scheme_registered(scheme: &str) -> bool {
    use windows::core::{w, HSTRING};
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CLASSES_ROOT, RRF_RT_REG_SZ};
    // SAFETY: a value read with no out-buffers: only whether it exists.
    unsafe { RegGetValueW(HKEY_CLASSES_ROOT, &HSTRING::from(scheme), w!("URL Protocol"), RRF_RT_REG_SZ, None, None, None) }
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    #[test]
    fn uac_cancel_reads_cancelled() {
        assert_eq!(error_message(ERROR_CANCELLED), "cancelled");
    }

    #[test]
    fn known_codes_have_messages() {
        assert_eq!(error_message(ERROR_FILE_NOT_FOUND), "file not found");
        assert_eq!(error_message(ERROR_DIRECTORY), "folder not found");
        assert_eq!(error_message(ERROR_ACCESS_DENIED), "access denied");
        assert_eq!(error_message(ERROR_NO_ASSOCIATION), "no app is associated with this file");
        assert_eq!(error_message(WIN32_ERROR(17)), "could not open (error 17)");
    }

    #[test]
    fn empty_or_blank_target_is_err() {
        assert_eq!(open(""), Err("nothing to open".to_string()));
        assert_eq!(open("  \t"), Err("nothing to open".to_string()));
    }

    #[test]
    fn folders_open_as_folders() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("setup");
        std::fs::create_dir(&folder).unwrap();
        let file = dir.path().join("setup.bat");
        std::fs::write(&file, b"").unwrap();
        let folder = folder.to_str().unwrap();
        let file = file.to_str().unwrap();
        // A folder gets a trailing backslash, so the shell cannot pick `setup.bat` instead.
        assert_eq!(open_target(folder), format!(r"{folder}\"));
        assert_eq!(open_target(&format!(r"{folder}\")), format!(r"{folder}\"));
        // Everything else is passed as it is.
        assert_eq!(open_target(file), file);
        assert_eq!(open_target("ms-settings:bluetooth"), "ms-settings:bluetooth");
        assert_eq!(open_target(r"C:\definitely\missing"), r"C:\definitely\missing");
        assert_eq!(open_target(""), "");
    }

    #[test]
    fn notepad_fallback_is_an_absolute_system32_path() {
        let exe = notepad_exe();
        assert!(exe.is_absolute() && exe.ends_with(r"System32\notepad.exe"), "{}", exe.display());
    }

    #[test]
    fn missing_file_is_err() {
        // Target does not exist, so nothing is launched.
        assert!(open(r"C:\definitely\missing.lnk").is_err());
    }

    #[test]
    fn shortcut_target_of_missing_or_non_link_is_none() {
        assert_eq!(shortcut_target(r"C:\definitely\missing.lnk"), None);
        assert_eq!(shortcut_target(r"C:\Windows\notepad.exe"), None);
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("Empty.lnk");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(with_com(|| shortcut_target(empty.to_str().unwrap())), None);
    }

    #[test]
    fn broken_shortcut_is_err_without_dialog() {
        // A real .lnk whose target does not exist: without NO_UI the shell
        // would show "Problem with Shortcut" and block here.
        let dir = tempfile::tempdir().unwrap();
        let lnk = dir.path().join("Broken.lnk");
        let script = format!(
            "$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); \
             $s.TargetPath = 'C:\\definitely\\missing\\nothing.exe'; $s.Save()",
            lnk.display()
        );
        let status = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .status()
            .unwrap();
        assert!(status.success() && lnk.exists(), "could not create test shortcut");
        assert_eq!(open(lnk.to_str().unwrap()), Err("shortcut target not found".to_string()));
    }

    // ---- launcher ------------------------------------------------------

    fn debian() -> Shell {
        Shell::Wsl { distro: "Debian".into() }
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    // Each refusal below happens before anything starts.

    #[test]
    fn invalid_distro_is_refused() {
        for distro in ["debian linux", "x\" & echo", "", "a;b", "-d", "--exec"] {
            let shell = Shell::Wsl { distro: distro.into() };
            assert_eq!(
                SystemLauncher.run_command(&shell, "true", &[], false, false, None),
                Err("invalid wsl distro name".to_string()),
                "{distro}"
            );
        }
    }

    #[test]
    fn wsl_commands_carry_values_in_the_script() {
        assert_eq!(
            SystemLauncher.run_command(&debian(), "true", &env(&[("FLICK_A", "x")]), false, false, None),
            Err("internal: wsl commands carry their values in the script".to_string())
        );
    }

    #[test]
    fn admin_commands_cannot_take_arguments() {
        let cmd = r#"prog "!FLICK_A!""#;
        assert_eq!(
            SystemLauncher.run_command(&Shell::Windows, cmd, &env(&[("FLICK_A", "x")]), false, true, None),
            Err("admin commands cannot take arguments".to_string())
        );
    }

    #[test]
    fn unsafe_windows_values_are_refused() {
        let cmd = r#"prog "!FLICK_A!""#;
        for (bad, message) in [
            ("a\"b", r#"cannot pass text containing " or % to this command"#),
            ("100%", r#"cannot pass text containing " or % to this command"#),
            ("a\nb", "argument contains characters that cannot be passed safely"),
            ("a\0b", "argument contains characters that cannot be passed safely"),
        ] {
            assert_eq!(
                SystemLauncher.run_command(&Shell::Windows, cmd, &env(&[("FLICK_A", bad)]), false, false, None),
                Err(message.to_string()),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn create_process_errors_read_like_open_errors() {
        // Target does not exist, so nothing starts.
        let program = format!(r"{}\definitely-missing-flick.exe", windows_dir());
        let line = CommandLine { program, params: "x".into(), hidden: true };
        assert_eq!(create_process(&line, &env(&[("FLICK_A", "x")]), None), Err("file not found".to_string()));
        let line = CommandLine { program: r"C:\definitely\missing.exe".into(), params: "x".into(), hidden: true };
        assert_eq!(create_process(&line, &env(&[("FLICK_A", "x")]), None), Err("path not found".to_string()));
    }

    fn block_entries(block: &[u16]) -> Vec<String> {
        assert!(block.ends_with(&[0, 0]), "block must end with two NULs");
        String::from_utf16(&block[..block.len() - 2])
            .unwrap()
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn environment_block_merges_sorts_and_terminates() {
        let base = env(&[("Path", r"C:\bin"), ("flick_a", "old"), ("TEMP", r"C:\t"), ("ALLUSERSPROFILE", "x")]);
        let extra = env(&[("FLICK_A", "a & b"), ("FLICK_B", "Résumé")]);
        let block = environment_block(base.into_iter(), &extra);
        assert_eq!(
            block_entries(&block),
            vec!["ALLUSERSPROFILE=x", "FLICK_A=a & b", "FLICK_B=Résumé", r"Path=C:\bin", r"TEMP=C:\t"]
        );
        // One NUL after each entry, one more at the end.
        let text: String = String::from_utf16(&block).unwrap();
        assert!(text.ends_with("TEMP=C:\\t\0\0"), "{text:?}");
        // Empty environment: still a valid (double-NUL) block.
        assert_eq!(environment_block(std::iter::empty(), &[]), vec![0, 0]);
    }

    #[test]
    fn recording_launcher_records_and_launches_nothing() {
        use testing::RecordingLauncher;
        let rec = RecordingLauncher::default();
        let l: &dyn Launcher = &rec;
        assert_eq!(l.open(r"C:\x\a.txt"), Ok(()));
        assert_eq!(l.open_with("rundll32.exe", "sysdm.cpl,EditEnvironmentVariables", false), Ok(()));
        assert_eq!(l.run_command(&debian(), "ls -la", &[], true, false, None), Ok(()));
        assert_eq!(l.run_command(&Shell::Windows, "dir", &[], false, true, None), Ok(()));
        let two = env(&[("FLICK_ARG", "a & b"), ("FLICK_NAME", "n")]);
        assert_eq!(l.run_command(&Shell::Windows, r#"prog "!FLICK_ARG!""#, &two, false, false, Some(r"C:\p\a b")), Ok(()));
        rec.fail_next("file not found");
        assert_eq!(l.reveal(r"C:\x\a.txt"), Err("file not found".to_string()));
        assert_eq!(l.open("again"), Ok(()));
        assert_eq!(
            rec.calls(),
            vec![
                r"open C:\x\a.txt",
                "open_with rundll32.exe elevated=false: sysdm.cpl,EditEnvironmentVariables",
                "run_command Wsl(Debian) env=[] keep=true elevated=false cwd=-: ls -la",
                "run_command Windows env=[] keep=false elevated=true cwd=-: dir",
                r#"run_command Windows env=[FLICK_ARG=a & b, FLICK_NAME=n] keep=false elevated=false cwd=C:\p\a b: prog "!FLICK_ARG!""#,
                r"reveal C:\x\a.txt",
                "open again",
            ]
        );
    }

    #[test]
    fn open_with_missing_file_is_err() {
        // Target does not exist, so nothing is launched and no dialog shows.
        assert!(open_with(r"C:\definitely\missing.exe", "--flag", false).is_err());
        assert!(SystemLauncher.open_with(r"C:\definitely\missing.exe", "", false).is_err());
    }
}
