//! The colour picker: a frozen full-screen picture of the monitor under the
//! mouse, a loupe, and one click that copies `#RRGGBB`.
//!
//! Everything that decides something is pure and tested here; the window
//! only feeds [`Input`]s to a [`Session`] and does what its [`Step`] says.
//! Every way out (Esc, right-click, focus loss, never getting focus, the
//! watchdog, any error or panic) destroys the window.

use std::cell::RefCell;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use windows::core::{w, BOOL};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DeleteDC, DeleteObject,
    DrawTextW, EndPaint, EnumDisplayMonitors, ExcludeClipRect, FillRect, FrameRect, GdiFlush, GetDC, GetMonitorInfoW, InvalidateRect,
    ReleaseDC, SelectClipRgn, SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, FW_SEMIBOLD, HBITMAP, HDC, HFONT, HGDIOBJ, HMONITOR, MONITORINFO, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
    SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetForegroundWindow, GetMessageW,
    IsWindow, KillTimer, LoadCursorW, PostQuitMessage, RegisterClassW, SetCursor, SetForegroundWindow, SetTimer,
    ShowWindow, TranslateMessage, IDC_CROSS, MSG, SW_SHOW, WA_INACTIVE, WM_ACTIVATE, WM_CLOSE, WM_DESTROY,
    WM_ERASEBKGND, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::clipboard::Clipboard;

/// A monitor or capture area, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// A captured picture: top-down rows of 4-byte BGRA pixels.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl Frame {
    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgb> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        let p = self.bgra.get(i..i + 4)?;
        Some(Rgb(p[2], p[1], p[0]))
    }
}

/// The monitor containing the cursor (`left <= x < left + width`, same for y).
pub fn monitor_at(monitors: &[Rect], cursor: (i32, i32)) -> Option<Rect> {
    monitors.iter().copied().find(|&m| to_frame(m, cursor).is_some())
}

/// Screen coordinates to the monitor's frame pixel, one to one.
pub fn to_frame(monitor: Rect, screen: (i32, i32)) -> Option<(u32, u32)> {
    let x = i64::from(screen.0) - i64::from(monitor.left);
    let y = i64::from(screen.1) - i64::from(monitor.top);
    if (0..i64::from(monitor.width)).contains(&x) && (0..i64::from(monitor.height)).contains(&y) {
        Some((x as u32, y as u32))
    } else {
        None
    }
}

/// `#RRGGBB`, upper case.
pub fn hex(c: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c.0, c.1, c.2)
}

pub const LOUPE_CELLS: u32 = 11;
pub const LOUPE_ZOOM: u32 = 10;

/// Gap between the cursor and the loupe.
const LOUPE_GAP: i64 = 20;

/// The `LOUPE_CELLS` x `LOUPE_CELLS` pixels around `center`, row-major;
/// `None` outside the frame.
pub fn loupe(frame: &Frame, center: (u32, u32)) -> Vec<Option<Rgb>> {
    let half = i64::from(LOUPE_CELLS / 2);
    let mut cells = Vec::with_capacity((LOUPE_CELLS * LOUPE_CELLS) as usize);
    for dy in -half..=half {
        for dx in -half..=half {
            let x = i64::from(center.0) + dx;
            let y = i64::from(center.1) + dy;
            let cell = if x < 0 || y < 0 || x > i64::from(u32::MAX) || y > i64::from(u32::MAX) {
                None
            } else {
                frame.pixel(x as u32, y as u32)
            };
            cells.push(cell);
        }
    }
    cells
}

/// One axis of [`loupe_origin`].
fn loupe_axis(cursor: i32, size: u32, area: u32) -> i32 {
    let (cursor, size, area) = (i64::from(cursor), i64::from(size), i64::from(area));
    let mut start = cursor + LOUPE_GAP;
    if start + size > area {
        start = cursor - LOUPE_GAP - size;
    }
    start.clamp(0, (area - size).max(0)) as i32
}

/// Top-left of a loupe of `size`: 20 px below-right of the cursor, flipped
/// to the other side at an edge, always fully inside `area` (when it fits).
pub fn loupe_origin(cursor: (i32, i32), size: (u32, u32), area: (u32, u32)) -> (i32, i32) {
    (loupe_axis(cursor.0, size.0, area.0), loupe_axis(cursor.1, size.1, area.1))
}

