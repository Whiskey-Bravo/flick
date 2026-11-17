//! Open top-level windows: the list shown to the `windows` provider, and the
//! real Win32 implementation behind it.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, RwLock};

#[derive(Clone, Debug, PartialEq)]
pub struct WinInfo {
    pub hwnd: isize,
    pub pid: u32,
    pub title: String,
    /// Full path of the process image.
    pub exe: String,
    /// File description of the exe, else its file stem.
    pub app: String,
    pub elevated: bool,
}

pub trait WindowOps: Send + Sync {
    fn list(&self) -> Vec<WinInfo>;
    fn exists(&self, hwnd: isize) -> bool;
    fn focus(&self, hwnd: isize) -> Result<(), String>;
    /// `pid` is the owner the window had when listed; the live window must still match.
    fn close(&self, hwnd: isize, pid: u32) -> Result<(), String>;
    /// `exe` is the image the process had when listed; the live process must still match.
    fn kill(&self, pid: u32, exe: &str) -> Result<(), String>;
    /// The window's AppUserModelID (set by Store apps and many shortcuts), if any.
    fn aumid(&self, _hwnd: isize) -> Option<String> {
        None
    }
}

/// Window classes that belong to the shell, never offered.
pub(crate) const EXCLUDED_CLASSES: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

/// Whether a top-level window is one the user would call "an open window".
/// `tool` is WS_EX_TOOLWINDOW.
pub fn listable(class: &str, pid: u32, own_pid: u32, title: &str, tool: bool, cloaked: bool, visible: bool) -> bool {
    visible
        && !cloaked
        && !tool
        && pid != own_pid
        && !title.trim().is_empty()
        && !EXCLUDED_CLASSES.iter().any(|c| c.eq_ignore_ascii_case(class))
}

/// Whether a process found live under `pid` may be ended: it must still be the
/// image that was listed, and never Flick itself or explorer.exe.
pub fn may_terminate(live_exe: &str, expected_exe: &str, pid: u32, own_pid: u32) -> bool {
    let name = std::path::Path::new(live_exe).file_name().map(|s| s.to_string_lossy().to_lowercase());
    pid != 0
        && pid != own_pid
        && !live_exe.is_empty()
        && live_exe.eq_ignore_ascii_case(expected_exe)
        && name.as_deref() != Some("explorer.exe")
}

/// Whether a live window may be sent WM_CLOSE: still owned by the listed
/// process, not Flick's own, not a shell window.
pub fn may_close(live_pid: u32, expected_pid: u32, own_pid: u32, class: &str) -> bool {
    live_pid != 0
        && live_pid == expected_pid
        && live_pid != own_pid
        && !EXCLUDED_CLASSES.iter().any(|c| c.eq_ignore_ascii_case(class))
}

/// The latest snapshot of open windows. Queries read it; the bar refreshes it
/// when it is shown.
pub struct WindowList {
    ops: Arc<dyn WindowOps>,
    snap: RwLock<Vec<WinInfo>>,
    /// AppUserModelID per listed window, read with the list.
    aumids: RwLock<HashMap<isize, String>>,
}

impl WindowList {
    pub fn new(ops: Arc<dyn WindowOps>) -> Arc<Self> {
        Arc::new(Self { ops, snap: RwLock::new(Vec::new()), aumids: RwLock::new(HashMap::new()) })
    }

    pub fn refresh(&self) {
        let fresh = self.ops.list();
        let aumids: HashMap<isize, String> =
            fresh.iter().filter_map(|w| self.ops.aumid(w.hwnd).map(|a| (w.hwnd, a))).collect();
        *self.snap.write().unwrap_or_else(|e| e.into_inner()) = fresh;
        *self.aumids.write().unwrap_or_else(|e| e.into_inner()) = aumids;
    }

    /// The AppUserModelID found for `hwnd` by the last refresh. Memory only.
    pub fn aumid(&self, hwnd: isize) -> Option<String> {
        self.aumids.read().unwrap_or_else(|e| e.into_inner()).get(&hwnd).cloned()
    }

