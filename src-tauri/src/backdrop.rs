//! The Glass theme's backdrop: a picture of the screen where the bar is about
//! to appear, taken just before it is shown. The bar page draws it behind its
//! panel (blurred a little, bent at the edges), which is what makes the panel
//! look like glass.
//!
//! The picture is only ever the bar window's own rectangle, lives in memory
//! while the bar is open, and is dropped when the bar hides. It is never
//! written to disk or sent anywhere. Nothing is captured under other themes.
//!
//! Served to the page on the `flick-backdrop` scheme as a BMP: no encoding
//! work, so taking it costs a few milliseconds.
//!
//! Tests cover the BMP layout only; they never read the screen.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tauri::http::{header, Response};

/// Scheme name registered with Tauri.
pub const SCHEME: &str = "flick-backdrop";

static CURRENT: Mutex<Option<Arc<Vec<u8>>>> = Mutex::new(None);
/// Counts the pictures taken, so each has an address of its own and the page
/// never shows a cached older one.
static TAKEN: AtomicU64 = AtomicU64::new(0);

/// What the page is told about a picture. `id` 0 means there is none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Taken {
    /// The picture's number: its address on the scheme.
    pub id: u64,
    /// Its average brightness, 0 (black) to 255 (white): the page writes in
    /// dark ink on a light picture and in white on a dark one.
    pub brightness: u8,
}

/// Takes the picture of the screen rectangle at (`x`, `y`), `width` by
/// `height` physical pixels, and keeps it as the current backdrop. `None`
/// when the screen could not be read (the page then shows its plain tint).
pub fn take(x: i32, y: i32, width: u32, height: u32) -> Option<Taken> {
    let pixels = screen::read(x, y, width, height)?;
    let bmp = bmp_from_bgra(width, height, &pixels)?;
    let brightness = mean_brightness(&pixels);
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(bmp));
    Some(Taken { id: TAKEN.fetch_add(1, Ordering::SeqCst) + 1, brightness })
}

/// The average brightness of BGRA pixels, 0 to 255, from every 16th pixel
/// (plenty for "light or dark", and quick). Empty input counts as black.
fn mean_brightness(pixels: &[u8]) -> u8 {
    let (mut sum, mut count) = (0u64, 0u64);
    for px in pixels.chunks_exact(4).step_by(16) {
        // Rec. 601 luma, in thousandths.
        sum += 114 * u64::from(px[0]) + 587 * u64::from(px[1]) + 299 * u64::from(px[2]);
        count += 1;
    }
    if count == 0 {
        0
    } else {
        (sum / (count * 1000)) as u8
    }
}

