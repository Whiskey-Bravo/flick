//! What a file or folder row offers in its actions menu: the entries per kind
//! of row, and the system calls behind the ones that change or show something
//! ([`FileOps`]). The pure helpers here never touch the disk.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, POINT};
use windows::Win32::Storage::FileSystem::{
    GetFileAttributesW, SetFileAttributesW, FILE_FLAGS_AND_ATTRIBUTES, INVALID_FILE_ATTRIBUTES,
};
use windows::Win32::System::DataExchange::{EmptyClipboard, RegisterClipboardFormatW, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT};
use windows::Win32::System::Ole::{CF_HDROP, DROPEFFECT_COPY};
use windows::Win32::UI::Shell::{SHObjectProperties, SHOpenWithDialog, DROPFILES, OAIF_EXEC, OPENASINFO, SHOP_FILEPATH};

use crate::provider::Action;

/// What the menu entries do to the disk and the shell. Real calls in
/// [`SystemFileOps`], a recording fake in tests.
pub trait FileOps: Send + Sync {
    /// A terminal in `folder`.
    fn open_terminal(&self, folder: &Path) -> Result<(), String>;
    /// The item's Properties window.
    fn properties(&self, path: &Path) -> Result<(), String>;
    /// The "Open with" chooser for `file`.
    fn open_with(&self, file: &Path) -> Result<(), String>;
    /// Puts the file itself on the clipboard (as a copy, for Explorer to paste).
    fn copy_file(&self, path: &Path) -> Result<(), String>;
    /// Zips the item next to itself; the zip's path.
    fn compress(&self, path: &Path) -> Result<PathBuf, String>;
    /// Cloud files: always keep on this device (`keep`) or online only.
    fn set_cloud_pin(&self, path: &Path, keep: bool) -> Result<(), String>;
    /// To the Recycle Bin. Not called until Delete is built.
    #[allow(dead_code)]
    fn recycle(&self, path: &Path) -> Result<(), String>;
    /// Renames within the same folder; the new path. Not called until Rename is built.
    #[allow(dead_code)]
    fn rename(&self, path: &Path, new_name: &str) -> Result<PathBuf, String>;
}

/// The actions menu of a file or folder row, in order. `cloud`: the item is
/// under a cloud sync root (adds the keep/free pair); `drive_root`: `C:\`,
/// which cannot be compressed, renamed, deleted or kept offline.
pub(crate) fn menu(is_dir: bool, cloud: bool, drive_root: bool) -> Vec<Action> {
    let mut actions = if is_dir {
        vec![Action::OpenFolder, Action::OpenTerminal, Action::CopyPath, Action::Compress, Action::Properties]
    } else {
        vec![Action::Reveal, Action::OpenWith, Action::CopyFile, Action::CopyPath, Action::Rename, Action::Delete]
    };
    if cloud {
        actions.extend([Action::KeepOffline, Action::FreeUpSpace]);
    }
    actions.push(Action::MoreOptions);
    if drive_root {
        actions.retain(|a| !matches!(a, Action::Compress | Action::Rename | Action::Delete | Action::KeepOffline | Action::FreeUpSpace));
    }
    actions
}

/// Whether `path` is a drive root: `C:\` or `C:`, any letter case.
pub(crate) fn is_drive_root(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || (b.len() == 3 && b[2] == b'\\'))
}


/// Where the zip of `path` goes: `<name>.zip` next to it, else `<name> (2).zip`,
/// `(3)` and so on, the first one `exists` does not know.
pub(crate) fn zip_target(path: &Path, exists: &dyn Fn(&Path) -> bool) -> PathBuf {
    let parent = path.parent().unwrap_or(Path::new(""));
    let stem = path.file_stem().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
    let first = parent.join(format!("{stem}.zip"));
    if !exists(&first) {
        return first;
    }
    (2..)
        .map(|n| parent.join(format!("{stem} ({n}).zip")))
        .find(|candidate| !exists(candidate))
        .expect("an unbounded range ends at a free name")
}

