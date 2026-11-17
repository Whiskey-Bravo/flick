//! Deciding how an app is uninstalled: the uninstaller Windows has registered
//! for it, or the Installed apps settings page. The decisions (`plan`,
//! `split_command`) are pure; the registry walk is thin and runs on the apps
//! refresh thread only.

use std::path::{Component, Path, PathBuf};

/// An entry of Windows' uninstall list.
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub name: String,
    pub install_location: Option<std::path::PathBuf>,
    pub uninstall: Option<String>,
    /// Marked `SystemComponent`: hidden from Installed apps, never uninstalled here.
    pub system: bool,
}

pub trait ProgramSource: Send + Sync {
    fn programs(&self) -> Vec<Program>;
}

#[derive(Clone, Debug, PartialEq)]
pub enum Uninstall {
    /// Run this registered command (split with [`split_command`]).
    Direct { command: String },
    /// Open the Installed apps page.
    Settings,
}

pub const APPS_SETTINGS: &str = "ms-settings:appsfeatures";

/// Path components, lowercased, for a case-insensitive prefix test.
fn parts(p: &Path) -> Vec<String> {
    p.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

/// Whether `path` lies inside `dir` (component prefix, case-insensitive).
/// An empty `dir` contains nothing.
fn inside(path: &Path, dir: &Path) -> bool {
    let dir = parts(dir);
    !dir.is_empty() && parts(path).starts_with(&dir)
}

/// Store ids (`<family>!<app>`) of Windows parts.
fn is_windows_package(store_id: &str) -> bool {
    let id = store_id.to_lowercase();
    id.starts_with("windows.") || id.starts_with("microsoft.windows.")
}

/// Vendor and filler words: sharing one says nothing about being the same app.
const FILLER: [&str; 10] = ["microsoft", "google", "adobe", "apple", "mozilla", "app", "apps", "the", "for", "and"];

/// Words of a name: letters and digits, lowercased, at least 3 characters,
/// without vendor and filler words.
fn words(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .filter(|w| !FILLER.contains(&w.as_str()))
        .collect()
}

/// Whether the exe is a browser's web-app launcher (`chrome_proxy.exe`).
fn is_proxy(exe: &Path) -> bool {
    exe.file_stem().is_some_and(|s| s.to_string_lossy().to_lowercase().ends_with("_proxy"))
}

/// Folders too shared to belong to one program: drive roots, anything under
/// the Windows folder, and the given `broad` roots themselves.
fn is_broad(loc: &Path, windows_dir: &Path, broad: &[PathBuf]) -> bool {
    let depth = parts(loc).len();
    depth <= 2 || inside(loc, windows_dir) || broad.iter().any(|b| parts(b) == parts(loc))
}

/// The shared folders (`Program Files`, `Program Files (x86)`, `%LOCALAPPDATA%\Programs`,
/// the profile folder, `AppData` and its `Local` / `Roaming`, `ProgramData`)
/// whose own location never identifies one program. `env` reads a variable.
pub fn broad_roots_from(env: impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "ProgramData", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        if let Some(v) = env(var).filter(|v| !v.is_empty()) {
            out.push(PathBuf::from(v));
        }
    }
    if let Some(users) = env("USERPROFILE").and_then(|p| Path::new(&p).parent().map(Path::to_path_buf)) {
        out.push(users);
    }
    if let Some(local) = env("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        let local = PathBuf::from(local);
        out.push(local.join("Programs"));
        if let Some(app_data) = local.parent() {
            out.push(app_data.to_path_buf());
        }
    }
    out
}

/// [`broad_roots_from`] with the real environment.
#[cfg_attr(test, allow(dead_code))]
pub fn broad_roots() -> Vec<PathBuf> {
    broad_roots_from(|v| std::env::var(v).ok())
}