/// Drops the picture: the bar has hidden.
pub fn clear() {
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Answers a request on the scheme: the current picture, or 404 when there
/// is none.
pub fn response() -> Response<Vec<u8>> {
    let current = CURRENT.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let (status, body) = match current {
        Some(bmp) => (200, bmp.as_ref().clone()),
        None => (404, Vec::new()),
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "image/bmp")
        .header(header::CACHE_CONTROL, "no-store")
        .body(body)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

const FILE_HEADER: usize = 14;
const INFO_HEADER: usize = 40;

/// A 24-bit BMP of `width` x `height` pixels from top-down BGRA rows (alpha
/// ignored). `None` when the size is zero or `pixels` is too short.
fn bmp_from_bgra(width: u32, height: u32, pixels: &[u8]) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || pixels.len() < w.checked_mul(h)?.checked_mul(4)? {
        return None;
    }
    // Rows are padded to four bytes and stored bottom row first.
    let row = (w * 3).div_ceil(4) * 4;
    let size = FILE_HEADER + INFO_HEADER + row * h;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&((FILE_HEADER + INFO_HEADER) as u32).to_le_bytes());
    out.extend_from_slice(&(INFO_HEADER as u32).to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for y in (0..h).rev() {
        let line = &pixels[y * w * 4..(y + 1) * w * 4];
        for px in line.chunks_exact(4) {
            out.extend_from_slice(&px[..3]);
        }
        out.resize(out.len() + row - w * 3, 0);
    }
    Some(out)
}

mod screen {
    use std::ffi::c_void;

    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC, ReleaseDC,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
    };

    /// The screen's pixels in the given rectangle as top-down BGRA; `None`
    /// when any step fails.
    pub(super) fn read(x: i32, y: i32, width: u32, height: u32) -> Option<Vec<u8>> {
        let (w, h) = (i32::try_from(width).ok()?, i32::try_from(height).ok()?);
        if w == 0 || h == 0 {
            return None;
        }
        let len = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
        // SAFETY: plain GDI calls; every handle made here is released before
        // returning, and the pixel memory is copied while the section lives.
        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                return None;
            }
            let memory = CreateCompatibleDC(Some(screen));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    // Negative: top row first.
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let section = CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0);
            let mut pixels = None;
            if let Ok(section) = section {
                if !bits.is_null() {
                    let old = SelectObject(memory, HGDIOBJ(section.0));
                    if BitBlt(memory, 0, 0, w, h, Some(screen), x, y, SRCCOPY).is_ok() {
                        let _ = GdiFlush();
                        pixels = Some(std::slice::from_raw_parts(bits as *const u8, len).to_vec());
                    }
                    SelectObject(memory, old);
                }
                let _ = DeleteObject(HGDIOBJ(section.0));
            }
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);
            pixels
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bmp_holds_the_rows_bottom_first_without_alpha() {
        // 2 x 2: top row blue, green; bottom row red, white (BGRA).
        let pixels = [255, 0, 0, 9, 0, 255, 0, 9, 0, 0, 255, 9, 255, 255, 255, 9];
        let bmp = bmp_from_bgra(2, 2, &pixels).unwrap();
        // Each row: 2 pixels of 3 bytes, padded to 8.
        assert_eq!(bmp.len(), 54 + 16);
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()), 70);
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 54);
        assert_eq!(i32::from_le_bytes(bmp[18..22].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(bmp[22..26].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(bmp[28..30].try_into().unwrap()), 24);
        // The bottom row comes first.
        assert_eq!(&bmp[54..62], &[0, 0, 255, 255, 255, 255, 0, 0]);
        assert_eq!(&bmp[62..70], &[255, 0, 0, 0, 255, 0, 0, 0]);
    }

    #[test]
    fn brightness_is_the_average_luma() {
        let fill = |b: u8, g: u8, r: u8| [b, g, r, 0].repeat(64);
        assert_eq!(mean_brightness(&fill(0, 0, 0)), 0);
        assert_eq!(mean_brightness(&fill(255, 255, 255)), 255);
        assert_eq!(mean_brightness(&fill(128, 128, 128)), 128);
        // Green counts most, blue least.
        assert_eq!(mean_brightness(&fill(0, 255, 0)), 149);
        assert_eq!(mean_brightness(&fill(255, 0, 0)), 29);
        assert_eq!(mean_brightness(&[]), 0);
    }

    #[test]
    fn rows_are_padded_to_four_bytes() {
        for (width, row) in [(1u32, 4usize), (3, 12), (4, 12), (5, 16)] {
            let bmp = bmp_from_bgra(width, 1, &vec![7; width as usize * 4]).unwrap();
            assert_eq!(bmp.len(), 54 + row, "{width}");
        }
    }

    #[test]
    fn an_empty_or_short_picture_is_refused() {
        assert_eq!(bmp_from_bgra(0, 4, &[]), None);
        assert_eq!(bmp_from_bgra(4, 0, &[]), None);
        assert_eq!(bmp_from_bgra(2, 2, &[0; 15]), None);
    }

    #[test]
    fn no_picture_answers_404_and_clear_forgets_it() {
        clear();
        assert_eq!(response().status(), 404);
        *CURRENT.lock().unwrap() = Some(Arc::new(vec![1, 2, 3]));
        let r = response();
        assert_eq!((r.status().as_u16(), r.body().as_slice()), (200, [1u8, 2, 3].as_slice()));
        assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
        clear();
        assert_eq!(response().status(), 404);
    }
}