/// The arguments that make `tar` write `zip` from `name` inside `parent`. The
/// `./` keeps a name starting with `-` from reading as an option; every value
/// is one argument, never text for a shell.
pub(crate) fn tar_args(parent: &Path, name: &str, zip: &Path) -> Vec<OsString> {
    vec![
        "-a".into(),
        "-c".into(),
        "-f".into(),
        zip.as_os_str().to_owned(),
        "-C".into(),
        parent.as_os_str().to_owned(),
        format!("./{name}").into(),
    ]
}

/// The program and arguments for a terminal in the current directory:
/// Windows Terminal when `wt` is there, else a console window of cmd.exe.
pub(crate) fn terminal_command(wt: Option<&Path>) -> (PathBuf, Vec<OsString>) {
    match wt {
        Some(wt) => (wt.to_path_buf(), vec!["-d".into(), ".".into()]),
        None => (PathBuf::from(crate::quote::system32("cmd.exe")), Vec::new()),
    }
}

/// Windows Terminal's launcher for the current user, when it is installed.
fn windows_terminal() -> Option<PathBuf> {
    let wt = Path::new(&std::env::var_os("LOCALAPPDATA")?).join(r"Microsoft\WindowsApps\wt.exe");
    wt.exists().then_some(wt)
}

const FILE_ATTRIBUTE_PINNED: u32 = 0x0008_0000;
const FILE_ATTRIBUTE_UNPINNED: u32 = 0x0010_0000;

/// `current` with the cloud pin flags set for keeping (`keep`) or for online only.
pub(crate) fn pin_attributes(current: u32, keep: bool) -> u32 {
    let (set, clear) =
        if keep { (FILE_ATTRIBUTE_PINNED, FILE_ATTRIBUTE_UNPINNED) } else { (FILE_ATTRIBUTE_UNPINNED, FILE_ATTRIBUTE_PINNED) };
    (current | set) & !clear
}

fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// An I/O error as one short sentence for the bar.
fn short(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// Sets the pin flags on `path` and, for a folder, on everything in it.
/// Entries that fail are skipped: how many worked, and the first failure.
fn pin_tree(path: &Path, keep: bool) -> (usize, Option<String>) {
    let (mut done, mut first_error) = (0, None);
    let mut stack = vec![path.to_path_buf()];
    while let Some(item) = stack.pop() {
        match pin_one(&item, keep) {
            Ok(()) => done += 1,
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
        if std::fs::symlink_metadata(&item).is_ok_and(|m| m.is_dir()) {
            if let Ok(children) = std::fs::read_dir(&item) {
                stack.extend(children.flatten().map(|c| c.path()));
            }
        }
    }
    (done, first_error)
}

fn pin_one(path: &Path, keep: bool) -> Result<(), String> {
    let name = wide(path);
    // SAFETY: `name` is NUL-terminated and outlives both calls.
    unsafe {
        let current = GetFileAttributesW(PCWSTR(name.as_ptr()));
        if current == INVALID_FILE_ATTRIBUTES {
            return Err(short(&std::io::Error::last_os_error()));
        }
        SetFileAttributesW(PCWSTR(name.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(pin_attributes(current, keep)))
            .map_err(|_| short(&std::io::Error::last_os_error()))
    }
}

/// The real calls.
pub struct SystemFileOps;

impl FileOps for SystemFileOps {
    fn open_terminal(&self, folder: &Path) -> Result<(), String> {
        let (program, args) = terminal_command(windows_terminal().as_deref());
        let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        crate::launch::start_in(&program.to_string_lossy(), &args, &folder.to_string_lossy())
    }

    fn properties(&self, path: &Path) -> Result<(), String> {
        let name = wide(path);
        // SAFETY: `name` is NUL-terminated and outlives the call; no owner window.
        let shown = crate::launch::with_com(|| unsafe {
            SHObjectProperties(None, SHOP_FILEPATH, PCWSTR(name.as_ptr()), PCWSTR::null())
        });
        if shown.as_bool() {
            Ok(())
        } else {
            Err("could not show properties".into())
        }
    }

    /// The chooser is modal: it runs on its own thread so the bar can hide.
    fn open_with(&self, file: &Path) -> Result<(), String> {
        let name = wide(file);
        std::thread::Builder::new()
            .name("open-with".into())
            .spawn(move || {
                let info = OPENASINFO { pcszFile: PCWSTR(name.as_ptr()), pcszClass: PCWSTR::null(), oaifInFlags: OAIF_EXEC };
                // SAFETY: `info` and the NUL-terminated name live through the call.
                // A dismissed chooser comes back as an error and is not one to show.
                let _ = crate::launch::with_com(|| unsafe { SHOpenWithDialog(None, &info) });
            })
            .map(|_| ())
            .map_err(|_| "could not open the chooser".to_string())
    }

    fn copy_file(&self, path: &Path) -> Result<(), String> {
        const FAILED: &str = "could not copy to the clipboard";
        let names = wide(path);
        let header = std::mem::size_of::<DROPFILES>();
        // DROPFILES, then the NUL-terminated name, then the list's own NUL.
        let bytes = header + (names.len() + 1) * 2;
        let _open = crate::clipboard::open()?;
        // SAFETY: the clipboard is open on this thread; each block is allocated
        // at the size written into it and is handed to the clipboard only when
        // SetClipboardData succeeds, else freed here.
        unsafe {
            EmptyClipboard().map_err(|_| FAILED.to_string())?;
            let files = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes).map_err(|_| FAILED.to_string())?;
            let base = GlobalLock(files) as *mut u8;
            if base.is_null() {
                let _ = GlobalFree(Some(files));
                return Err(FAILED.into());
            }
            base.cast::<DROPFILES>().write_unaligned(DROPFILES {
                pFiles: header as u32,
                pt: POINT::default(),
                fNC: false.into(),
                fWide: true.into(),
            });
            std::ptr::copy_nonoverlapping(names.as_ptr().cast::<u8>(), base.add(header), names.len() * 2);
            let _ = GlobalUnlock(files);
            if SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(files.0))).is_err() {
                let _ = GlobalFree(Some(files));
                return Err(FAILED.into());
            }
            // Explorer pastes a copy, not a move. Without the hint the file is
            // still on the clipboard, so a failure here is not worth an error.
            let format = RegisterClipboardFormatW(windows::core::w!("Preferred DropEffect"));
            let effect: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, 4).unwrap_or_default();
            let at = GlobalLock(effect) as *mut u32;
            if format != 0 && !at.is_null() {
                at.write_unaligned(DROPEFFECT_COPY.0);
                let _ = GlobalUnlock(effect);
                if SetClipboardData(format, Some(HANDLE(effect.0))).is_ok() {
                    return Ok(());
                }
            }
            if !at.is_null() {
                let _ = GlobalUnlock(effect);
            }
            if !effect.is_invalid() {
                let _ = GlobalFree(Some(effect));
            }
        }
        Ok(())
    }

    fn compress(&self, path: &Path) -> Result<PathBuf, String> {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { return Err("can't compress this".into()) };
        let zip = zip_target(path, &|p| p.exists());
        let output = Command::new(crate::quote::system32("tar.exe"))
            .args(tar_args(parent, &name.to_string_lossy(), &zip))
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| short(&e))?;
        if output.status.success() {
            return Ok(zip);
        }
        let _ = std::fs::remove_file(&zip);
        let said = String::from_utf8_lossy(&output.stderr);
        Err(said
            .lines()
            .next()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| "could not compress".into()))
    }

    fn set_cloud_pin(&self, path: &Path, keep: bool) -> Result<(), String> {
        match pin_tree(path, keep) {
            (0, Some(e)) => Err(e),
            _ => Ok(()),
        }
    }

    fn recycle(&self, _path: &Path) -> Result<(), String> {
        Err("not yet".into())
    }

    fn rename(&self, _path: &Path, _new_name: &str) -> Result<PathBuf, String> {
        Err("not yet".into())
    }
}

