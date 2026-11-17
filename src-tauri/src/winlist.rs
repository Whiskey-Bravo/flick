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
    fn close(&self, hwnd: isize) -> Result<(), String>;
    fn kill(&self, pid: u32) -> Result<(), String>;
}

/// Window classes that belong to the shell, never offered.
const EXCLUDED_CLASSES: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

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

/// The latest snapshot of open windows. Queries read it; the bar refreshes it
/// when it is shown.
pub struct WindowList {
    ops: Arc<dyn WindowOps>,
    snap: RwLock<Vec<WinInfo>>,
}

impl WindowList {
    pub fn new(ops: Arc<dyn WindowOps>) -> Arc<Self> {
        Arc::new(Self { ops, snap: RwLock::new(Vec::new()) })
    }

    pub fn refresh(&self) {
        let fresh = self.ops.list();
        *self.snap.write().unwrap_or_else(|e| e.into_inner()) = fresh;
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
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindow, GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
        IsWindow, IsWindowVisible, PostMessageW, SetForegroundWindow, ShowWindow, GWL_EXSTYLE, GW_OWNER, SW_RESTORE,
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

    fn image_path(pid: u32) -> Option<String> {
        // SAFETY: plain handle calls; the handle is closed below.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
            let _ = CloseHandle(h);
            r.ok()?;
            Some(String::from_utf16_lossy(&buf[..len as usize]))
        }
    }

    /// True when the token cannot be read from here or reports elevation.
    fn is_elevated(pid: u32) -> bool {
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
                let _ = ShowWindow(w, SW_RESTORE);
            }
            if SetForegroundWindow(w).as_bool() {
                Ok(())
            } else {
                Err("Windows would not switch to it.".to_string())
            }
        }
    }

    pub fn close(h: isize) -> Result<(), String> {
        // SAFETY: posting a message to a window handle.
        unsafe { PostMessageW(Some(hw(h)), WM_CLOSE, WPARAM(0), LPARAM(0)) }.map_err(|_| "Could not close it.".to_string())
    }

    pub fn kill(pid: u32) -> Result<(), String> {
        // SAFETY: handle is closed below.
        unsafe {
            if pid == GetCurrentProcessId() {
                return Err("Not ending Flick itself.".to_string());
            }
            let h = OpenProcess(PROCESS_TERMINATE, false, pid).map_err(|_| "Could not end it.".to_string())?;
            let r = TerminateProcess(h, 1);
            let _ = CloseHandle(h);
            r.map_err(|_| "Could not end it.".to_string())
        }
    }
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
    fn close(&self, hwnd: isize) -> Result<(), String> {
        sys::close(hwnd)
    }
    fn kill(&self, pid: u32) -> Result<(), String> {
        sys::kill(pid)
    }
}