    pub fn snapshot(&self) -> Vec<WinInfo> {
        self.snap.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn ops(&self) -> &Arc<dyn WindowOps> {
        &self.ops
    }
}

/// The real thing. Never use in tests.
#[derive(Default)]
pub struct SystemWindows {
    names: Mutex<HashMap<String, String>>,
}

impl SystemWindows {
    pub fn new() -> Self {
        Self::default()
    }
}

mod sys {
    use super::*;
    use windows::core::{BOOL, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, TerminateProcess,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };
    use windows::core::GUID;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
    use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindow, GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
        IsWindow, IsWindowVisible, PostMessageW, SetForegroundWindow, ShowWindowAsync, GWL_EXSTYLE, GW_OWNER, SW_RESTORE,
        WM_CLOSE, WS_EX_TOOLWINDOW,
    };

    fn hw(h: isize) -> HWND {
        HWND(h as *mut c_void)
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn text_of(buf: &[u16], n: i32) -> String {
        String::from_utf16_lossy(&buf[..(n.max(0) as usize).min(buf.len())])
    }

    /// Image path of an open process handle.
    fn image_of(h: HANDLE) -> Option<String> {
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: buf and len describe the same buffer.
        unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }

    fn image_path(pid: u32) -> Option<String> {
        // SAFETY: the handle is closed before returning.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let r = image_of(h);
            let _ = CloseHandle(h);
            r
        }
    }

