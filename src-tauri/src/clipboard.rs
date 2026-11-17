//! Putting text on the Windows clipboard.

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

pub trait Clipboard: Send + Sync {
    fn set_text(&self, text: &str) -> Result<(), String>;
}

/// Reading the text on the clipboard (read only).
#[cfg_attr(not(test), allow(dead_code))] // until the qr provider is registered (Task 8)
pub trait ClipboardText: Send + Sync {
    /// The Unicode text on the clipboard; `None` when it holds no text.
    fn text(&self) -> Result<Option<String>, String>;
}

/// The real clipboard (Unicode text).
pub struct WindowsClipboard;

const FAILED: &str = "could not copy to the clipboard";

/// Closes the clipboard when dropped.
struct Open;

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: only built after OpenClipboard succeeded on this thread.
        let _ = unsafe { CloseClipboard() };
    }
}

/// Another program may hold the clipboard for a moment: a few short retries.
fn open() -> Result<Open, String> {
    for attempt in 0..5 {
        // SAFETY: no owner window; the clipboard is closed again by `Open`.
        if unsafe { OpenClipboard(None) }.is_ok() {
            return Ok(Open);
        }
        if attempt < 4 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    Err(FAILED.into())
}

impl Clipboard for WindowsClipboard {
    fn set_text(&self, text: &str) -> Result<(), String> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = wide.len() * std::mem::size_of::<u16>();
        let _open = open()?;
        // SAFETY: the clipboard is open on this thread; the global block is allocated
        // with the size copied into it, and given to the system only on success.
        unsafe {
            EmptyClipboard().map_err(|_| FAILED.to_string())?;
            let block: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|_| FAILED.to_string())?;
            let dest = GlobalLock(block) as *mut u16;
            if dest.is_null() {
                let _ = GlobalFree(Some(block));
                return Err(FAILED.into());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), dest, wide.len());
            let _ = GlobalUnlock(block);
            if SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(block.0))).is_err() {
                let _ = GlobalFree(Some(block));
                return Err(FAILED.into());
            }
        }
        Ok(())
    }
}

impl ClipboardText for WindowsClipboard {
    fn text(&self) -> Result<Option<String>, String> {
        // SAFETY: no pointers; asks only whether the format is present.
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32) }.is_err() {
            return Ok(None);
        }
        let _open = open().map_err(|_| "could not read the clipboard".to_string())?;
        // SAFETY: the clipboard is open on this thread. The block belongs to the
        // clipboard; it is locked, read up to its size or the first NUL, and unlocked.
        unsafe {
            let Ok(handle) = GetClipboardData(CF_UNICODETEXT.0 as u32) else { return Ok(None) };
            let block = HGLOBAL(handle.0);
            let units = GlobalSize(block) / std::mem::size_of::<u16>();
            let src = GlobalLock(block) as *const u16;
            if src.is_null() {
                return Err("could not read the clipboard".into());
            }
            let all = std::slice::from_raw_parts(src, units);
            let end = all.iter().position(|&c| c == 0).unwrap_or(units);
            let text = String::from_utf16_lossy(&all[..end]);
            let _ = GlobalUnlock(block);
            Ok(Some(text))
        }
    }
}

/// Test clipboard: records what was copied.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeClipboard(pub std::sync::Mutex<Vec<String>>);

#[cfg(test)]
impl Clipboard for FakeClipboard {
    fn set_text(&self, text: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(text.to_string());
        Ok(())
    }
}

/// Test clipboard text: what [`ClipboardText::text`] returns, and how often it was read.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeText(pub std::sync::Mutex<Option<String>>, pub std::sync::atomic::AtomicUsize);

#[cfg(test)]
impl FakeText {
    pub fn new(text: Option<&str>) -> Self {
        Self(std::sync::Mutex::new(text.map(str::to_string)), Default::default())
    }

    pub fn calls(&self) -> usize {
        self.1.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
impl ClipboardText for FakeText {
    fn text(&self) -> Result<Option<String>, String> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.0.lock().unwrap().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_text_counts_reads() {
        let f = FakeText::new(Some("hi"));
        assert_eq!(f.calls(), 0);
        assert_eq!(f.text(), Ok(Some("hi".to_string())));
        assert_eq!(f.text(), Ok(Some("hi".to_string())));
        assert_eq!(f.calls(), 2);
        *f.0.lock().unwrap() = None;
        assert_eq!(f.text(), Ok(None));
        assert_eq!(f.calls(), 3);
    }
}
