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
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::Shell::{
    FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName, SHObjectProperties, SHOpenWithDialog, DROPFILES,
    FILEOPERATION_FLAGS, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING,
    OAIF_EXEC, OPENASINFO, SHOP_FILEPATH,
};

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
    /// To the Recycle Bin.
    fn recycle(&self, path: &Path) -> Result<(), String>;
    /// Renames within the same folder; the new path.
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

/// Whether `name` can be the new name of a file or folder: the error is a
/// short sentence for the bar.
pub(crate) fn valid_new_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Type a name".into());
    }
    if name.chars().any(|c| c.is_control() || r#"\/:*?"<>|"#.contains(c)) {
        return Err(r#"A name can't contain \ / : * ? " < > |"#.into());
    }
    if name.ends_with([' ', '.']) {
        return Err("A name can't end with a space or a dot".into());
    }
    // Windows reserves these with any extension (`con.txt`, `NUL.tar.gz`).
    let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    let numbered = |prefix: &str| stem.strip_prefix(prefix).is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']));
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered("COM") || numbered("LPT") {
        return Err("That name is reserved by Windows".into());
    }
    Ok(())
}

/// A row id without the `\` a folder's id ends with.
fn bare(id: &str) -> &str {
    id.trim_end_matches('\\')
}

/// Whether two row ids name the same item: equal without regard to case, a
/// folder's id with or without its trailing `\`.
pub(crate) fn same_item(a: &str, b: &str) -> bool {
    bare(a).to_lowercase() == bare(b).to_lowercase()
}

/// The id `id` gets when `old` becomes `new` (paths of items, folders with or
/// without a trailing `\`): `new` itself for `old`, and for anything below
/// `old` the same rest below `new`. `None` for any other id. A folder id keeps
/// its trailing `\`.
pub(crate) fn moved_id(id: &str, old: &str, new: &str) -> Option<String> {
    let (id_bare, old_bare) = (bare(id), bare(old));
    if old_bare.is_empty() {
        return None;
    }
    let head = id_bare.get(..old_bare.len())?;
    if head.to_lowercase() != old_bare.to_lowercase() {
        return None;
    }
    let rest = &id_bare[old_bare.len()..];
    (rest.is_empty() || rest.starts_with('\\')).then(|| format!("{}{rest}{}", bare(new), &id[id_bare.len()..]))
}

/// Whether `path` is a drive root: `C:\` or `C:`, any letter case.
pub(crate) fn is_drive_root(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || (b.len() == 3 && b[2] == b'\\'))
}


/// Where the zip of `path` goes (`is_dir`: a folder): `<name>.zip` next to it, else `<name> (2).zip`,
/// `(3)` and so on, the first one `exists` does not know.
pub(crate) fn zip_target(path: &Path, is_dir: bool, exists: &dyn Fn(&Path) -> bool) -> PathBuf {
    let parent = path.parent().unwrap_or(Path::new(""));
    // A folder keeps its whole name; a file drops its extension.
    let name = if is_dir { path.file_name() } else { path.file_stem() };
    let stem = name.unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
    let first = parent.join(format!("{stem}.zip"));
    if !exists(&first) {
        return first;
    }
    (2..)
        .map(|n| parent.join(format!("{stem} ({n}).zip")))
        .find(|candidate| !exists(candidate))
        .expect("an unbounded range ends at a free name")
}