/// Test file ops: logs each call as `<method> <path>` (`set_cloud_pin` adds
/// the flag) and answers `Ok` unless the method was told to fail.
#[cfg(test)]
#[derive(Default)]
pub struct FakeFileOps {
    calls: std::sync::Mutex<Vec<String>>,
    failing: std::sync::Mutex<std::collections::HashMap<&'static str, String>>,
}

#[cfg(test)]
impl FakeFileOps {
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Makes every later call of `method` answer `Err(error)`.
    pub fn fail(&self, method: &'static str, error: &str) {
        self.failing.lock().unwrap().insert(method, error.to_string());
    }

    fn record(&self, method: &'static str, path: &Path, extra: &str) -> Result<(), String> {
        let line = if extra.is_empty() { format!("{method} {}", path.display()) } else { format!("{method} {} {extra}", path.display()) };
        self.calls.lock().unwrap().push(line);
        match self.failing.lock().unwrap().get(method) {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
impl FileOps for FakeFileOps {
    fn open_terminal(&self, folder: &Path) -> Result<(), String> {
        self.record("open_terminal", folder, "")
    }

    fn properties(&self, path: &Path) -> Result<(), String> {
        self.record("properties", path, "")
    }

    fn open_with(&self, file: &Path) -> Result<(), String> {
        self.record("open_with", file, "")
    }

    fn copy_file(&self, path: &Path) -> Result<(), String> {
        self.record("copy_file", path, "")
    }

    fn compress(&self, path: &Path) -> Result<PathBuf, String> {
        self.record("compress", path, "").map(|()| path.with_extension("zip"))
    }

    fn set_cloud_pin(&self, path: &Path, keep: bool) -> Result<(), String> {
        self.record("set_cloud_pin", path, &keep.to_string())
    }

    fn recycle(&self, path: &Path) -> Result<(), String> {
        self.record("recycle", path, "")
    }

    fn rename(&self, path: &Path, new_name: &str) -> Result<PathBuf, String> {
        self.record("rename", path, new_name).map(|()| path.with_file_name(new_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action::*;

    #[test]
    fn menu_orders_for_file_and_folder() {
        assert_eq!(menu(true, true, false), [OpenFolder, OpenTerminal, CopyPath, Compress, Properties, KeepOffline, FreeUpSpace, MoreOptions]);
        assert_eq!(menu(false, true, false), [Reveal, OpenWith, CopyFile, CopyPath, Rename, Delete, KeepOffline, FreeUpSpace, MoreOptions]);
    }

    #[test]
    fn menu_without_cloud_drops_the_pair() {
        assert_eq!(menu(true, false, false), [OpenFolder, OpenTerminal, CopyPath, Compress, Properties, MoreOptions]);
        assert_eq!(menu(false, false, false), [Reveal, OpenWith, CopyFile, CopyPath, Rename, Delete, MoreOptions]);
    }

    #[test]
    fn menu_on_a_drive_root_drops_changes() {
        assert_eq!(menu(true, false, true), [OpenFolder, OpenTerminal, CopyPath, Properties, MoreOptions]);
        assert_eq!(menu(true, true, true), [OpenFolder, OpenTerminal, CopyPath, Properties, MoreOptions]);
    }

    #[test]
    fn drive_root_rules() {
        assert!(is_drive_root(r"C:\"));
        assert!(is_drive_root("c:"));
        assert!(!is_drive_root(r"C:\x\"));
        assert!(!is_drive_root(r"\\srv\share\"));
        assert!(!is_drive_root(""));
    }

    #[test]
    fn zip_target_numbers_taken_names() {
        let taken = |names: &'static [&'static str]| move |p: &Path| names.contains(&p.to_string_lossy().as_ref());
        assert_eq!(zip_target(Path::new(r"C:\d\a.txt"), &taken(&[])), PathBuf::from(r"C:\d\a.zip"));
        assert_eq!(zip_target(Path::new(r"C:\d\Photos"), &taken(&[])), PathBuf::from(r"C:\d\Photos.zip"));
        assert_eq!(zip_target(Path::new(r"C:\d\a.txt"), &taken(&[r"C:\d\a.zip"])), PathBuf::from(r"C:\d\a (2).zip"));
        assert_eq!(
            zip_target(Path::new(r"C:\d\a.txt"), &taken(&[r"C:\d\a.zip", r"C:\d\a (2).zip"])),
            PathBuf::from(r"C:\d\a (3).zip")
        );
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn tar_args_guard_a_leading_dash() {
        let args = tar_args(Path::new(r"C:\d"), "-x", Path::new(r"C:\d\-x.zip"));
        assert_eq!(strings(&args), ["-a", "-c", "-f", r"C:\d\-x.zip", "-C", r"C:\d", "./-x"]);
    }

    #[test]
    fn tar_args_keep_unicode_and_spaces() {
        let args = tar_args(Path::new(r"C:\Ü ser"), "Ä b&c", Path::new(r"C:\Ü ser\Ä b&c.zip"));
        assert_eq!(args.len(), 7);
        assert_eq!(args[6], OsString::from("./Ä b&c"));
        assert_eq!(args[5], OsString::from(r"C:\Ü ser"));
        assert_eq!(args[3], OsString::from(r"C:\Ü ser\Ä b&c.zip"));
    }

    #[test]
    fn terminal_prefers_windows_terminal() {
        let wt = PathBuf::from(r"C:\Users\u\AppData\Local\Microsoft\WindowsApps\wt.exe");
        let (program, args) = terminal_command(Some(&wt));
        assert_eq!((program, strings(&args)), (wt, vec!["-d".to_string(), ".".to_string()]));
        let (program, args) = terminal_command(None);
        assert!(program.to_string_lossy().to_lowercase().ends_with(r"system32\cmd.exe"), "{program:?}");
        assert!(args.is_empty());
    }

    #[test]
    fn pin_attributes_swap_the_two_flags() {
        let (pinned, unpinned) = (0x80000, 0x100000);
        assert_eq!(pin_attributes(0x20 | unpinned, true), 0x20 | pinned);
        assert_eq!(pin_attributes(0x20 | pinned, false), 0x20 | unpinned);
        assert_eq!(pin_attributes(0x20, true), 0x20 | pinned);
    }

    /// Real copy to the clipboard and real zip, in a fresh temp folder that is
    /// deleted at the end. Run by hand: `cargo test -- --ignored real_copy_file_and_zip_probe`.
    #[test]
    #[ignore = "uses the real clipboard and tar.exe"]
    fn real_copy_file_and_zip_probe() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Ä b&c.txt");
        std::fs::write(&file, b"hello").unwrap();
        let ops = SystemFileOps;
        ops.copy_file(&file).expect("copy_file");
        let zip = ops.compress(&file).expect("compress");
        assert_eq!(zip, dir.path().join("Ä b&c.zip"));
        assert!(std::fs::metadata(&zip).unwrap().len() > 0, "the zip is not empty");
        // A second zip does not overwrite the first.
        assert_eq!(ops.compress(&file).unwrap(), dir.path().join("Ä b&c (2).zip"));
        println!("probe: copied {} and zipped it to {}", file.display(), zip.display());
        dir.close().unwrap();
    }

    #[test]
    fn the_fake_logs_calls_and_fails_on_demand() {
        let ops = FakeFileOps::default();
        assert_eq!(ops.copy_file(Path::new(r"C:\a")), Ok(()));
        ops.fail("copy_file", "boom");
        assert_eq!(ops.copy_file(Path::new(r"C:\b")), Err("boom".to_string()));
        assert_eq!(ops.calls(), [r"copy_file C:\a", r"copy_file C:\b"]);
    }
}