/// What happened to the picker window.
pub enum Input {
    /// Left click at client coordinates, which are frame pixels.
    Click((i32, i32)),
    RightClick,
    Escape,
    FocusLost,
    NoFocus,
    Timeout,
    Error,
}

#[derive(Debug, PartialEq)]
pub enum Step {
    Copy(String),
    Close,
    Ignore,
}

/// One picker run: at most one pick, and every other input closes it.
pub struct Session {
    frame: Frame,
    done: bool,
}

impl Session {
    pub fn new(frame: Frame) -> Self {
        Session { frame, done: false }
    }

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    pub fn handle(&mut self, input: Input) -> Step {
        if self.done {
            return Step::Ignore;
        }
        self.done = true;
        match input {
            Input::Click((x, y)) if x >= 0 && y >= 0 => match self.frame.pixel(x as u32, y as u32) {
                Some(c) => Step::Copy(hex(c)),
                None => Step::Close,
            },
            _ => Step::Close,
        }
    }
}

pub const FOCUS_WAIT: Duration = Duration::from_millis(500);
pub const MAX_OPEN: Duration = Duration::from_secs(60);

/// The real screen, behind a trait so tests never capture it.
pub trait Screen: Send + Sync {
    fn monitors(&self) -> Vec<Rect>;
    fn cursor(&self) -> Option<(i32, i32)>;
    fn capture(&self, r: Rect) -> Result<Frame, String>;
}

/// Captures the monitor under the mouse.
pub fn prepare(screen: &dyn Screen) -> Result<(Rect, Frame), String> {
    let cursor = screen.cursor().ok_or("can't find the mouse")?;
    let rect = monitor_at(&screen.monitors(), cursor).ok_or("no monitor under the mouse")?;
    let frame = screen.capture(rect)?;
    if (frame.width, frame.height) != (rect.width, rect.height)
        || frame.bgra.len() != rect.width as usize * rect.height as usize * 4
    {
        return Err("the screen capture has the wrong size".into());
    }
    Ok((rect, frame))
}

// ---- The real screen and window ---------------------------------------------

/// Runs a closure when dropped.
struct Defer<F: FnMut()>(F);

impl<F: FnMut()> Drop for Defer<F> {
    fn drop(&mut self) {
        (self.0)()
    }
}

fn bitmap_info(width: i32, height: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Negative: rows top-down, like `Frame`.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// A top-down 32-bit DIB selected into its own memory DC; freed on drop.
struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl Canvas {
    fn new(width: i32, height: i32) -> Result<Canvas, String> {
        const FAILED: &str = "could not make a picture of the screen";
        if width <= 0 || height <= 0 {
            return Err(FAILED.into());
        }
        // SAFETY: plain GDI object creation; everything made here is freed on
        // the error paths or by `Drop`.
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return Err(FAILED.into());
            }
            let info = bitmap_info(width, height);
            let mut bits: *mut c_void = null_mut();
            let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) if !bits.is_null() => b,
                Ok(b) => {
                    let _ = DeleteObject(b.into());
                    let _ = DeleteDC(dc);
                    return Err(FAILED.into());
                }
                Err(_) => {
                    let _ = DeleteDC(dc);
                    return Err(FAILED.into());
                }
            };
            let old = SelectObject(dc, bitmap.into());
            Ok(Canvas { dc, bitmap, old, bits: bits as *mut u8, width, height })
        }
    }

    fn len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: the DC and bitmap were made by `new` and are freed once.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// The real screen, read with GDI. Coordinates are physical pixels only on a
/// thread set to per-monitor DPI awareness, as [`start`]'s thread is.
pub struct GdiScreen;

unsafe extern "system" fn add_monitor(monitor: HMONITOR, _dc: HDC, _clip: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the `&mut Vec<Rect>` passed by `monitors` for the
    // duration of the enumeration.
    let list = unsafe { &mut *(data.0 as *mut Vec<Rect>) };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `info` is a properly sized MONITORINFO.
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        let r = info.rcMonitor;
        if r.right > r.left && r.bottom > r.top {
            list.push(Rect {
                left: r.left,
                top: r.top,
                width: (r.right - r.left) as u32,
                height: (r.bottom - r.top) as u32,
            });
        }
    }
    BOOL(1)
}

