//! Result-row icons, served to the webview on the `flick-icon` scheme.
//!
//! The UI asks for `flick-icon://localhost/<percent-encoded key>?px=<n>`, which
//! WebView2 on Windows loads as `http://flick-icon.localhost/<key>?px=<n>`.
//! Every request is answered on a worker thread: reading an icon never runs on
//! the main thread and never touches the query path.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use tauri::http::{header, Response};

/// Scheme name registered with Tauri.
pub const SCHEME: &str = "flick-icon";
/// Cache entries kept before the whole cache is dropped.
const CACHE_MAX: usize = 512;
const PX_MIN: u32 = 16;
const PX_MAX: u32 = 64;
const PX_DEFAULT: u32 = 32;
/// Worker threads answering icon requests: one slow file does not hold up the rest.
const WORKERS: usize = 2;

#[derive(Clone, Debug, PartialEq)]
pub enum IconKey {
    File(PathBuf),
    /// Lower-case, with the leading dot: `.pdf`.
    Ext(String),
    Folder,
}

/// Reads a row's icon key. `builtin:*` keys are drawn by the UI itself: `None`.
pub fn parse_key(s: &str) -> Option<IconKey> {
    if s == "folder" {
        return Some(IconKey::Folder);
    }
    if let Some(path) = s.strip_prefix("file:") {
        return (!path.is_empty()).then(|| IconKey::File(PathBuf::from(path)));
    }
    if let Some(ext) = s.strip_prefix("ext:") {
        let ok = ext.len() > 1 && ext.starts_with('.') && !ext[1..].contains(['.', '/', '\\', ':']);
        return ok.then(|| IconKey::Ext(ext.to_lowercase()));
    }
    None
}

pub trait IconSource: Send + Sync {
    fn png(&self, key: &IconKey, px: u32) -> Option<Vec<u8>>;
}

/// PNGs already made, by key and size. Holds at most `CACHE_MAX` entries;
/// one past that drops them all. Extension and folder icons are shared by
/// every row with that key.
#[derive(Default)]
pub struct IconCache {
    map: Mutex<HashMap<String, Arc<Vec<u8>>>>,
}

impl IconCache {
    fn get(&self, k: &str) -> Option<Arc<Vec<u8>>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).get(k).cloned()
    }

    fn put(&self, k: String, png: Arc<Vec<u8>>) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= CACHE_MAX && !map.contains_key(&k) {
            map.clear();
        }
        map.insert(k, png);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.lock().unwrap().len()
    }
}

fn cache_key(key: &IconKey, px: u32) -> String {
    match key {
        IconKey::File(p) => format!("file:{}@{px}", p.display()),
        IconKey::Ext(e) => format!("ext:{e}@{px}"),
        IconKey::Folder => format!("folder@{px}"),
    }
}

/// Answers one request: `(200, png)` or `(404, empty)`. May read the disk:
/// call on a worker thread only.
pub fn serve(cache: &IconCache, source: &dyn IconSource, key: &str, px: u32) -> (u16, Vec<u8>) {
    let Some(key) = parse_key(key) else {
        return (404, Vec::new());
    };
    let px = px.clamp(PX_MIN, PX_MAX);
    let ck = cache_key(&key, px);
    if let Some(png) = cache.get(&ck) {
        return (200, png.as_ref().clone());
    }
    match source.png(&key, px) {
        Some(png) => {
            let png = Arc::new(png);
            cache.put(ck, png.clone());
            (200, png.as_ref().clone())
        }
        None => (404, Vec::new()),
    }
}

