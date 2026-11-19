//! Windows' own classic context menu for a file or folder (More options…):
//! entries from shell extensions (cloud apps, PowerToys, Send to, Share)
//! appear as in Explorer, and the one picked runs; Windows' own Rename and
//! Delete are left out (Flick's are in its menu). Not unit-tested: it shows a
//! real menu. `Engine::more_options` checks the row before this is reached.

use std::cell::RefCell;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::{w, Interface, PCSTR, PCWSTR, PSTR};
use windows::Win32::Foundation::{ERROR_CANCELLED, E_ABORT, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IContextMenu, IContextMenu2, IContextMenu3, IShellFolder, SHBindToParent, SHParseDisplayName, CMF_EXTENDEDVERBS,
    CMF_NORMAL, GCS_VERBW, CMIC_MASK_PTINVOKE, CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX, SEE_MASK_NOASYNC,
    SEE_MASK_UNICODE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DeleteMenu, DestroyMenu, DestroyWindow, GetMenuItemCount, GetMenuItemID,
    GetMenuItemInfoW, MENUITEMINFOW, MFT_SEPARATOR, MIIM_FTYPE, MF_BYPOSITION,
    PostMessageW, RegisterClassW, MF_BYCOMMAND,
    SetForegroundWindow, TrackPopupMenuEx, HMENU, SW_SHOWNORMAL, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR, WM_NULL, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

/// The menu could not be built (the item is gone, a network folder is offline).
const NO_MENU: &str = "Windows' menu can't be shown for this item";
/// What the user picked could not run.
const FAILED: &str = "That command didn't run";

/// `CMIC_MASK_UNICODE` and `CMIC_MASK_NOASYNC` are `SEE_MASK_UNICODE` and
/// `SEE_MASK_NOASYNC` (shobjidl.h); the crate has only the latter.
const CMIC_MASK_UNICODE: u32 = SEE_MASK_UNICODE;
const CMIC_MASK_NOASYNC: u32 = SEE_MASK_NOASYNC;

/// The ids the menu's entries are numbered from and up to.
const FIRST_ID: u32 = 1;
const LAST_ID: u32 = 0x7FFF;

/// Shows Windows' menu for `path` with its top-left corner at screen pixels
/// `x, y` and runs the entry picked. `true` when a command ran, `false` when
/// the menu was dismissed. Runs on a thread of its own (COM in a
/// single-threaded apartment, with a hidden owner window for the menu's
/// messages) and waits for it: as long as the menu stays open.
pub fn show(path: &Path, x: i32, y: i32) -> Result<bool, String> {
    let path = path.to_path_buf();
    std::thread::Builder::new()
        .name("shell-menu".into())
        .spawn(move || on_own_thread(&path, x, y))
        .map_err(|_| NO_MENU.to_string())?
        .join()
        .map_err(|_| NO_MENU.to_string())?
}

/// The menu's own message handler while it shows: entries drawn by an
/// extension (submenus filled late, icons, owner-drawn items) need these.
enum Handler {
    Three(IContextMenu3),
    Two(IContextMenu2),
}

thread_local! {
    /// Set for the time the menu shows, on the menu's thread only.
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
}

fn instance() -> HINSTANCE {
    extern "C" {
        static __ImageBase: u8;
    }
    // Only the address is taken, never the value.
    HINSTANCE(std::ptr::addr_of!(__ImageBase) as *mut c_void)
}

fn on_own_thread(path: &Path, x: i32, y: i32) -> Result<bool, String> {
    // Declared first, dropped last: everything below is released before
    // COM is.
    let _com = Com::init()?;
    let owner = Owner::create()?;
    let pidl = Pidl::parse(path)?;
    let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: `pidl` is a valid id list owned by `Pidl`; `child` points into
    // it (not freed separately) and is used only while `pidl` lives.
    let folder: IShellFolder = unsafe { SHBindToParent(pidl.0, Some(&mut child)) }.map_err(|_| NO_MENU.to_string())?;
    if child.is_null() {
        return Err(NO_MENU.into());
    }
    // SAFETY: `child` is the last id of `pidl`, a child of `folder`; the
    // owner window lives on this thread.
    let menu: IContextMenu =
        unsafe { folder.GetUIObjectOf(owner.0, &[child as *const ITEMIDLIST], None) }.map_err(|_| NO_MENU.to_string())?;
    let popup = Popup::create()?;
    // SAFETY: `popup.0` is a menu this thread created and still owns.
    let added = unsafe { menu.QueryContextMenu(popup.0, 0, FIRST_ID, LAST_ID, CMF_NORMAL | CMF_EXTENDEDVERBS) };
    if added.is_err() {
        return Err(NO_MENU.into());
    }
    drop_own_entries(&menu, &popup);
    let picked = {
        let _handler = Handling::set(&menu);
        // SAFETY: the owner window and the menu belong to this thread and
        // outlive the call. The foreground owner makes the menu close on a
        // click elsewhere; WM_NULL afterwards lets it close properly
        // (KB135788).
        unsafe {
            let _ = SetForegroundWindow(owner.0);
            let picked = TrackPopupMenuEx(popup.0, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, x, y, owner.0, None).0;
            let _ = PostMessageW(Some(owner.0), WM_NULL, WPARAM(0), LPARAM(0));
            picked
        }
    };
    let Some(offset) = command_offset(picked) else {
        return Ok(false);
    };
    invoke(&menu, owner.0, offset, x, y)
}

/// Whether an entry with this verb is left out of Windows' menu: Rename and
/// Delete are Flick's own entries (Delete with its second Enter), and keep
/// Flick's pins, ranking and history in step; Windows' would bypass both.
fn dropped_verb(verb: &str) -> bool {
    verb.eq_ignore_ascii_case("rename") || verb.eq_ignore_ascii_case("delete")
}

/// Removes the entries [`dropped_verb`] names from the menu's top level (where
/// Explorer puts them); every other entry stays. An entry whose verb can't be
/// read stays too.
fn drop_own_entries(menu: &IContextMenu, popup: &Popup) {
    // SAFETY: `popup.0` is a live menu of this thread; the verb buffer
    // outlives each call, which writes at most `buf.len()` UTF-16 units.
    unsafe {
        for pos in (0..GetMenuItemCount(Some(popup.0))).rev() {
            let Some(offset) = command_offset(GetMenuItemID(popup.0, pos) as i32) else { continue };
            let mut buf = [0u16; 64];
            // GCS_VERBW writes UTF-16 through the PSTR.
            if menu.GetCommandString(offset as usize, GCS_VERBW, None, PSTR(buf.as_mut_ptr().cast()), buf.len() as u32).is_err() {
                continue;
            }
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            if dropped_verb(&String::from_utf16_lossy(&buf[..end])) {
                let _ = DeleteMenu(popup.0, offset + FIRST_ID, MF_BYCOMMAND);
            }
        }
        tidy_separators(popup);
    }
}

/// The positions of separators to remove so the menu has none first or last
/// and never two in a row. `is_separator[i]` says whether item `i` is one.
fn separators_to_drop(is_separator: &[bool]) -> Vec<usize> {
    let first = is_separator.iter().position(|&s| !s);
    let Some(first) = first else { return (0..is_separator.len()).collect() };
    let last = is_separator.iter().rposition(|&s| !s).unwrap_or(first);
    (0..is_separator.len()).filter(|&i| is_separator[i] && (i < first || i > last || is_separator[i - 1])).collect()
}

/// Applies [`separators_to_drop`] to the menu's top level, from the end so
/// positions stay valid.
///
/// # Safety
/// `popup.0` must be a live menu of this thread.
unsafe fn tidy_separators(popup: &Popup) {
    let count = GetMenuItemCount(Some(popup.0)).max(0) as u32;
    let kinds: Vec<bool> = (0..count)
        .map(|pos| {
            let mut info = MENUITEMINFOW { cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32, fMask: MIIM_FTYPE, ..Default::default() };
            GetMenuItemInfoW(popup.0, pos, true, &mut info).is_ok() && info.fType.0 & MFT_SEPARATOR.0 != 0
        })
        .collect();
    for pos in separators_to_drop(&kinds).into_iter().rev() {
        let _ = DeleteMenu(popup.0, pos as u32, MF_BYPOSITION);
    }
}

/// The command offset `InvokeCommand` takes for the id the menu returned;
/// `None` when nothing was picked (0) or the id is not one of the menu's.
fn command_offset(picked: i32) -> Option<u32> {
    let id = u32::try_from(picked).ok()?;
    (FIRST_ID..=LAST_ID).contains(&id).then(|| id - FIRST_ID)
}

fn invoke(menu: &IContextMenu, owner: HWND, offset: u32, x: i32, y: i32) -> Result<bool, String> {
    let info = CMINVOKECOMMANDINFOEX {
        cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        // NOASYNC: the menu's thread (its COM apartment and owner window) ends
        // once this returns, so an extension that works asynchronously must
        // finish first.
        fMask: CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE | CMIC_MASK_NOASYNC,
        hwnd: owner,
        // MAKEINTRESOURCE(offset): the verb is the entry's offset, not text.
        lpVerb: PCSTR(offset as usize as *const u8),
        lpVerbW: PCWSTR(offset as usize as *const u16),
        nShow: SW_SHOWNORMAL.0,
        ptInvoke: POINT { x, y },
        ..Default::default()
    };
    // SAFETY: `info` is a complete CMINVOKECOMMANDINFOEX (its size in
    // `cbSize`), which the call reads through the CMINVOKECOMMANDINFO
    // prefix it starts with; it lives for the whole call.
    match unsafe { menu.InvokeCommand(&info as *const CMINVOKECOMMANDINFOEX as *const CMINVOKECOMMANDINFO) } {
        Ok(()) => Ok(true),
        // Cancelled in a dialog the command opened: as if dismissed.
        Err(e) if e.code() == E_ABORT || e.code() == ERROR_CANCELLED.to_hresult() => Ok(false),
        Err(_) => Err(FAILED.into()),
    }
}

/// COM on this thread, balanced on drop when the init succeeded.
struct Com;

impl Com {
    fn init() -> Result<Self, String> {
        // SAFETY: plain COM init on this thread, paired in `drop`.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
            .ok()
            .map_err(|_| NO_MENU.to_string())?;
        Ok(Com)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: pairs the successful CoInitializeEx in `init`, on the same
        // thread (`Com` is created and dropped on the menu's thread).
        unsafe { CoUninitialize() };
    }
}

/// The hidden window that owns the menu and gets its messages; destroyed on drop.
struct Owner(HWND);

impl Owner {
    fn create() -> Result<Self, String> {
        let class = w!("FlickShellMenuOwner");
        // SAFETY: registers a class whose window procedure is `wndproc` and
        // creates a never-shown window of it on this thread.
        unsafe {
            let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance(), lpszClassName: class, ..Default::default() };
            // 0 when already registered by an earlier menu; a real failure
            // makes CreateWindowExW fail below.
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class,
                w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance()),
                None,
            )
            .map_err(|_| NO_MENU.to_string())?;
            Ok(Owner(hwnd))
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        // SAFETY: the window was created on this thread and is destroyed once.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

/// The popup menu the entries are added to; destroyed on drop.
struct Popup(HMENU);

impl Popup {
    fn create() -> Result<Self, String> {
        // SAFETY: no arguments; the menu is destroyed in `drop`.
        unsafe { CreatePopupMenu() }.map(Popup).map_err(|_| NO_MENU.to_string())
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        // SAFETY: a menu this thread created, destroyed once (with its submenus).
        let _ = unsafe { DestroyMenu(self.0) };
    }
}

/// An item id list from the Shell, freed on drop.
struct Pidl(*mut ITEMIDLIST);

impl Pidl {
    fn parse(path: &Path) -> Result<Self, String> {
        let name: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        // SAFETY: `name` is NUL-terminated and outlives the call; the id list
        // it fills in is owned by `Pidl` and freed in `drop`.
        unsafe { SHParseDisplayName(PCWSTR(name.as_ptr()), None::<&IBindCtx>, &mut pidl, 0, None) }
            .map_err(|_| NO_MENU.to_string())?;
        if pidl.is_null() {
            return Err(NO_MENU.into());
        }
        Ok(Pidl(pidl))
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        // SAFETY: the id list SHParseDisplayName allocated, freed once.
        unsafe { CoTaskMemFree(Some(self.0 as *const c_void)) };
    }
}

/// Sets `HANDLER` from the menu for as long as it lives; cleared on drop,
/// before the menu (and COM) are released.
struct Handling;

impl Handling {
    fn set(menu: &IContextMenu) -> Self {
        let handler = menu.cast::<IContextMenu3>().map(Handler::Three).or_else(|_| menu.cast::<IContextMenu2>().map(Handler::Two)).ok();
        HANDLER.with(|h| *h.borrow_mut() = handler);
        Handling
    }
}

impl Drop for Handling {
    fn drop(&mut self) {
        let handler = HANDLER.with(|h| h.borrow_mut().take());
        drop(handler);
    }
}

/// Passes the menu's drawing and filling messages to the extension's
/// handler; everything else is the default.
extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR) {
        // A clone (one more reference) so no borrow is held while the
        // extension runs: it may send more messages to this window.
        let handler = HANDLER.with(|h| match &*h.borrow() {
            Some(Handler::Three(m)) => Some(Handler::Three(m.clone())),
            Some(Handler::Two(m)) => Some(Handler::Two(m.clone())),
            None => None,
        });
        match handler {
            Some(Handler::Three(m)) => {
                let mut result = LRESULT(0);
                // SAFETY: a live interface on its own thread; `result` outlives the call.
                if unsafe { m.HandleMenuMsg2(msg, wparam, lparam, Some(&mut result)) }.is_ok() {
                    return result;
                }
            }
            // IContextMenu2 has no answer for WM_MENUCHAR.
            Some(Handler::Two(m)) if msg != WM_MENUCHAR => {
                // SAFETY: a live interface on its own thread.
                if unsafe { m.HandleMenuMsg(msg, wparam, lparam) }.is_ok() {
                    // WM_INITMENUPOPUP is answered with 0, the drawing messages with TRUE.
                    return LRESULT(if msg == WM_INITMENUPOPUP { 0 } else { 1 });
                }
            }
            _ => {}
        }
    }
    // SAFETY: the default handling for this thread's own window.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_are_tidied_to_none_first_last_or_doubled() {
        const S: bool = true;
        const I: bool = false;
        let cases: [(&[bool], &[usize]); 8] = [
            (&[], &[]),
            (&[I, S, I], &[]),
            (&[S, I, I], &[0]),
            (&[I, I, S], &[2]),
            (&[I, S, S, I], &[2]),
            (&[S, S, I, S, S, S, I, S], &[0, 1, 4, 5, 7]),
            (&[S, S, S], &[0, 1, 2]),
            (&[I, S, S, S, I], &[2, 3]),
        ];
        for (kinds, want) in cases {
            assert_eq!(separators_to_drop(kinds), want, "{kinds:?}");
        }
    }

    #[test]
    fn windows_own_rename_and_delete_are_left_out() {
        for verb in ["rename", "Delete", "DELETE"] {
            assert!(dropped_verb(verb), "{verb}");
        }
        for verb in ["open", "properties", "copy", "cut", "link", "runas", "", "delete2"] {
            assert!(!dropped_verb(verb), "{verb}");
        }
    }

    #[test]
    fn only_the_menus_own_ids_are_run() {
        assert_eq!(command_offset(0), None, "dismissed");
        assert_eq!(command_offset(-1), None);
        assert_eq!(command_offset(FIRST_ID as i32), Some(0));
        assert_eq!(command_offset(42), Some(41));
        assert_eq!(command_offset(LAST_ID as i32), Some(LAST_ID - FIRST_ID));
        assert_eq!(command_offset(LAST_ID as i32 + 1), None);
    }
}