impl Screen for GdiScreen {
    fn monitors(&self) -> Vec<Rect> {
        let mut list: Vec<Rect> = Vec::new();
        // SAFETY: the callback only uses `list` while this call runs.
        let _ = unsafe { EnumDisplayMonitors(None, None, Some(add_monitor), LPARAM(&mut list as *mut Vec<Rect> as isize)) };
        list
    }

    fn cursor(&self) -> Option<(i32, i32)> {
        let mut p = POINT::default();
        // SAFETY: writes one POINT.
        unsafe { GetCursorPos(&mut p) }.ok()?;
        Some((p.x, p.y))
    }

    fn capture(&self, r: Rect) -> Result<Frame, String> {
        const FAILED: &str = "could not capture the screen";
        let width = i32::try_from(r.width).map_err(|_| FAILED.to_string())?;
        let height = i32::try_from(r.height).map_err(|_| FAILED.to_string())?;
        let canvas = Canvas::new(width, height)?;
        // SAFETY: the screen DC is released before returning; the DIB bits
        // hold `canvas.len()` bytes and are read only after GdiFlush.
        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                return Err(FAILED.into());
            }
            let copied = BitBlt(canvas.dc, 0, 0, width, height, Some(screen), r.left, r.top, SRCCOPY | CAPTUREBLT);
            ReleaseDC(None, screen);
            copied.map_err(|_| FAILED.to_string())?;
            let _ = GdiFlush();
            let bgra = std::slice::from_raw_parts(canvas.bits, canvas.len()).to_vec();
            Ok(Frame { width: r.width, height: r.height, bgra })
        }
    }
}

/// Loupe cells as drawn: `LOUPE_CELLS` x `LOUPE_ZOOM` pixels square.
const LOUPE_PX: i32 = (LOUPE_CELLS * LOUPE_ZOOM) as i32;
const LOUPE_BORDER: i32 = 1;
/// The hex label under the cells.
const LABEL_PX: i32 = 24;
const LOUPE_SIZE: (u32, u32) = ((LOUPE_PX + 2 * LOUPE_BORDER) as u32, (LOUPE_PX + 2 * LOUPE_BORDER + LABEL_PX) as u32);

const FOCUS_TIMER: usize = 1;
const MAX_TIMER: usize = 2;

fn colorref(c: Rgb) -> COLORREF {
    COLORREF(u32::from(c.0) | u32::from(c.1) << 8 | u32::from(c.2) << 16)
}

fn loupe_rect(cursor: (i32, i32), area: (u32, u32)) -> RECT {
    let (x, y) = loupe_origin(cursor, LOUPE_SIZE, area);
    RECT { left: x, top: y, right: x + LOUPE_SIZE.0 as i32, bottom: y + LOUPE_SIZE.1 as i32 }
}

/// The open picker's state, on its own thread.
struct Picker {
    session: Session,
    clipboard: Arc<dyn Clipboard>,
    size: (u32, u32),
    /// Mouse position in client coordinates (= frame pixels).
    cursor: Option<(i32, i32)>,
    /// The frozen frame, ready to blit. The session's `Frame` keeps the
    /// pixels for sampling; the copy only lives as long as this DIB.
    picture: Canvas,
    /// The loupe, drawn here first so it never flickers (loupe-sized).
    back: Canvas,
    font: HFONT,
    /// The left button went down in this window: only then does its
    /// release pick (a stray release from before the picker opened never does).
    pressed: bool,
}

impl Drop for Picker {
    fn drop(&mut self) {
        if !self.font.is_invalid() {
            // SAFETY: the font was made by `run` and is no longer selected.
            let _ = unsafe { DeleteObject(self.font.into()) };
        }
    }
}

thread_local! {
    static PICKER: RefCell<Option<Picker>> = const { RefCell::new(None) };
}

/// Destroys the window when dropped (normal end, error or panic).
struct Window(HWND);

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: called on the thread that made the window.
        unsafe {
            if IsWindow(Some(self.0)).as_bool() {
                let _ = DestroyWindow(self.0);
            }
        }
    }
}