/// How to uninstall an app, or `None` when it must not be offered at all:
/// Flick itself (`own_dir`), anything in the Windows folder, Windows' own
/// Store packages. A Store app opens Installed apps. A desktop app is
/// uninstalled directly only when its exe lies inside the install location
/// of one registered program (the longest such location) that is not a
/// system component, has an uninstall command and does not live under the
/// Windows folder, whose location is not a shared folder (`broad`, drive
/// roots) and whose display name shares a word with the app's `name`;
/// anything else opens Installed apps, as does a shortcut that passes
/// arguments or whose exe is a `_proxy` launcher (the program is then a host,
/// not the app).
pub fn plan(
    name: &str,
    has_args: bool,
    exe: Option<&Path>,
    store_id: Option<&str>,
    programs: &[Program],
    windows_dir: &Path,
    own_dir: &Path,
    broad: &[PathBuf],
) -> Option<Uninstall> {
    if exe.is_some_and(|e| inside(e, own_dir) || inside(e, windows_dir)) {
        return None;
    }
    if let Some(id) = store_id {
        return (!is_windows_package(id)).then_some(Uninstall::Settings);
    }
    let Some(exe) = exe else { return Some(Uninstall::Settings) };
    if has_args || is_proxy(exe) {
        return Some(Uninstall::Settings);
    }
    let owner = programs
        .iter()
        .filter_map(|p| {
            let loc = p.install_location.as_deref()?;
            let loc = Path::new(loc.to_str()?.trim().trim_matches('"'));
            inside(exe, loc).then_some((parts(loc).len(), p, loc))
        })
        .max_by_key(|(depth, ..)| *depth);
    let direct = owner.and_then(|(_, p, loc)| {
        let command = p.uninstall.as_deref()?.trim();
        let wanted = words(name);
        let same_app = words(&p.name).iter().any(|w| wanted.contains(w));
        (!p.system && !command.is_empty() && !is_broad(loc, windows_dir, broad) && same_app).then(|| command.to_string())
    });
    Some(match direct {
        Some(command) => Uninstall::Direct { command },
        None => Uninstall::Settings,
    })
}

/// Split a registered uninstall command into program and parameters, to be
/// run by the launcher and never through a shell. A quoted program ends at
/// the closing quote; an unquoted one at the first `.exe` followed by a space
/// or the end, else at the first whitespace.
pub fn split_command(cmd: &str) -> Option<(String, String)> {
    let cmd = cmd.trim();
    let (program, rest) = if let Some(quoted) = cmd.strip_prefix('"') {
        match quoted.split_once('"') {
            Some((program, rest)) => (program, rest),
            None => (quoted, ""),
        }
    } else {
        let lower = cmd.to_ascii_lowercase();
        let end = lower
            .match_indices(".exe")
            .map(|(i, m)| i + m.len())
            .find(|&end| lower[end..].is_empty() || lower[end..].starts_with(' '))
            .or_else(|| cmd.find(char::is_whitespace))
            .unwrap_or(cmd.len());
        cmd.split_at(end)
    };
    let program = program.trim();
    (!program.is_empty()).then(|| (program.to_string(), rest.trim().to_string()))
}

/// The real uninstall list. Reads the registry: never use in tests, never on
/// the query path.
#[cfg_attr(test, allow(dead_code))]
pub struct RegistryPrograms;

