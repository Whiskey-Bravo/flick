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
    FILEOPERATION_FLAGS, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING,
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

/// 100 ns ticks from 1601-01-01 (a FILETIME) to 1970-01-01.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

/// A zip entry's date and time from a local time given as a FILETIME (100 ns
/// ticks since 1601-01-01): `None` outside what a zip can hold (1980-2107).
pub(crate) fn zip_time(local_ticks: u64) -> Option<zip::DateTime> {
    let secs = local_ticks.checked_sub(FILETIME_UNIX_EPOCH)? / 10_000_000;
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Days since 1970-01-01 to a civil date (H. Hinnant's `civil_from_days`).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = yoe + era * 400 + i64::from(month <= 2);
    let (hour, minute, second) = ((rest / 3_600) as u8, (rest / 60 % 60) as u8, (rest % 60) as u8);
    zip::DateTime::from_date_and_time(u16::try_from(year).ok()?, month, day, hour, minute, second).ok()
}

/// A file's modification time as a zip entry's (local time, as Explorer's
/// zips have it); `None` when it can't be had or held.
fn zip_time_of(meta: &std::fs::Metadata) -> Option<zip::DateTime> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    let since = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    let ticks = FILETIME_UNIX_EPOCH + since.as_secs() * 10_000_000 + u64::from(since.subsec_nanos()) / 100;
    let utc = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut local = FILETIME::default();
    // SAFETY: both point at live FILETIMEs for the call.
    unsafe { FileTimeToLocalFileTime(&utc, &mut local) }.ok()?;
    zip_time((u64::from(local.dwHighDateTime) << 32) | u64::from(local.dwLowDateTime))
}

/// Writes `file` (the new zip) holding `item` (a file, or a folder with
/// everything in it) under its own name, relative to `parent`. Symlinks and
/// junctions are left out, never followed. Names are stored as UTF-8,
/// contents deflated, each entry with its file's modification time; an entry
/// of 4 GiB or more is written as ZIP64.
fn write_zip(parent: &Path, item: &Path, file: std::fs::File) -> Result<(), String> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let base = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
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
        let options = match zip_time_of(&meta) {
            Some(time) => base.last_modified_time(time),
            None => base,
        };
        if meta.is_dir() {
            writer.add_directory(format!("{name}/"), options).map_err(|e| e.to_string())?;
            for child in std::fs::read_dir(&current).map_err(|e| short(&e))? {
                stack.push(child.map_err(|e| short(&e))?.path());
            }
        } else {
            let options = options.large_file(meta.len() >= u64::from(u32::MAX));
            writer.start_file(name, options).map_err(|e| e.to_string())?;
            let mut source = std::fs::File::open(&current).map_err(|e| short(&e))?;
            std::io::copy(&mut source, &mut writer).map_err(|e| short(&e))?;
        }
    }
    writer.finish().map_err(|e| e.to_string())?.flush().map_err(|e| short(&e))
}

/// How many names a compress tries when the ones it picked keep appearing
/// before it can create them.
const ZIP_NAME_TRIES: usize = 20;

/// Zips `path` next to itself, as `<name>.zip` or the next free `<name> (N).zip`;
/// the zip's path. `taken` says which names look used. The zip is created
/// new, so nothing already there is ever replaced: a name that turns out to
/// be taken anyway is skipped for the next one. A failed zip is removed only
/// when this call created it.
fn compress_to(path: &Path, taken: &dyn Fn(&Path) -> bool) -> Result<PathBuf, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (Some(parent), false) = (path.parent(), name.is_empty()) else { return Err("can't compress this".into()) };
    let failed = || format!("Couldn't compress {name}");
    let is_dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
    let mut tried: Vec<PathBuf> = Vec::new();
    for _ in 0..ZIP_NAME_TRIES {
        let zip = zip_target(path, is_dir, &|p| taken(p) || tried.iter().any(|t| t == p));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&zip) {
            Ok(file) => {
                return match write_zip(parent, path, file) {
                    Ok(()) => Ok(zip),
                    Err(_) => {
                        // Ours: created by the open above.
                        let _ = std::fs::remove_file(&zip);
                        Err(failed())
                    }
                };
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => tried.push(zip),
            Err(_) => return Err(failed()),
        }
    }
    Err(failed())
}

/// The error for a terminal in a network folder when only cmd.exe is there.
pub(crate) const NO_NETWORK_TERMINAL: &str = "Can't open a terminal in a network folder without Windows Terminal";