/// Reads the key and size from a request's path (`/<percent-encoded key>`)
/// and query (`px=<n>`). The size is clamped to 16..=64, 32 when missing.
pub fn parse_request(path: &str, query: Option<&str>) -> (String, u32) {
    let key = percent_decode(path.strip_prefix('/').unwrap_or(path));
    let px = query
        .into_iter()
        .flat_map(|q| q.split('&'))
        .find_map(|pair| pair.strip_prefix("px="))
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(PX_DEFAULT)
        .clamp(PX_MIN, PX_MAX);
    (key, px)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// The HTTP response for a served request.
pub fn response(status: u16, body: Vec<u8>) -> Response<Vec<u8>> {
    let builder = Response::builder().status(status);
    let builder = if status == 200 {
        builder.header(header::CONTENT_TYPE, "image/png").header(header::CACHE_CONTROL, "max-age=86400")
    } else {
        builder
    };
    builder.body(body).unwrap_or_else(|_| Response::new(Vec::new()))
}

type Job = Box<dyn FnOnce() + Send>;

/// Cache, source and the worker threads that answer requests.
pub struct IconServer {
    cache: Arc<IconCache>,
    source: Arc<dyn IconSource>,
    jobs: Mutex<mpsc::Sender<Job>>,
}

impl IconServer {
    /// Starts the worker threads (each with COM initialised for its lifetime).
    pub fn new(source: Arc<dyn IconSource>) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..WORKERS {
            let rx = rx.clone();
            let _ = std::thread::Builder::new().name(format!("flick-icon-{i}")).spawn(move || {
                let _com = shell::Com::init();
                loop {
                    // The guard is dropped before the job runs.
                    let job = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                    match job {
                        // A panicking job must not take the worker with it.
                        Ok(job) => drop(catch_unwind(AssertUnwindSafe(job))),
                        Err(_) => return,
                    }
                }
            });
        }
        Self { cache: Arc::new(IconCache::default()), source, jobs: Mutex::new(tx) }
    }

    /// Queues a request; `reply` is called on a worker thread with the status
    /// and body. Never blocks on the source.
    pub fn request(&self, path: &str, query: Option<&str>, reply: impl FnOnce(u16, Vec<u8>) + Send + 'static) {
        let (key, px) = parse_request(path, query);
        let cache = self.cache.clone();
        let source = self.source.clone();
        let job: Job = Box::new(move || {
            let (status, body) =
                catch_unwind(AssertUnwindSafe(|| serve(&cache, source.as_ref(), &key, px))).unwrap_or((500, Vec::new()));
            reply(status, body);
        });
        // Fails only with no worker left; workers survive panicking jobs.
        let _ = self.jobs.lock().unwrap_or_else(|e| e.into_inner()).send(job);
    }
}

/// Straight RGBA from the same icon drawn over black and over white (BGRA).
/// What shows through is transparency: this recovers alpha for icons with a
/// real alpha channel and for old icons that only have a mask.
fn rgba_from_draws(black: &[u8], white: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(black.len());
    for (b, w) in black.chunks_exact(4).zip(white.chunks_exact(4)) {
        let through = (0..3).map(|c| w[c].saturating_sub(b[c])).max().unwrap_or(0);
        let a = 255 - through as u32;
        if a == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        // Over black the colour is premultiplied by alpha: undo that.
        let un = |v: u8| ((v as u32 * 255 + a / 2) / a).min(255) as u8;
        out.extend_from_slice(&[un(b[2]), un(b[1]), un(b[0]), a as u8]);
    }
    out
}

fn encode_png(rgba: &[u8], px: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, px, px);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
        writer.finish().ok()?;
    }
    Some(out)
}

/// Real icons from Windows Shell. `Ext` and `Folder` keys touch no file.
pub struct ShellIcons;

impl IconSource for ShellIcons {
    fn png(&self, key: &IconKey, px: u32) -> Option<Vec<u8>> {
        let _com = shell::Com::init();
        let icon = shell::icon(key)?;
        let drawn = shell::draw(icon, px, 0x00).zip(shell::draw(icon, px, 0xFF));
        shell::destroy(icon);
        let (black, white) = drawn?;
        encode_png(&rgba_from_draws(&black, &white), px)
    }
}