    /// True when the token cannot be read from here or reports elevation.
    pub(super) fn is_elevated(pid: u32) -> bool {
        // SAFETY: handles are closed; the out buffer matches the size passed.
        unsafe {
            let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return true;
            };
            let mut tok = HANDLE::default();
            let elevated = if OpenProcessToken(p, TOKEN_QUERY, &mut tok).is_err() {
                true
            } else {
                let mut el = TOKEN_ELEVATION::default();
                let mut ret = 0u32;
                let ok = GetTokenInformation(
                    tok,
                    TokenElevation,
                    Some(&mut el as *mut _ as *mut c_void),
                    std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                    &mut ret,
                );
                let _ = CloseHandle(tok);
                ok.is_err() || el.TokenIsElevated != 0
            };
            let _ = CloseHandle(p);
            elevated
        }
    }

    fn file_description(exe: &str) -> Option<String> {
        let path = wide(exe);
        // SAFETY: the buffer is sized by the API and outlives every query into it.
        unsafe {
            let size = GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None);
            if size == 0 {
                return None;
            }
            let mut data = vec![0u8; size as usize];
            GetFileVersionInfoW(PCWSTR(path.as_ptr()), None, size, data.as_mut_ptr() as *mut c_void).ok()?;
            let mut langs: Vec<String> = Vec::new();
            let mut p: *mut c_void = std::ptr::null_mut();
            let mut n = 0u32;
            let key = wide(r"\VarFileInfo\Translation");
            if VerQueryValueW(data.as_ptr() as *const c_void, PCWSTR(key.as_ptr()), &mut p, &mut n).as_bool()
                && !p.is_null()
                && n >= 4
            {
                let pairs = std::slice::from_raw_parts(p as *const u16, (n / 2) as usize);
                for c in pairs.chunks_exact(2) {
                    langs.push(format!("{:04x}{:04x}", c[0], c[1]));
                }
            }
            langs.push("040904b0".to_string());
            for l in langs {
                let key = wide(&format!(r"\StringFileInfo\{l}\FileDescription"));
                let mut p: *mut c_void = std::ptr::null_mut();
                let mut n = 0u32;
                if VerQueryValueW(data.as_ptr() as *const c_void, PCWSTR(key.as_ptr()), &mut p, &mut n).as_bool()
                    && !p.is_null()
                    && n > 1
                {
                    let s = std::slice::from_raw_parts(p as *const u16, (n - 1) as usize);
                    let s = String::from_utf16_lossy(s).trim().to_string();
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
            None
        }
    }

    fn stem(exe: &str) -> String {
        std::path::Path::new(exe).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }

    struct Ctx {
        own: u32,
        out: Vec<(isize, u32, String)>,
    }

    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: lp is the &mut Ctx passed to EnumWindows below.
        let ctx = unsafe { &mut *(lp.0 as *mut Ctx) };
        // SAFETY: plain queries on a window handle with correctly sized buffers.
        unsafe {
            let visible = IsWindowVisible(hwnd).as_bool();
            let tool = (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32) & WS_EX_TOOLWINDOW.0 != 0;
            let owned = GetWindow(hwnd, GW_OWNER).map(|h| !h.0.is_null()).unwrap_or(false);
            let mut cloak = 0u32;
            let cloaked =
                DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloak as *mut _ as *mut c_void, 4).is_ok() && cloak != 0;
            let mut tbuf = [0u16; 512];
            let tn = GetWindowTextW(hwnd, &mut tbuf);
            let title = text_of(&tbuf, tn);
            let mut cbuf = [0u16; 256];
            let cn = GetClassNameW(hwnd, &mut cbuf);
            let class = text_of(&cbuf, cn);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if !owned && listable(&class, pid, ctx.own, &title, tool, cloaked, visible) {
                ctx.out.push((hwnd.0 as isize, pid, title));
            }
        }
        true.into()
    }

    pub fn list(names: &Mutex<HashMap<String, String>>) -> Vec<super::WinInfo> {
        // SAFETY: no pointers or preconditions.
        let mut ctx = Ctx { own: unsafe { GetCurrentProcessId() }, out: Vec::new() };
        // SAFETY: ctx outlives the synchronous EnumWindows call.
        let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut ctx as *mut Ctx as isize)) };
        let mut elev: HashMap<u32, bool> = HashMap::new();
        let mut out = Vec::new();
        for (hwnd, pid, title) in ctx.out {
            let Some(exe) = image_path(pid) else { continue };
            let elevated = *elev.entry(pid).or_insert_with(|| is_elevated(pid));
            let key = exe.to_lowercase();
            let cached = names.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned();
            let app = match cached {
                Some(a) => a,
                None => {
                    let a = file_description(&exe).unwrap_or_else(|| stem(&exe));
                    names.lock().unwrap_or_else(|e| e.into_inner()).insert(key, a.clone());
                    a
                }
            };
            out.push(super::WinInfo { hwnd, pid, title, exe, app, elevated });
        }
        out
    }

    pub fn exists(h: isize) -> bool {
        // SAFETY: IsWindow accepts any handle value.
        unsafe { IsWindow(Some(hw(h))).as_bool() }
    }

    pub fn focus(h: isize) -> Result<(), String> {
        let w = hw(h);
        // SAFETY: calls on a window handle; failures are reported, not assumed.
        unsafe {
            if IsIconic(w).as_bool() {
                // Async: a hung window must not block this thread.
                let _ = ShowWindowAsync(w, SW_RESTORE);
            }
            if SetForegroundWindow(w).as_bool() {
                Ok(())
            } else {
                Err("Windows would not switch to it.".to_string())
            }
        }
    }

    pub fn close(h: isize, expected_pid: u32) -> Result<(), String> {
        let w = hw(h);
        // SAFETY: queries and a post on a window handle; buffers are sized.
        unsafe {
            if !IsWindow(Some(w)).as_bool() {
                return Err("That window is gone.".to_string());
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(w, Some(&mut pid));
            let mut cbuf = [0u16; 256];
            let cn = GetClassNameW(w, &mut cbuf);
            let class = text_of(&cbuf, cn);
            if !super::may_close(pid, expected_pid, GetCurrentProcessId(), &class) {
                return Err("That window is gone.".to_string());
            }
            PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0)).map_err(|_| "Could not close it.".to_string())
        }
    }

    /// `PKEY_AppUserModel_ID`, spelled out: its home module is another `windows` feature.
    const PKEY_APP_USER_MODEL_ID: PROPERTYKEY =
        PROPERTYKEY { fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3), pid: 5 };

    pub fn aumid(h: isize) -> Option<String> {
        window_aumid(h).or_else(|| process_aumid(h))
    }

    /// The window's own `PKEY_AppUserModel_ID`; many packaged apps do not set it.
    fn window_aumid(h: isize) -> Option<String> {
        // SAFETY: the property store and the string it hands out are released
        // before returning; a window without the property reads as empty.
        unsafe {
            let store: IPropertyStore = SHGetPropertyStoreForWindow(hw(h)).ok()?;
            let value = store.GetValue(&PKEY_APP_USER_MODEL_ID).ok()?;
            let text = PropVariantToStringAlloc(&value).ok()?;
            let id = text.to_string().ok().filter(|s| !s.is_empty());
            windows::Win32::System::Com::CoTaskMemFree(Some(text.as_ptr() as *const c_void));
            id
        }
    }

    /// The AUMID of the window's process (a packaged app's identity). Windows
    /// hosted by ApplicationFrameHost.exe report the host's process, which has
    /// none, so those still do not match; that is accepted.
    fn process_aumid(h: isize) -> Option<String> {
        // SAFETY: the one handle is closed on every path; the buffer outlives the call.
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hw(h), Some(&mut pid));
            if pid == 0 {
                return None;
            }
            let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 256];
            let mut len = buf.len() as u32;
            let rc = GetApplicationUserModelId(proc, &mut len, Some(PWSTR(buf.as_mut_ptr())));
            let _ = CloseHandle(proc);
            if rc != ERROR_SUCCESS || len < 2 {
                return None;
            }
            // `len` counts the terminating nul.
            String::from_utf16(&buf[..(len as usize - 1).min(buf.len())]).ok().filter(|s| !s.is_empty())
        }
    }

    pub fn kill(pid: u32, expected_exe: &str) -> Result<(), String> {
        const GONE: &str = "That app is gone.";
        // SAFETY: the one handle pins the process while it is checked and ended,
        // and is closed on every path.
        unsafe {
            let own = GetCurrentProcessId();
            if pid == own {
                return Err(GONE.to_string());
            }
            let h = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                .map_err(|_| GONE.to_string())?;
            let r = match image_of(h) {
                Some(live) if super::may_terminate(&live, expected_exe, pid, own) => {
                    TerminateProcess(h, 1).map_err(|_| "Could not end it.".to_string())
                }
                _ => Err(GONE.to_string()),
            };
            let _ = CloseHandle(h);
            r
        }
    }
}