fn destroy(hwnd: HWND) {
    // SAFETY: called on the window's own thread.
    let _ = unsafe { DestroyWindow(hwnd) };
}

/// Runs `input` through the session and does what it says. The state is not
/// borrowed while copying or destroying, which re-enter the window procedure.
fn feed(hwnd: HWND, input: Input) {
    let step = PICKER.with(|p| {
        let mut p = p.try_borrow_mut().ok()?;
        let p = p.as_mut()?;
        Some((p.session.handle(input), p.clipboard.clone()))
    });
    match step {
        Some((Step::Ignore, _)) => {}
        // Close first, so a slow clipboard never keeps the frozen window up.
        Some((Step::Copy(hex), clipboard)) => {
            destroy(hwnd);
            let _ = clipboard.set_text(&hex);
        }
        // Close, or no usable state: close.
        _ => destroy(hwnd),
    }
}

fn client_point(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 & 0xFFFF) as u16 as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
    (x, y)
}

fn move_to(hwnd: HWND, p: (i32, i32)) {
    let rects = PICKER.with(|s| {
        let mut s = s.try_borrow_mut().ok()?;
        let s = s.as_mut()?;
        let old = s.cursor.map(|c| loupe_rect(c, s.size));
        s.cursor = Some(p);
        Some((old, loupe_rect(p, s.size)))
    });
    if let Some((old, new)) = rects {
        // SAFETY: rects are plain values; the window is ours.
        unsafe {
            if let Some(old) = old {
                let _ = InvalidateRect(Some(hwnd), Some(&old), false);
            }
            let _ = InvalidateRect(Some(hwnd), Some(&new), false);
        }
    }
}

/// Fills `r` with one colour.
///
/// SAFETY: `dc` must be a valid DC.
unsafe fn fill(dc: HDC, r: &RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(dc, r, brush);
        let _ = DeleteObject(brush.into());
    }
}

/// Outlines `r` with one colour.
///
/// SAFETY: `dc` must be a valid DC.
unsafe fn outline(dc: HDC, r: &RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FrameRect(dc, r, brush);
        let _ = DeleteObject(brush.into());
    }
}

/// Draws the loupe for `cursor` on `p.back`, at its top-left. Returns where
/// it goes in the window, or `None` when there is nothing to draw.
fn draw_loupe(p: &Picker, cursor: (i32, i32)) -> Option<RECT> {
    if cursor.0 < 0 || cursor.1 < 0 {
        return None;
    }
    let center = (cursor.0 as u32, cursor.1 as u32);
    let frame = p.session.frame();
    let picked = frame.pixel(center.0, center.1)?;
    let dc = p.back.dc;
    let target = loupe_rect(cursor, p.size);
    let area = RECT { left: 0, top: 0, right: LOUPE_SIZE.0 as i32, bottom: LOUPE_SIZE.1 as i32 };
    let cells = loupe(frame, center);
    let zoom = LOUPE_ZOOM as i32;
    let x0 = area.left + LOUPE_BORDER;
    let y0 = area.top + LOUPE_BORDER;
    // SAFETY: `dc` is the back buffer's memory DC, alive while `p` is.
    unsafe {
        fill(dc, &area, COLORREF(0x0020_2020));
        for (i, cell) in cells.iter().enumerate() {
            let (col, row) = ((i as u32 % LOUPE_CELLS) as i32, (i as u32 / LOUPE_CELLS) as i32);
            let r = RECT {
                left: x0 + col * zoom,
                top: y0 + row * zoom,
                right: x0 + (col + 1) * zoom,
                bottom: y0 + (row + 1) * zoom,
            };
            fill(dc, &r, cell.map(colorref).unwrap_or(COLORREF(0x0040_4040)));
        }
        let mid = (LOUPE_CELLS / 2) as i32;
        let r = RECT {
            left: x0 + mid * zoom - 1,
            top: y0 + mid * zoom - 1,
            right: x0 + (mid + 1) * zoom + 1,
            bottom: y0 + (mid + 1) * zoom + 1,
        };
        outline(dc, &r, COLORREF(0x00FF_FFFF));
        let r = RECT { left: r.left - 1, top: r.top - 1, right: r.right + 1, bottom: r.bottom + 1 };
        outline(dc, &r, COLORREF(0));
        let mut label = RECT { left: area.left, top: y0 + LOUPE_PX, right: area.right, bottom: area.bottom };
        let mut text: Vec<u16> = hex(picked).encode_utf16().collect();
        let old_font = SelectObject(dc, p.font.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00FF_FFFF));
        DrawTextW(dc, &mut text, &mut label, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(dc, old_font);
    }
    Some(target)
}

fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: BeginPaint/EndPaint pair on our window; the canvases live in
    // the state borrowed for the whole paint.
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        PICKER.with(|s| {
            let Ok(s) = s.try_borrow() else { return };
            let Some(p) = s.as_ref() else { return };
            let r = ps.rcPaint;
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let loupe = p.cursor.and_then(|c| draw_loupe(p, c));
            // The frame everywhere but under the loupe, then the loupe: no
            // pixel is drawn twice, so nothing flickers.
            if let Some(l) = loupe {
                ExcludeClipRect(hdc, l.left, l.top, l.right, l.bottom);
            }
            let _ = BitBlt(hdc, r.left, r.top, w, h, Some(p.picture.dc), r.left, r.top, SRCCOPY);
            if let Some(l) = loupe {
                SelectClipRgn(hdc, None);
                let _ = BitBlt(hdc, l.left, l.top, l.right - l.left, l.bottom - l.top, Some(p.back.dc), 0, 0, SRCCOPY);
            }
        });
        let _ = EndPaint(hwnd, &ps);
    }
}

fn on_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        // Buttons act on release, so the release never lands on the window
        // underneath (a right-button release there opens its menu).
        WM_RBUTTONDOWN => {}
        WM_LBUTTONDOWN => {
            let _ = PICKER.with(|s| s.try_borrow_mut().ok()?.as_mut().map(|p| p.pressed = true));
            // SAFETY: our own window, on its thread.
            unsafe { SetCapture(hwnd) };
        }
        WM_LBUTTONUP => {
            let pressed = PICKER.with(|s| s.try_borrow_mut().ok()?.as_mut().map(|p| std::mem::take(&mut p.pressed)));
            // SAFETY: releases only this thread's capture.
            let _ = unsafe { ReleaseCapture() };
            if pressed == Some(true) {
                feed(hwnd, Input::Click(client_point(lparam)));
            }
        }
        WM_RBUTTONUP => feed(hwnd, Input::RightClick),
        WM_KEYDOWN if wparam.0 == usize::from(VK_ESCAPE.0) => feed(hwnd, Input::Escape),
        // Alt+F4 and the like.
        WM_CLOSE => feed(hwnd, Input::Escape),
        WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => feed(hwnd, Input::FocusLost),
        WM_KILLFOCUS => feed(hwnd, Input::FocusLost),
        WM_TIMER if wparam.0 == FOCUS_TIMER => {
            // SAFETY: our own timer and window.
            unsafe {
                let _ = KillTimer(Some(hwnd), FOCUS_TIMER);
                if GetForegroundWindow() != hwnd {
                    feed(hwnd, Input::NoFocus);
                }
            }
        }
        WM_TIMER if wparam.0 == MAX_TIMER => feed(hwnd, Input::Timeout),
        WM_MOUSEMOVE => move_to(hwnd, client_point(lparam)),
        WM_SETCURSOR => {
            // SAFETY: a system cursor; nothing to free.
            unsafe { SetCursor(LoadCursorW(None, IDC_CROSS).ok()) };
            return Some(LRESULT(1));
        }
        WM_ERASEBKGND => return Some(LRESULT(1)),
        WM_PAINT => paint(hwnd),
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
        }
        _ => return None,
    }
    Some(LRESULT(0))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match catch_unwind(AssertUnwindSafe(|| on_message(hwnd, msg, wparam, lparam))) {
        Ok(Some(r)) => r,
        // SAFETY: the arguments are the ones Windows passed in.
        Ok(None) => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        // Any panic closes the picker.
        Err(_) => {
            destroy(hwnd);
            LRESULT(0)
        }
    }
}