mod shell {
    use std::ffi::{c_void, OsString};
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES};
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
    use windows::Win32::UI::Shell::{
        SHGetFileInfoW, SHFILEINFOW, SHGFI_FLAGS, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES,
    };
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, DrawIconEx, DI_NORMAL, HICON};

    use super::IconKey;

    /// COM for the current thread, released on drop when this call took it.
    pub(super) struct Com(bool);

    impl Com {
        pub(super) fn init() -> Self {
            // S_FALSE (already initialised) is ok too and must be balanced.
            Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_ok())
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    /// The Shell's large icon for `key`; the caller destroys it.
    pub(super) fn icon(key: &IconKey) -> Option<HICON> {
        let (name, attrs, extra): (OsString, FILE_FLAGS_AND_ATTRIBUTES, SHGFI_FLAGS) = match key {
            IconKey::File(p) => (p.as_os_str().to_owned(), FILE_FLAGS_AND_ATTRIBUTES(0), SHGFI_FLAGS(0)),
            // Attributes only: no file is looked at.
            IconKey::Ext(e) => (OsString::from(e), FILE_ATTRIBUTE_NORMAL, SHGFI_USEFILEATTRIBUTES),
            IconKey::Folder => (OsString::from("folder"), FILE_ATTRIBUTE_DIRECTORY, SHGFI_USEFILEATTRIBUTES),
        };
        let wide: Vec<u16> = name.encode_wide().chain(std::iter::once(0)).collect();
        let mut info = SHFILEINFOW::default();
        let found = unsafe {
            SHGetFileInfoW(
                PCWSTR(wide.as_ptr()),
                attrs,
                Some(&mut info),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON | extra,
            )
        };
        (found != 0 && !info.hIcon.is_invalid()).then_some(info.hIcon)
    }

    pub(super) fn destroy(icon: HICON) {
        let _ = unsafe { DestroyIcon(icon) };
    }

    /// `icon` drawn at `px`×`px` over a background of `fill` bytes, as
    /// top-down BGRA.
    pub(super) fn draw(icon: HICON, px: u32, fill: u8) -> Option<Vec<u8>> {
        let side = px as i32;
        let len = (px * px * 4) as usize;
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return None;
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: side,
                    biHeight: -side, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let out = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bmp) if !bits.is_null() => {
                    let old = SelectObject(dc, HGDIOBJ(bmp.0));
                    let buf = std::slice::from_raw_parts_mut(bits as *mut u8, len);
                    buf.fill(fill);
                    let drawn = DrawIconEx(dc, 0, 0, icon, side, side, 0, None, DI_NORMAL).is_ok();
                    let _ = GdiFlush();
                    let copy = drawn.then(|| buf.to_vec());
                    SelectObject(dc, old);
                    let _ = DeleteObject(HGDIOBJ(bmp.0));
                    copy
                }
                Ok(bmp) => {
                    let _ = DeleteObject(HGDIOBJ(bmp.0));
                    None
                }
                Err(_) => None,
            };
            let _ = DeleteDC(dc);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::provider::{Modifier, Outcome, Provider, Query, ResultItem};
    use crate::ranking::Ranking;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// Counts calls; answers a tiny fake PNG (or nothing) after `delay`.
    struct FakeSource {
        calls: AtomicUsize,
        delay: Duration,
        answer: bool,
        started: Mutex<Option<mpsc::Sender<()>>>,
    }

    impl FakeSource {
        fn new(answer: bool, delay: Duration) -> Self {
            Self { calls: AtomicUsize::new(0), delay, answer, started: Mutex::new(None) }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl IconSource for FakeSource {
        fn png(&self, _key: &IconKey, _px: u32) -> Option<Vec<u8>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(tx) = self.started.lock().unwrap().take() {
                let _ = tx.send(());
            }
            std::thread::sleep(self.delay);
            self.answer.then(|| b"\x89PNG fake".to_vec())
        }
    }

    #[test]
    fn parse_key_reads_file_ext_and_folder_keys() {
        assert_eq!(parse_key(r"file:C:\x\a.lnk"), Some(IconKey::File(PathBuf::from(r"C:\x\a.lnk"))));
        assert_eq!(parse_key("ext:.PDF"), Some(IconKey::Ext(".pdf".into())));
        assert_eq!(parse_key("folder"), Some(IconKey::Folder));
        assert_eq!(parse_key("builtin:app"), None);
        assert_eq!(parse_key("garbage"), None);
        assert_eq!(parse_key("file:"), None);
        assert_eq!(parse_key("ext:pdf"), None);
    }

    #[test]
    fn cache_serves_repeat_requests_without_the_source() {
        let cache = IconCache::default();
        let source = FakeSource::new(true, Duration::ZERO);
        let first = serve(&cache, &source, "ext:.pdf", 32);
        let second = serve(&cache, &source, "ext:.pdf", 32);
        assert_eq!(first.0, 200);
        assert_eq!(first, second);
        assert_eq!(source.calls(), 1);
        // Same extension, any case: the same entry.
        serve(&cache, &source, "ext:.PDF", 32);
        assert_eq!(source.calls(), 1);
    }

    #[test]
    fn cache_drops_everything_past_512_entries() {
        let cache = IconCache::default();
        let source = FakeSource::new(true, Duration::ZERO);
        for i in 0..512 {
            serve(&cache, &source, &format!("ext:.e{i}"), 32);
        }
        assert_eq!(cache.len(), 512);
        serve(&cache, &source, "ext:.e0", 32);
        assert_eq!(source.calls(), 512, "still cached");
        serve(&cache, &source, "ext:.new", 32);
        assert_eq!(cache.len(), 1);
        serve(&cache, &source, "ext:.e0", 32);
        assert_eq!(source.calls(), 514, "dropped with the rest");
    }

    #[test]
    fn missing_icon_answers_404() {
        let cache = IconCache::default();
        let source = FakeSource::new(false, Duration::ZERO);
        assert_eq!(serve(&cache, &source, "ext:.zzz", 32).0, 404);
        assert_eq!(serve(&cache, &source, "builtin:app", 32).0, 404);
        assert_eq!(serve(&cache, &source, "garbage", 32).0, 404);
    }

    #[test]
    fn requests_read_percent_encoded_keys_and_clamp_px() {
        assert_eq!(
            parse_request("/file%3AC%3A%5Cx%5Ca%20b.lnk", Some("px=48")),
            (r"file:C:\x\a b.lnk".to_string(), 48)
        );
        assert_eq!(parse_request("/folder", None), ("folder".to_string(), 32));
        assert_eq!(parse_request("/folder", Some("px=2")).1, 16);
        assert_eq!(parse_request("/folder", Some("px=500")).1, 64);
        assert_eq!(parse_request("/folder", Some("px=abc")).1, 32);
        assert_eq!(parse_request("/%E2%82%AC", None).0, "\u{20ac}");
    }

    #[test]
    fn alpha_is_recovered_from_black_and_white_draws() {
        // Opaque red, transparent, half-transparent white (BGRA).
        let black = [0, 0, 255, 0, 0, 0, 0, 0, 128, 128, 128, 0];
        let white = [0, 0, 255, 0, 255, 255, 255, 0, 255, 255, 255, 0];
        let rgba = rgba_from_draws(&black, &white);
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(rgba[7], 0);
        assert_eq!(rgba[11], 128);
        assert!(rgba[8] >= 254);
    }

    #[test]
    fn ok_responses_are_cacheable_pngs() {
        let ok = response(200, b"\x89PNG".to_vec());
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(ok.headers()[header::CACHE_CONTROL], "max-age=86400");
        let missing = response(404, Vec::new());
        assert_eq!(missing.status(), 404);
        assert!(missing.headers().get(header::CACHE_CONTROL).is_none());
    }

    struct Quiet;
    impl Provider for Quiet {
        fn name(&self) -> &'static str {
            "quiet"
        }
        fn query(&self, _q: &Query) -> Vec<ResultItem> {
            vec![]
        }
        fn activate(&self, _id: &str, _m: Modifier) -> Result<Outcome, String> {
            Ok(Outcome::Hide)
        }
    }

    #[test]
    fn slow_icons_do_not_block_queries() {
        let dir = tempfile::tempdir().unwrap();
        let ranking = Arc::new(Ranking::load(dir.path().join("picks.json")));
        let history = Arc::new(crate::history::History::load(dir.path().join("history.json"), true));
        let pins = Arc::new(crate::pins::Pins::load(dir.path().join("pins.json")));
        let engine = Engine::new(vec![Box::new(Quiet)], ranking, history, pins);

        let source = Arc::new(FakeSource::new(true, Duration::from_millis(200)));
        let (started_tx, started_rx) = mpsc::channel();
        *source.started.lock().unwrap() = Some(started_tx);
        let server = IconServer::new(source.clone());
        let (done_tx, done_rx) = mpsc::channel();
        server.request("/ext%3A.pdf", Some("px=32"), move |status, _| {
            let _ = done_tx.send(status);
        });
        started_rx.recv_timeout(Duration::from_secs(5)).expect("icon request started");

        let t = Instant::now();
        engine.query("anything", None);
        assert!(t.elapsed() < Duration::from_millis(30), "query took {:?}", t.elapsed());
        assert!(done_rx.try_recv().is_err(), "the slow icon is still being read");

        assert_eq!(done_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 200);
    }

    /// Manual probe: reads notepad's icon from Windows Shell (starts nothing).
    #[test]
    #[ignore]
    fn shell_icon_for_a_known_system_file() {
        let root = std::env::var_os("SystemRoot").expect("SystemRoot");
        let notepad = PathBuf::from(root).join(r"System32\notepad.exe");
        let png = ShellIcons.png(&IconKey::File(notepad), 32).expect("an icon");
        assert!(png.starts_with(b"\x89PNG"));
        // The alpha path on a real Shell icon: 32x32, with see-through pixels
        // and opaque pixels that are not black.
        let mut reader = png::Decoder::new(std::io::Cursor::new(&png)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (32, 32));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        let pixels: Vec<&[u8]> = buf[..info.buffer_size()].chunks_exact(4).collect();
        assert!(pixels.iter().any(|p| p[3] == 0), "some transparent pixels");
        assert!(
            pixels.iter().any(|p| p[3] == 255 && p[..3].iter().map(|&c| c as u32).sum::<u32>() > 0),
            "some opaque, non-black pixels"
        );
        // Extension and folder keys touch no file.
        assert!(ShellIcons.png(&IconKey::Ext(".txt".into()), 48).unwrap().starts_with(b"\x89PNG"));
        assert!(ShellIcons.png(&IconKey::Folder, 16).unwrap().starts_with(b"\x89PNG"));
    }
}