/// Whether `path` is on a network share (`\\server\share\…`, also in its
/// `\\?\UNC\…` form).
fn is_unc(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let upper = text.to_ascii_uppercase();
    upper.starts_with(r"\\?\UNC\") || (text.starts_with(r"\\") && !text.starts_with(r"\\?\") && !text.starts_with(r"\\.\"))
}

/// The program and arguments for a terminal in `folder` (passed as the
/// working directory): Windows Terminal when `wt` is there, else a console
/// window of cmd.exe, which can't start in a network folder (it would open in
/// the Windows folder instead), so that is refused.
pub(crate) fn terminal_command(wt: Option<&Path>, folder: &Path) -> Result<(PathBuf, Vec<OsString>), String> {
    match wt {
        Some(wt) => Ok((wt.to_path_buf(), vec!["-d".into(), ".".into()])),
        None if is_unc(folder) => Err(NO_NETWORK_TERMINAL.into()),
        None => Ok((PathBuf::from(crate::quote::system32("cmd.exe")), Vec::new())),
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

/// The error `recycle` gives when the user answered No to Windows' prompt
/// (delete for good an item the bin can't take): the item stays, and the
/// engine treats the delete as not done, with no error to show.
pub(crate) const DELETE_DECLINED: &str = "delete declined";

/// The error `rename` gives when the user answered No to Windows' prompt: the
/// item keeps its name; the command answers with no new id and no error.
pub(crate) const RENAME_DECLINED: &str = "rename declined";

/// How a shell file operation did not finish.
#[derive(Debug, PartialEq)]
enum ShellError {
    /// The user said No (or cancelled) in a prompt Windows showed.
    Declined,
    Failed,
}

/// Whether an operation's error code means the user said No or cancelled.
fn declined(code: windows::core::HRESULT) -> bool {
    use windows::Win32::Foundation::{ERROR_CANCELLED, E_ABORT};
    use windows::Win32::UI::Shell::COPYENGINE_E_USER_CANCELLED;
    code == COPYENGINE_E_USER_CANCELLED || code == ERROR_CANCELLED.to_hresult() || code == E_ABORT
}

/// One shell file operation on `path`, with `flags`, run to the end on this
/// thread (COM is set up for the call). `queue` adds the work to the
/// operation. Any prompt Windows shows is owned by the bar's window.
fn shell_operation(
    path: &Path,
    flags: FILEOPERATION_FLAGS,
    queue: impl FnOnce(&IFileOperation, &IShellItem) -> windows::core::Result<()>,
) -> Result<(), ShellError> {
    let name = wide(path);
    let failed = |_| ShellError::Failed;
    crate::launch::with_com(|| {
        // SAFETY: `name` is NUL-terminated and outlives the calls; the interfaces
        // are used on this thread only and released when they go out of scope.
        unsafe {
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(failed)?;
            op.SetOperationFlags(flags).map_err(failed)?;
            if let Some(owner) = crate::window::bar_hwnd() {
                // Without an owner the prompt can open behind other windows.
                let _ = op.SetOwnerWindow(owner);
            }
            let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None).map_err(failed)?;
            queue(&op, &item).map_err(failed)?;
            if let Err(e) = op.PerformOperations() {
                return Err(if declined(e.code()) { ShellError::Declined } else { ShellError::Failed });
            }
            if op.GetAnyOperationsAborted().map_err(failed)?.as_bool() {
                return Err(ShellError::Declined);
            }
            Ok(())
        }
    })
}

/// The real calls.
pub struct SystemFileOps;

impl FileOps for SystemFileOps {
    fn open_terminal(&self, folder: &Path) -> Result<(), String> {
        let (program, args) = terminal_command(windows_terminal().as_deref(), folder)?;
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
        // A link counts as taken even when what it points at is gone.
        compress_to(path, &|p| std::fs::symlink_metadata(p).is_ok())
    }

    fn set_cloud_pin(&self, path: &Path, keep: bool) -> Result<(), String> {
        match pin_tree(path, keep) {
            (0, Some(e)) => Err(e),
            _ => Ok(()),
        }
    }

    /// Asks for a confirmation only when the item can't go to the bin (too
    /// big, a network share): Windows' own prompt, owned by the bar, which
    /// is held open meanwhile. A No there is [`DELETE_DECLINED`].
    fn recycle(&self, path: &Path) -> Result<(), String> {
        let flags = FOFX_RECYCLEONDELETE | FOF_ALLOWUNDO | FOF_WANTNUKEWARNING | FOF_NOERRORUI | FOF_SILENT;
        crate::window::holding_open(|| shell_operation(path, flags, |op, item| unsafe { op.DeleteItem(item, None) })).map_err(|e| match e {
            ShellError::Declined => DELETE_DECLINED.to_string(),
            ShellError::Failed => format!("Couldn't delete {}", file_name_of(path)),
        })
    }

    /// No FOF_NOCONFIRMATION: should the name be taken after all (it was
    /// free a moment ago), Windows asks instead of replacing the file.
    fn rename(&self, path: &Path, new_name: &str) -> Result<PathBuf, String> {
        valid_new_name(new_name)?;
        let target = path.with_file_name(new_name);
        let same_name = path.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase() == new_name.to_lowercase());
        // The shell would ask or number the new name: refuse before it can.
        if !same_name && std::fs::symlink_metadata(&target).is_ok() {
            return Err("A file with that name already exists".into());
        }
        let flags = FOF_NOERRORUI | FOF_SILENT;
        let new = wide_str(new_name);
        crate::window::holding_open(|| shell_operation(path, flags, |op, item| unsafe { op.RenameItem(item, PCWSTR(new.as_ptr()), None) }))
            .map_err(|e| match e {
                ShellError::Declined => RENAME_DECLINED.to_string(),
                ShellError::Failed => format!("Couldn't rename {}", file_name_of(path)),
            })?;
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
        let local = Path::new(r"C:\Work");
        let (program, args) = terminal_command(Some(&wt), local).unwrap();
        assert_eq!((program, strings(&args)), (wt, vec!["-d".to_string(), ".".to_string()]));
        let (program, args) = terminal_command(None, local).unwrap();
        assert!(program.to_string_lossy().to_lowercase().ends_with(r"system32\cmd.exe"), "{program:?}");
        assert!(args.is_empty());
    }

    #[test]
    fn a_network_folder_needs_windows_terminal() {
        let wt = PathBuf::from(r"C:\wt.exe");
        for share in [r"\\server\share", r"\\server\share\x y", r"\\?\UNC\server\share\x"] {
            assert_eq!(terminal_command(None, Path::new(share)), Err(NO_NETWORK_TERMINAL.to_string()), "{share}");
            assert!(terminal_command(Some(&wt), Path::new(share)).is_ok(), "{share}");
        }
        assert!(terminal_command(None, Path::new(r"\\?\C:\Work")).is_ok(), "a local long path");
        assert!(terminal_command(None, Path::new(r"D:\")).is_ok());
    }

    #[test]
    fn the_fake_refuses_a_network_terminal_like_the_real_one_without_wt() {
        // The fake records the call; the rule itself is `terminal_command`'s,
        // which the real `open_terminal` asks before starting anything.
        let ops = FakeFileOps::default();
        ops.fail("open_terminal", NO_NETWORK_TERMINAL);
        assert_eq!(ops.open_terminal(Path::new(r"\\server\share")), Err(NO_NETWORK_TERMINAL.to_string()));
    }

    #[test]
    fn zip_times_are_the_files_own() {
        let ticks = |unix: u64| FILETIME_UNIX_EPOCH + unix * 10_000_000;
        // 2024-02-29 13:45:30 and 1999-12-31 23:59:58.
        assert_eq!(zip_time(ticks(1_709_214_330)), Some(zip::DateTime::from_date_and_time(2024, 2, 29, 13, 45, 30).unwrap()));
        assert_eq!(zip_time(ticks(946_684_798)), Some(zip::DateTime::from_date_and_time(1999, 12, 31, 23, 59, 58).unwrap()));
        assert_eq!(zip_time(ticks(0)), None, "1970: before what a zip holds");
        assert_eq!(zip_time(0), None);
    }

    #[test]
    fn compress_never_touches_a_zip_already_there() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"hello").unwrap();
        let existing = dir.path().join("a.zip");
        std::fs::write(&existing, b"not mine").unwrap();
        // The name looked free when picked (another compress raced it in).
        let zip = compress_to(&file, &|_| false).unwrap();
        assert_eq!(zip, dir.path().join("a (2).zip"));
        assert_eq!(std::fs::read(&existing).unwrap(), b"not mine");
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&zip).unwrap()).unwrap();
        let entry = archive.by_index(0).unwrap();
        assert_eq!(entry.name().unwrap(), "a.txt");
        let mtime = std::fs::metadata(&file).unwrap();
        assert_eq!(entry.last_modified(), zip_time_of(&mtime), "the file's own time");
        assert_ne!(entry.last_modified().map(|t| t.year()), Some(1980));
    }

    #[test]
    fn a_failed_compress_removes_only_its_own_zip() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("gone.zip");
        std::fs::write(&existing, b"not mine").unwrap();
        // The item is missing, so writing fails after the zip was created.
        let gone = dir.path().join("gone.txt");
        assert_eq!(compress_to(&gone, &|_| false), Err("Couldn't compress gone.txt".to_string()));
        assert_eq!(std::fs::read(&existing).unwrap(), b"not mine");
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["gone.zip"], "its own half-written zip is gone, the other one stays");
    }

    #[test]
    fn a_no_in_windows_prompt_is_a_decline() {
        use windows::Win32::Foundation::{ERROR_CANCELLED, E_ABORT, E_FAIL};
        use windows::Win32::UI::Shell::COPYENGINE_E_USER_CANCELLED;
        assert!(declined(COPYENGINE_E_USER_CANCELLED));
        assert!(declined(ERROR_CANCELLED.to_hresult()));
        assert!(declined(E_ABORT));
        assert!(!declined(E_FAIL));
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