/// The name of `item` inside a zip made from `base`: its path below `base`
/// with `/` separators and no `.` or `..` parts. `None` when `item` is not
/// below `base`.
pub(crate) fn zip_entry_name(base: &Path, item: &Path) -> Option<String> {
    let rest = item.strip_prefix(base).ok()?;
    let parts: Vec<String> = rest
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Writes `zip` holding `item` (a file, or a folder with everything in it)
/// under its own name, relative to `parent`. Symlinks and junctions are left
/// out, never followed. Names are stored as UTF-8, contents deflated.
fn write_zip(parent: &Path, item: &Path, zip: &Path) -> Result<(), String> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    // `create_new`: never replaces a zip that appeared since the name was picked.
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(zip).map_err(|e| short(&e))?;
    let mut writer = zip::ZipWriter::new(file);
    let mut stack = vec![item.to_path_buf()];
    while let Some(current) = stack.pop() {
        let meta = std::fs::symlink_metadata(&current).map_err(|e| short(&e))?;
        let Some(name) = zip_entry_name(parent, &current) else { return Err("outside".into()) };
        if meta.file_type().is_symlink() {
            if current == item {
                return Err("link".into());
            }
            continue;
        }
        if meta.is_dir() {
            writer.add_directory(format!("{name}/"), options).map_err(|e| e.to_string())?;
            for child in std::fs::read_dir(&current).map_err(|e| short(&e))? {
                stack.push(child.map_err(|e| short(&e))?.path());
            }
        } else {
            writer.start_file(name, options).map_err(|e| e.to_string())?;
            let mut source = std::fs::File::open(&current).map_err(|e| short(&e))?;
            std::io::copy(&mut source, &mut writer).map_err(|e| short(&e))?;
        }
    }
    writer.finish().map_err(|e| e.to_string())?.flush().map_err(|e| short(&e))
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

fn wide_str(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn file_name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// One shell file operation on `path`, with `flags`, run to the end on this
/// thread (COM is set up for the call). `queue` adds the work to the operation.
fn shell_operation(
    path: &Path,
    flags: FILEOPERATION_FLAGS,
    queue: impl FnOnce(&IFileOperation, &IShellItem) -> windows::core::Result<()>,
) -> Result<(), String> {
    let name = wide(path);
    crate::launch::with_com(|| {
        // SAFETY: `name` is NUL-terminated and outlives the calls; the interfaces
        // are used on this thread only and released when they go out of scope.
        unsafe {
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
            op.SetOperationFlags(flags).map_err(|e| e.to_string())?;
            let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None).map_err(|e| e.to_string())?;
            queue(&op, &item).map_err(|e| e.to_string())?;
            op.PerformOperations().map_err(|e| e.to_string())?;
            if op.GetAnyOperationsAborted().map_err(|e| e.to_string())?.as_bool() {
                return Err("aborted".to_string());
            }
            Ok(())
        }
    })
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
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (Some(parent), false) = (path.parent(), name.is_empty()) else { return Err("can't compress this".into()) };
        let is_dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
        let zip = zip_target(path, is_dir, &|p| p.exists());
        write_zip(parent, path, &zip).map_err(|_| {
            let _ = std::fs::remove_file(&zip);
            format!("Couldn't compress {name}")
        })?;
        Ok(zip)
    }

    fn set_cloud_pin(&self, path: &Path, keep: bool) -> Result<(), String> {
        match pin_tree(path, keep) {
            (0, Some(e)) => Err(e),
            _ => Ok(()),
        }
    }

    /// Asks for a confirmation only when the item is too big for the bin
    /// (it would be deleted for good): never silently.
    fn recycle(&self, path: &Path) -> Result<(), String> {
        let flags = FOFX_RECYCLEONDELETE | FOF_ALLOWUNDO | FOF_WANTNUKEWARNING | FOF_NOERRORUI | FOF_SILENT;
        shell_operation(path, flags, |op, item| unsafe { op.DeleteItem(item, None) })
            .map_err(|_| format!("Couldn't delete {}", file_name_of(path)))
    }

    fn rename(&self, path: &Path, new_name: &str) -> Result<PathBuf, String> {
        valid_new_name(new_name)?;
        let target = path.with_file_name(new_name);
        let same_name = path.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase() == new_name.to_lowercase());
        // The shell would ask or number the new name: refuse before it can.
        if !same_name && std::fs::symlink_metadata(&target).is_ok() {
            return Err("A file with that name already exists".into());
        }
        let flags = FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT;
        let new = wide_str(new_name);
        shell_operation(path, flags, |op, item| unsafe { op.RenameItem(item, PCWSTR(new.as_ptr()), None) })
            .map_err(|_| format!("Couldn't rename {}", file_name_of(path)))?;
        Ok(target)
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
    fn name_rules() {
        let refused = |name: &str| valid_new_name(name).unwrap_err();
        for blank in ["", " ", "   ", "\t"] {
            assert_eq!(refused(blank), "Type a name", "{blank:?}");
        }
        let chars = r#"A name can't contain \ / : * ? " < > |"#;
        for bad in ["a\\b", "a/b", "a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "a\u{1}b", "a\nb", "a\u{7f}b"] {
            assert_eq!(refused(bad), chars, "{bad:?}");
        }
        let ending = "A name can't end with a space or a dot";
        for bad in [".", "..", "a.", "a ", "a. ", "report.final."] {
            assert_eq!(refused(bad), ending, "{bad:?}");
        }
        let reserved = "That name is reserved by Windows";
        for bad in ["CON", "con", "Prn", "aux", "NUL", "COM1", "com9", "LPT1", "lpt9", "con.txt", "NUL.tar.gz", "Aux.TXT"] {
            assert_eq!(refused(bad), reserved, "{bad:?}");
        }
        for good in ["report.final.docx", "a.b", ".gitignore", "Ä ß 😀.txt", "console", "com10", "com0", "lpt", "a b", "-x", "100%", "a&b^c"] {
            assert_eq!(valid_new_name(good), Ok(()), "{good:?}");
        }
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
        assert_eq!(zip_target(Path::new(r"C:\d\a.txt"), false, &taken(&[])), PathBuf::from(r"C:\d\a.zip"));
        assert_eq!(zip_target(Path::new(r"C:\d\Photos"), true, &taken(&[])), PathBuf::from(r"C:\d\Photos.zip"));
        assert_eq!(zip_target(Path::new(r"C:\d\a.txt"), false, &taken(&[r"C:\d\a.zip"])), PathBuf::from(r"C:\d\a (2).zip"));
        assert_eq!(
            zip_target(Path::new(r"C:\d\a.txt"), false, &taken(&[r"C:\d\a.zip", r"C:\d\a (2).zip"])),
            PathBuf::from(r"C:\d\a (3).zip")
        );
    }

    #[test]
    fn zip_target_keeps_a_dotted_folder_name() {
        let none = |_: &Path| false;
        assert_eq!(zip_target(Path::new(r"C:\d\v1.2"), true, &none), PathBuf::from(r"C:\d\v1.2.zip"));
        assert_eq!(zip_target(Path::new(r"C:\d\v1.2"), false, &none), PathBuf::from(r"C:\d\v1.zip"));
    }

    #[test]
    fn zip_target_of_a_dotfile() {
        let none = |_: &Path| false;
        assert_eq!(zip_target(Path::new(r"C:\d\.gitignore"), false, &none), PathBuf::from(r"C:\d\.gitignore.zip"));
        assert_eq!(zip_target(Path::new(r"C:\d\.config"), true, &none), PathBuf::from(r"C:\d\.config.zip"));
        assert_eq!(zip_target(Path::new(r"C:\d\archive.tar.gz"), false, &none), PathBuf::from(r"C:\d\archive.tar.zip"));
    }

    #[test]
    fn zip_entry_names_are_relative_with_slashes() {
        let base = Path::new(r"C:\d");
        assert_eq!(zip_entry_name(base, Path::new(r"C:\d\ł😀 b&c.txt")).as_deref(), Some("ł😀 b&c.txt"));
        assert_eq!(zip_entry_name(base, Path::new(r"C:\d\Ä dir\sub\x.txt")).as_deref(), Some("Ä dir/sub/x.txt"));
        assert_eq!(zip_entry_name(base, Path::new(r"C:\d\-x")).as_deref(), Some("-x"));
        assert_eq!(zip_entry_name(base, base), None);
        assert_eq!(zip_entry_name(base, Path::new(r"C:\other\a.txt")), None);
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
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

    /// Real copy to the clipboard and real zips, in a fresh temp folder that is
    /// deleted at the end. Run by hand: `cargo test -- --ignored real_copy_file_and_zip_probe`.
    #[test]
    #[ignore = "uses the real clipboard"]
    fn real_copy_file_and_zip_probe() {
        let entry_names = |zip: &Path| -> Vec<String> {
            let mut archive = zip::ZipArchive::new(std::fs::File::open(zip).unwrap()).unwrap();
            let mut names: Vec<String> = (0..archive.len()).map(|i| archive.by_index(i).unwrap().name().unwrap().into_owned()).collect();
            names.sort();
            names
        };
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ł😀 b&c.txt");
        std::fs::write(&file, b"hello").unwrap();
        let folder = dir.path().join("Ä dir");
        std::fs::create_dir_all(folder.join("empty")).unwrap();
        std::fs::write(folder.join("ü.txt"), b"x").unwrap();
        let ops = SystemFileOps;
        ops.copy_file(&file).expect("copy_file");

        let zip = ops.compress(&file).expect("compress file");
        assert_eq!(zip, dir.path().join("ł😀 b&c.zip"));
        assert_eq!(entry_names(&zip), ["ł😀 b&c.txt"]);
        // A second zip does not overwrite the first.
        assert_eq!(ops.compress(&file).unwrap(), dir.path().join("ł😀 b&c (2).zip"));

        let zip = ops.compress(&folder).expect("compress folder");
        assert_eq!(zip, dir.path().join("Ä dir.zip"));
        assert_eq!(entry_names(&zip), ["Ä dir/", "Ä dir/empty/", "Ä dir/ü.txt"]);
        println!("probe: zip entries read back: {:?}", entry_names(&zip));
        dir.close().unwrap();
    }

    #[test]
    fn moved_ids_follow_a_rename_without_regard_to_case() {
        let m = |id: &str| moved_id(id, r"C:\d\Sub", r"C:\d\New");
        assert_eq!(m(r"C:\d\sub").as_deref(), Some(r"C:\d\New"));
        assert_eq!(m(r"C:\D\SUB\").as_deref(), Some(r"C:\d\New\"));
        assert_eq!(m(r"C:\d\sub\x\y.txt").as_deref(), Some(r"C:\d\New\x\y.txt"));
        assert_eq!(m(r"C:\d\sub2\x.txt"), None);
        assert_eq!(m(r"C:\d"), None);
        assert!(same_item(r"C:\d\Sub\", r"c:\D\sub"));
        assert!(!same_item(r"C:\d\a", r"C:\d\b"));
    }

    /// Real rename and recycle in a fresh temp folder; the file lands in the
    /// Recycle Bin. Run by hand: `cargo test -- --ignored real_rename_and_recycle_probe --nocapture`.
    #[test]
    #[ignore = "puts a file in the real Recycle Bin"]
    fn real_rename_and_recycle_probe() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("flick-probe-a.txt");
        std::fs::write(&a, b"probe").unwrap();
        let ops = SystemFileOps;
        let renamed = ops.rename(&a, "flick-probe-Ä b.txt").expect("rename");
        assert_eq!(renamed, dir.path().join("flick-probe-Ä b.txt"));
        assert!(renamed.exists() && !a.exists());
        std::fs::write(&a, b"other").unwrap();
        assert_eq!(ops.rename(&a, "flick-probe-ä B.txt"), Err("A file with that name already exists".to_string()));
        std::fs::remove_file(&a).unwrap();
        ops.recycle(&renamed).expect("recycle");
        assert!(!renamed.exists(), "gone from the folder");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        println!("probe: renamed to {renamed:?}, recycled, folder empty");
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