#[cfg_attr(test, allow(dead_code))]
mod sys {
    use super::Program;
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
        KEY_READ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };

    const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
    const UNINSTALL_32: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn string_value(key: HKEY, name: &str) -> Option<String> {
        let name = wide(name);
        let mut bytes = 0u32;
        // Plain strings only: asking for REG_EXPAND_SZ without RRF_NOEXPAND fails the call.
        let flags = RRF_RT_REG_SZ;
        // SAFETY: the size query passes no buffer; the read passes the buffer it sized.
        unsafe {
            RegGetValueW(key, PCWSTR::null(), PCWSTR(name.as_ptr()), flags, None, None, Some(&mut bytes)).ok().ok()?;
            let mut buf = vec![0u16; (bytes as usize).div_ceil(2) + 1];
            let mut bytes = (buf.len() * 2) as u32;
            RegGetValueW(
                key,
                PCWSTR::null(),
                PCWSTR(name.as_ptr()),
                flags,
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

    fn dword_value(key: HKEY, name: &str) -> Option<u32> {
        let name = wide(name);
        let mut value = 0u32;
        let mut bytes = 4u32;
        // SAFETY: the out buffer is a u32 and `bytes` says so.
        unsafe {
            RegGetValueW(
                key,
                PCWSTR::null(),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut value as *mut u32 as *mut std::ffi::c_void),
                Some(&mut bytes),
            )
            .ok()
            .ok()?;
        }
        Some(value)
    }

    fn read_root(root: HKEY, path: &str, out: &mut Vec<Program>) {
        let path = wide(path);
        let mut parent = HKEY::default();
        // SAFETY: every key opened here is closed before leaving; buffers are sized.
        unsafe {
            if RegOpenKeyExW(root, PCWSTR(path.as_ptr()), None, KEY_READ, &mut parent).is_err() {
                return;
            }
            let mut index = 0u32;
            loop {
                let mut name = [0u16; 256];
                let mut len = name.len() as u32;
                let status =
                    RegEnumKeyExW(parent, index, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None);
                if status.is_err() {
                    break;
                }
                index += 1;
                let mut child = HKEY::default();
                if RegOpenKeyExW(parent, PCWSTR(name.as_ptr()), None, KEY_READ, &mut child).is_err() {
                    continue;
                }
                if let Some(display) = string_value(child, "DisplayName") {
                    out.push(Program {
                        name: display,
                        install_location: string_value(child, "InstallLocation").map(Into::into),
                        uninstall: string_value(child, "UninstallString"),
                        system: dword_value(child, "SystemComponent") == Some(1),
                    });
                }
                let _ = RegCloseKey(child);
            }
            let _ = RegCloseKey(parent);
        }
    }

    pub fn programs() -> Vec<Program> {
        let mut out = Vec::new();
        read_root(HKEY_LOCAL_MACHINE, UNINSTALL, &mut out);
        read_root(HKEY_CURRENT_USER, UNINSTALL, &mut out);
        read_root(HKEY_LOCAL_MACHINE, UNINSTALL_32, &mut out);
        out
    }
}

impl ProgramSource for RegistryPrograms {
    fn programs(&self) -> Vec<Program> {
        sys::programs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn program(name: &str, location: Option<&str>, uninstall: Option<&str>, system: bool) -> Program {
        Program {
            name: name.into(),
            install_location: location.map(PathBuf::from),
            uninstall: uninstall.map(String::from),
            system,
        }
    }

    fn broad() -> Vec<PathBuf> {
        broad_roots_from(|v| {
            match v {
                "ProgramFiles" => Some(r"C:\Program Files"),
                "ProgramFiles(x86)" => Some(r"C:\Program Files (x86)"),
                "ProgramData" => Some(r"C:\ProgramData"),
                "USERPROFILE" => Some(r"C:\Users\someone"),
                "APPDATA" => Some(r"C:\Users\someone\AppData\Roaming"),
                "LOCALAPPDATA" => Some(r"C:\Users\someone\AppData\Local"),
                _ => None,
            }
            .map(String::from)
        })
    }

    fn plan_named(name: &str, exe: &str, programs: &[Program]) -> Option<Uninstall> {
        plan(name, false, Some(Path::new(exe)), None, programs, Path::new(r"C:\Windows"), Path::new(r"C:\Apps\Flick"), &broad())
    }

    fn plan_for(exe: &str, programs: &[Program]) -> Option<Uninstall> {
        plan_named("Foo Suite Tool", exe, programs)
    }

    fn direct(command: &str) -> Option<Uninstall> {
        Some(Uninstall::Direct { command: command.into() })
    }

    #[test]
    fn direct_only_when_the_exe_is_inside_an_install_location() {
        let foo = program("Foo", Some(r"C:\Program Files\Foo"), Some(r"C:\Program Files\Foo\unins000.exe"), false);
        assert_eq!(plan_for(r"C:\Program Files\Foo\foo.exe", &[foo.clone()]), direct(r"C:\Program Files\Foo\unins000.exe"));
        assert_eq!(plan_for(r"c:\program files\foo\bin\foo.exe", &[foo.clone()]), direct(r"C:\Program Files\Foo\unins000.exe"));
        // A sibling that merely shares a name prefix is not inside.
        assert_eq!(plan_for(r"C:\Program Files\Foobar\foobar.exe", &[foo]), Some(Uninstall::Settings));
    }

    #[test]
    fn the_program_name_must_share_a_word_with_the_app() {
        let foo = program("Foo 2.1", Some(r"C:\Program Files\Foo"), Some("foo-uninstall"), false);
        assert_eq!(plan_named("Foo", r"C:\Program Files\Foo\foo.exe", &[foo.clone()]), direct("foo-uninstall"));
        assert_eq!(plan_named("Bar", r"C:\Program Files\Foo\bar.exe", &[foo]), Some(Uninstall::Settings));
        // A web-app shortcut hosted by the browser must not uninstall the browser.
        let chrome = program(
            "Google Chrome",
            Some(r"C:\Program Files\Google\Chrome\Application"),
            Some("chrome-uninstall"),
            false,
        );
        let proxy = r"C:\Program Files\Google\Chrome\Application\chrome_proxy.exe";
        assert_eq!(plan_named("YouTube", proxy, &[chrome.clone()]), Some(Uninstall::Settings));
        // Even under its own name: the exe is a launcher for web apps.
        assert_eq!(plan_named("Google Chrome", proxy, &[chrome]), Some(Uninstall::Settings));
    }

    #[test]
    fn hosts_arguments_and_vendor_words_never_make_a_match() {
        let chrome = program(
            "Google Chrome",
            Some(r"C:\Program Files\Google\Chrome\Application"),
            Some("chrome-uninstall"),
            false,
        );
        let proxy = Path::new(r"C:\Program Files\Google\Chrome\Application\chrome_proxy.exe");
        let args = |name, has_args, exe| {
            plan(name, has_args, Some(exe), None, &[chrome.clone()], Path::new(r"C:\Windows"), Path::new(r"C:\Apps\Flick"), &broad())
        };
        for has_args in [false, true] {
            assert_eq!(args("Google Docs", has_args, proxy), Some(Uninstall::Settings));
        }
        let foo = program("Foo", Some(r"C:\Program Files\Foo"), Some("foo-uninstall"), false);
        let foo_exe = Path::new(r"C:\Program Files\Foo\foo.exe");
        let foo_plan = |has_args| {
            plan("Foo", has_args, Some(foo_exe), None, &[foo.clone()], Path::new(r"C:\Windows"), Path::new(r"C:\Apps\Flick"), &broad())
        };
        assert_eq!(foo_plan(false), direct("foo-uninstall"));
        assert_eq!(foo_plan(true), Some(Uninstall::Settings));
        let office = program("Microsoft Office", Some(r"C:\Program Files\Microsoft Office"), Some("office-uninstall"), false);
        assert_eq!(
            plan_named("Microsoft Teams", r"C:\Program Files\Microsoft Office\root\teams.exe", &[office]),
            Some(Uninstall::Settings)
        );
        assert!(broad().contains(&PathBuf::from(r"C:\Users")));
    }

    #[test]
    fn broad_install_locations_never_uninstall_directly() {
        for loc in [
            r"C:\Program Files",
            r"C:\Program Files (x86)",
            r"C:\Users\someone\AppData\Local\Programs",
            r"C:\Users\someone",
            r"C:\Users\someone\AppData",
            r"C:\Users\someone\AppData\Local",
            r"C:\Users\someone\AppData\Roaming",
            r"C:\ProgramData",
            r"C:\",
            r"C:\Windows\Foo",
        ] {
            let p = program("Foo", Some(loc), Some("foo-uninstall"), false);
            let exe = format!(r"{}\Foo\foo.exe", loc.trim_end_matches('\\'));
            let got = plan_named("Foo", &exe, &[p]);
            assert!(matches!(got, Some(Uninstall::Settings) | None), "{loc}: {got:?}");
        }
        // One level below a shared folder is a program's own folder.
        let p = program("Foo", Some(r"C:\Users\someone\AppData\Local\Programs\Foo"), Some("foo-uninstall"), false);
        assert_eq!(plan_named("Foo", r"C:\Users\someone\AppData\Local\Programs\Foo\foo.exe", &[p]), direct("foo-uninstall"));
    }

    #[test]
    fn longest_install_location_wins() {
        let suite = program("Suite", Some(r"C:\Program Files\Suite"), Some("suite-uninstall"), false);
        let tool = program("Suite Tool", Some(r"C:\Program Files\Suite\Tool"), Some("tool-uninstall"), false);
        assert_eq!(plan_for(r"C:\Program Files\Suite\Tool\tool.exe", &[suite.clone(), tool.clone()]), direct("tool-uninstall"));
        assert_eq!(plan_for(r"C:\Program Files\Suite\Tool\tool.exe", &[tool, suite.clone()]), direct("tool-uninstall"));
        assert_eq!(plan_for(r"C:\Program Files\Suite\suite.exe", &[suite]), direct("suite-uninstall"));
    }

    #[test]
    fn name_only_or_no_location_opens_settings() {
        let no_location = program("Foo", None, Some("foo-uninstall"), false);
        let blank = program("Foo", Some(""), Some("foo-uninstall"), false);
        let no_command = program("Foo", Some(r"C:\Program Files\Foo"), None, false);
        for p in [no_location, blank, no_command] {
            assert_eq!(plan_for(r"C:\Program Files\Foo\foo.exe", &[p]), Some(Uninstall::Settings));
        }
        assert_eq!(plan("Foo", false, None, None, &[], Path::new(r"C:\Windows"), Path::new(r"C:\Apps\Flick"), &broad()), Some(Uninstall::Settings));
    }

    #[test]
    fn system_components_and_the_windows_folder_never_uninstall_directly() {
        let system = program("Foo", Some(r"C:\Program Files\Foo"), Some("foo-uninstall"), true);
        assert_eq!(plan_for(r"C:\Program Files\Foo\foo.exe", &[system]), Some(Uninstall::Settings));
        assert_eq!(plan_for(r"C:\Windows\System32\cmd.exe", &[]), None);
        // A location outside the Windows folder is fine; an exe registered
        // under it never gets an offer at all.
        let in_windows = program("Tool", Some(r"C:\Windows\Tool"), Some("tool-uninstall"), false);
        assert_eq!(plan_for(r"C:\Windows\Tool\tool.exe", &[in_windows]), None);
    }

    #[test]
    fn flick_itself_is_never_offered() {
        let own = program("Flick", Some(r"C:\Apps\Flick"), Some("flick-uninstall"), false);
        assert_eq!(plan_for(r"C:\Apps\Flick\flick.exe", &[own]), None);
    }

    #[test]
    fn store_apps_open_settings_except_windows_parts() {
        let plan_of = |id| plan("App", false, None, Some(id), &[], Path::new(r"C:\Windows"), Path::new(r"C:\Apps\Flick"), &broad());
        assert_eq!(plan_of("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"), Some(Uninstall::Settings));
        assert_eq!(plan_of("windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel"), None);
        assert_eq!(plan_of("Microsoft.Windows.Photos_8wekyb3d8bbwe!App"), None);
        assert_eq!(plan_of("MICROSOFT.WINDOWS.Cortana_x!App"), None);
    }

    #[test]
    fn split_command_handles_quotes_and_msiexec() {
        let split = |c| split_command(c);
        assert_eq!(
            split("\"C:\\Program Files\\Foo\\unins000.exe\" /SILENT"),
            Some((r"C:\Program Files\Foo\unins000.exe".to_string(), "/SILENT".to_string()))
        );
        assert_eq!(split("MsiExec.exe /X{0000}"), Some(("MsiExec.exe".to_string(), "/X{0000}".to_string())));
        assert_eq!(
            split(r"C:\Program Files\Foo\uninst.exe"),
            Some((r"C:\Program Files\Foo\uninst.exe".to_string(), String::new()))
        );
        assert_eq!(
            split(r"C:\Program Files\Foo\uninst.exe /S /D=C:\x"),
            Some((r"C:\Program Files\Foo\uninst.exe".to_string(), r"/S /D=C:\x".to_string()))
        );
        assert_eq!(split("rundll32 setup.dll,Remove"), Some(("rundll32".to_string(), "setup.dll,Remove".to_string())));
        assert_eq!(split(""), None);
        assert_eq!(split("   "), None);
        assert_eq!(split("\"\" /x"), None);
    }

    /// Reads the real registry: run by hand, never in the normal suite.
    /// `cargo test real_registry -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_registry_lists_programs() {
        let programs = RegistryPrograms.programs();
        println!("{} programs, {} with an uninstall command", programs.len(), programs.iter().filter(|p| p.uninstall.is_some()).count());
        assert!(!programs.is_empty());
    }
}