/// True when the process token cannot be read from here or reports elevation.
#[allow(dead_code)] // Used by paste.rs until Task 8 wires it in.
pub(crate) fn process_elevated(pid: u32) -> bool {
    sys::is_elevated(pid)
}

impl WindowOps for SystemWindows {
    fn list(&self) -> Vec<WinInfo> {
        sys::list(&self.names)
    }
    fn exists(&self, hwnd: isize) -> bool {
        sys::exists(hwnd)
    }
    fn focus(&self, hwnd: isize) -> Result<(), String> {
        sys::focus(hwnd)
    }
    fn close(&self, hwnd: isize, pid: u32) -> Result<(), String> {
        sys::close(hwnd, pid)
    }
    fn kill(&self, pid: u32, exe: &str) -> Result<(), String> {
        sys::kill(pid, exe)
    }
    fn aumid(&self, hwnd: isize) -> Option<String> {
        sys::aumid(hwnd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lists two windows; only the first has an AUMID. Nothing real is read.
    struct FakeOps;

    impl WindowOps for FakeOps {
        fn list(&self) -> Vec<WinInfo> {
            [1, 2]
                .iter()
                .map(|&hwnd| WinInfo {
                    hwnd,
                    pid: 10,
                    title: "t".into(),
                    exe: r"C:\Apps\Foo\foo.exe".into(),
                    app: "Foo".into(),
                    elevated: false,
                })
                .collect()
        }
        fn exists(&self, _hwnd: isize) -> bool {
            true
        }
        fn focus(&self, _hwnd: isize) -> Result<(), String> {
            Ok(())
        }
        fn close(&self, _hwnd: isize, _pid: u32) -> Result<(), String> {
            Ok(())
        }
        fn kill(&self, _pid: u32, _exe: &str) -> Result<(), String> {
            Ok(())
        }
        fn aumid(&self, hwnd: isize) -> Option<String> {
            (hwnd == 1).then(|| "Contoso.Foo_abc!App".to_string())
        }
    }

    #[test]
    fn refresh_stores_aumids() {
        let list = WindowList::new(Arc::new(FakeOps));
        assert_eq!(list.aumid(1), None);
        list.refresh();
        assert_eq!(list.aumid(1).as_deref(), Some("Contoso.Foo_abc!App"));
        assert_eq!(list.aumid(2), None);
        assert_eq!(list.snapshot().len(), 2);
    }

    /// Prints each real window's AUMID; reads only, closes nothing.
    /// `cargo test real_windows -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_windows_list_aumids() {
        let ops = SystemWindows::new();
        for w in ops.list() {
            println!("{:?} {} {} -> {:?}", w.hwnd, w.app, w.title, ops.aumid(w.hwnd));
        }
    }
}
