//! Putting text on the Windows clipboard.

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

pub trait Clipboard: Send + Sync {
    fn set_text(&self, text: &str) -> Result<(), String>;
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