/// This module's HINSTANCE (the linker's `__ImageBase`).
fn instance() -> HINSTANCE {
    extern "C" {
        static __ImageBase: u8;
    }
    // Only the address is taken, never the value.
    HINSTANCE(std::ptr::addr_of!(__ImageBase) as *mut c_void)
}

/// Captures, shows and runs the picker until it closes. On this thread only.
fn run(screen: &dyn Screen, clipboard: Arc<dyn Clipboard>) -> Result<(), String> {
    const FAILED: &str = "could not open the colour picker";
    let (rect, frame) = prepare(screen)?;
    let (width, height) = (rect.width as i32, rect.height as i32);
    let cursor = screen.cursor().and_then(|c| to_frame(rect, c)).map(|(x, y)| (x as i32, y as i32));
    let picture = Canvas::new(width, height)?;
    // SAFETY: `prepare` checked the frame holds exactly `picture.len()` bytes.
    unsafe { std::ptr::copy_nonoverlapping(frame.bgra.as_ptr(), picture.bits, picture.len()) };
    let back = Canvas::new(LOUPE_SIZE.0 as i32, LOUPE_SIZE.1 as i32)?;
    // SAFETY: a plain font; freed by `Picker`'s drop.
    let font = unsafe {
        CreateFontW(
            -16,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        )
    };
    let size = (rect.width, rect.height);
    PICKER.with(|p| {
        *p.borrow_mut() = Some(Picker {
            session: Session::new(frame),
            clipboard,
            size,
            cursor,
            picture,
            back,
            font,
            pressed: false,
        })
    });
    // Dropped last: after the window is gone.
    let _state = Defer(|| drop(PICKER.with(|p| p.borrow_mut().take())));
    let class = w!("FlickColourPicker");
    // SAFETY: window creation and the message loop on this thread; the
    // `Window` guard destroys the window on every way out.
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            ..Default::default()
        };
        // 0 when already registered by an earlier picker; a real failure
        // makes CreateWindowExW fail below.
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!(""),
            WS_POPUP,
            rect.left,
            rect.top,
            width,
            height,
            None,
            None,
            Some(instance()),
            None,
        )
        .map_err(|_| FAILED.to_string())?;
        let _window = Window(hwnd);
        if SetTimer(Some(hwnd), MAX_TIMER, MAX_OPEN.as_millis() as u32, None) == 0
            || SetTimer(Some(hwnd), FOCUS_TIMER, FOCUS_WAIT.as_millis() as u32, None) == 0
        {
            return Err(FAILED.into());
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let mut msg = MSG::default();
        // 0 is WM_QUIT (posted on WM_DESTROY), -1 an error: both end it.
        loop {
            match GetMessageW(&mut msg, None, 0, 0).0 {
                0 => break,
                -1 => {
                    feed(hwnd, Input::Error);
                    break;
                }
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
    Ok(())
}

/// Set while a picker thread runs: one picker at a time.
static OPEN: AtomicBool = AtomicBool::new(false);

/// Opens the colour picker on the monitor under the mouse, on its own
/// thread; returns at once. Does nothing while a picker is already open.
pub fn start(screen: Arc<dyn Screen>, clipboard: Arc<dyn Clipboard>) {
    if OPEN.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("colour picker".into()).spawn(move || {
        let _open = Defer(|| OPEN.store(false, Ordering::SeqCst));
        // SAFETY: affects this thread only, before any other call on it.
        let previous = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        // Without per-monitor awareness the coordinates are scaled and the
        // picked pixel could be the wrong one: then no picker at all.
        if previous.0.is_null() {
            return;
        }
        let _ = catch_unwind(AssertUnwindSafe(|| run(screen.as_ref(), clipboard)));
    });
    if spawned.is_err() {
        OPEN.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const LEFT: Rect = Rect { left: -1920, top: -300, width: 1920, height: 1080 };
    const MAIN: Rect = Rect { left: 0, top: 0, width: 2560, height: 1440 };

    /// A frame whose pixel (x, y) is Rgb(x, y, 7).
    fn frame(width: u32, height: u32) -> Frame {
        let mut bgra = Vec::new();
        for y in 0..height {
            for x in 0..width {
                bgra.extend_from_slice(&[7, y as u8, x as u8, 255]);
            }
        }
        Frame { width, height, bgra }
    }

    #[test]
    fn the_monitor_under_the_mouse_with_negative_coordinates() {
        let ms = [LEFT, MAIN];
        assert_eq!(monitor_at(&ms, (-1, 0)), Some(LEFT));
        assert_eq!(monitor_at(&ms, (0, 0)), Some(MAIN));
        assert_eq!(monitor_at(&ms, (-1920, -300)), Some(LEFT));
        assert_eq!(monitor_at(&ms, (-1921, 0)), None);
        assert_eq!(monitor_at(&ms, (-1, 780)), None, "below the left monitor");
        assert_eq!(monitor_at(&ms, (2559, 1439)), Some(MAIN));
        assert_eq!(monitor_at(&ms, (2560, 0)), None);
        assert_eq!(monitor_at(&[], (0, 0)), None);
    }

    #[test]
    fn frame_mapping_is_one_to_one_in_physical_pixels() {
        assert_eq!(to_frame(LEFT, (-1920, -300)), Some((0, 0)));
        assert_eq!(to_frame(LEFT, (-1, 779)), Some((1919, 1079)));
        assert_eq!(to_frame(LEFT, (0, 0)), None);
        assert_eq!(to_frame(LEFT, (-1921, -300)), None);
        assert_eq!(to_frame(LEFT, (-1920, 780)), None);
        assert_eq!(to_frame(MAIN, (100, 200)), Some((100, 200)));
    }

    #[test]
    fn hex_is_upper_case() {
        assert_eq!(hex(Rgb(255, 10, 171)), "#FF0AAB");
        assert_eq!(hex(Rgb(0, 0, 0)), "#000000");
    }

    #[test]
    fn pixel_reads_bgra() {
        let f = Frame { width: 1, height: 1, bgra: vec![3, 2, 1, 255] };
        assert_eq!(f.pixel(0, 0), Some(Rgb(1, 2, 3)));
        assert_eq!(f.pixel(1, 0), None);
        assert_eq!(f.pixel(0, 1), None);
        let f = frame(4, 3);
        assert_eq!(f.pixel(3, 2), Some(Rgb(3, 2, 7)));
        let short = Frame { width: 2, height: 2, bgra: vec![0; 8] };
        assert_eq!(short.pixel(1, 1), None, "a short buffer never panics");
    }

    #[test]
    fn loupe_is_11_by_11_and_clips_at_edges() {
        let f = frame(20, 20);
        let cells = loupe(&f, (0, 0));
        assert_eq!(cells.len(), (LOUPE_CELLS * LOUPE_CELLS) as usize);
        assert!(cells[..5 * 11].iter().all(|c| c.is_none()), "first 5 rows are above the frame");
        for row in 5..11 {
            assert!(cells[row * 11..row * 11 + 5].iter().all(|c| c.is_none()), "left of the frame");
        }
        assert_eq!(cells[60], Some(Rgb(0, 0, 7)));
        assert_eq!(cells[120], Some(Rgb(5, 5, 7)));
        let mid = loupe(&f, (10, 10));
        assert!(mid.iter().all(|c| c.is_some()));
        assert_eq!(mid[0], Some(Rgb(5, 5, 7)));
        assert_eq!(mid[60], Some(Rgb(10, 10, 7)));
        let corner = loupe(&f, (19, 19));
        assert_eq!(corner[60], Some(Rgb(19, 19, 7)));
        assert_eq!(corner[61], None);
        assert_eq!(corner[71], None);
    }

    fn inside(origin: (i32, i32), size: (u32, u32), area: (u32, u32)) -> bool {
        origin.0 >= 0
            && origin.1 >= 0
            && origin.0 as u32 + size.0 <= area.0
            && origin.1 as u32 + size.1 <= area.1
    }

    #[test]
    fn loupe_stays_on_screen() {
        let size = (112, 136);
        let area = (1920, 1080);
        assert_eq!(loupe_origin((100, 100), size, area), (120, 120), "20 px below-right");
        let o = loupe_origin((1910, 1070), size, area);
        assert_eq!(o, (1910 - 20 - 112, 1070 - 20 - 136), "flipped up-left at the corner");
        assert!(inside(o, size, area));
        assert_eq!(loupe_origin((1910, 100), size, area), (1910 - 20 - 112, 120));
        assert_eq!(loupe_origin((100, 1070), size, area), (120, 1070 - 20 - 136));
        for cursor in [(0, 0), (-50, -50), (5000, 5000), (1919, 0), (0, 1079), (60, 60), (1850, 1000)] {
            let o = loupe_origin(cursor, size, area);
            assert!(inside(o, size, area), "{cursor:?} -> {o:?}");
        }
        assert_eq!(loupe_origin((10, 10), size, (50, 50)), (0, 0), "a too-small area pins it to the corner");
    }

    #[test]
    fn every_exit_closes_and_only_one_pick_copies() {
        let mut s = Session::new(frame(20, 20));
        assert_eq!(s.handle(Input::Click((3, 4))), Step::Copy("#030407".into()));
        assert_eq!(s.handle(Input::Click((3, 4))), Step::Ignore);
        assert_eq!(s.handle(Input::Escape), Step::Ignore);
        type Make = fn() -> Input;
        let exits: [Make; 6] = [
            || Input::RightClick,
            || Input::Escape,
            || Input::FocusLost,
            || Input::Timeout,
            || Input::Error,
            || Input::Click((20, 0)),
        ];
        for exit in exits {
            let mut s = Session::new(frame(20, 20));
            assert_eq!(s.handle(exit()), Step::Close);
            assert_eq!(s.handle(Input::Click((1, 1))), Step::Ignore);
            assert_eq!(s.handle(Input::Escape), Step::Ignore);
        }
        let mut s = Session::new(frame(20, 20));
        assert_eq!(s.handle(Input::Click((-1, 0))), Step::Close, "outside the frame closes");
    }

    #[test]
    fn no_focus_closes_the_picker() {
        let mut s = Session::new(frame(2, 2));
        assert_eq!(s.handle(Input::NoFocus), Step::Close);
        assert_eq!(s.handle(Input::Click((0, 0))), Step::Ignore);
        assert_eq!(FOCUS_WAIT, Duration::from_millis(500));
        assert_eq!(MAX_OPEN, Duration::from_secs(60));
    }

    struct FakeScreen {
        monitors: Vec<Rect>,
        cursor: Option<(i32, i32)>,
        /// Frame size returned instead of the rect's, if set.
        size: Option<(u32, u32)>,
        captures: Mutex<Vec<Rect>>,
    }

    impl Screen for FakeScreen {
        fn monitors(&self) -> Vec<Rect> {
            self.monitors.clone()
        }
        fn cursor(&self) -> Option<(i32, i32)> {
            self.cursor
        }
        fn capture(&self, r: Rect) -> Result<Frame, String> {
            self.captures.lock().unwrap().push(r);
            let (w, h) = self.size.unwrap_or((r.width, r.height));
            Ok(Frame { width: w, height: h, bgra: vec![0; (w * h * 4) as usize] })
        }
    }

    fn screen(cursor: Option<(i32, i32)>, size: Option<(u32, u32)>) -> FakeScreen {
        let small = |r: Rect| Rect { width: r.width / 64, height: r.height / 64, ..r };
        FakeScreen { monitors: vec![small(LEFT), small(MAIN)], cursor, size, captures: Mutex::new(Vec::new()) }
    }

    #[test]
    fn prepare_captures_only_the_monitor_under_the_mouse() {
        let s = screen(Some((-1900, -290)), None);
        let (rect, f) = prepare(&s).unwrap();
        let want = Rect { left: -1920, top: -300, width: 30, height: 16 };
        assert_eq!(rect, want);
        assert_eq!((f.width, f.height), (30, 16));
        assert_eq!(*s.captures.lock().unwrap(), vec![want]);

        let s = screen(None, None);
        assert!(prepare(&s).is_err());
        assert!(s.captures.lock().unwrap().is_empty());

        let s = screen(Some((5000, 5000)), None);
        assert!(prepare(&s).is_err());
        assert!(s.captures.lock().unwrap().is_empty());

        let s = screen(Some((1, 1)), Some((10, 10)));
        assert!(prepare(&s).is_err(), "a frame of the wrong size");
        assert_eq!(s.captures.lock().unwrap().len(), 1);
    }
}
