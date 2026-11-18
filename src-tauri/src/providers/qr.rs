//! `qr`: the clipboard (or the typed text) as a QR code the bar draws as a panel.
//! The clipboard is read on a [`Loader`] thread; nothing is stored or logged.

use std::path::PathBuf;
use std::sync::Arc;

use qrcodegen::{QrCode, QrCodeEcc};

use crate::clipboard::{Clipboard, ClipboardText};
use crate::dirs::{one_line, Load, Loader, ReadError, NEVER_STALE, SLOW};
use crate::notifications::{Notifier, TITLE};
use crate::provider::{Action, HelpLine, Modifier, Outcome, Provider, Query, ResultItem, Route, Then};

/// `qr` / `qr some text`.
pub const ROUTE: Route = Route { prefix: "qr", needs_space: true };

/// Characters of the text shown as the row title.
pub const MAX_PREVIEW: usize = 80;

const READING: &str = "Reading clipboard\u{2026}";
const TOO_LONG: &str = "Too long for a QR code";
const NOTHING: &str = "Nothing to show \u{2014} copy some text first";
const CANT_READ: &str = "Can't read the clipboard";

/// Blank modules around the code.
const QUIET: i32 = 4;

/// The file name the Save window starts with (never derived from the text).
pub const SUGGESTED_NAME: &str = "qr-code.png";

/// Pixels a saved picture is at least wide.
const PNG_MIN: i32 = 1024;

const COULD_NOT_SAVE: &str = "Could not save the QR code";

/// The code for `text` and the side of the drawing in modules (quiet zone
/// included), shared by the SVG and the PNG. `None` when it does not fit.
fn layout(text: &str) -> Option<(QrCode, i32)> {
    let code = QrCode::encode_text(text, QrCodeEcc::Low).ok()?;
    let side = code.size() + 2 * QUIET;
    Some((code, side))
}

/// `text` as SVG: a white square and one black cell per dark module, with a
/// quiet zone. `None` when the text does not fit a code. Only module
/// positions reach the markup, never the text.
pub fn qr_svg(text: &str) -> Option<String> {
    let (code, side) = layout(text)?;
    let n = code.size();
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {side} {side}\" shape-rendering=\"crispEdges\">\
         <rect width=\"{side}\" height=\"{side}\" fill=\"#fff\"/><path fill=\"#000\" d=\""
    );
    for y in 0..n {
        for x in 0..n {
            if code.get_module(x, y) {
                svg.push_str(&format!("M{},{}h1v1h-1z", x + QUIET, y + QUIET));
            }
        }
    }
    svg.push_str("\"/></svg>");
    Some(svg)
}

/// `text` as a grayscale PNG, black on white, with the same quiet zone as the
/// SVG and at least 1024 pixels a side. `None` when the text does not fit.
pub fn qr_png(text: &str) -> Option<Vec<u8>> {
    let (code, side) = layout(text)?;
    let scale = (PNG_MIN + side - 1) / side;
    let px = (side * scale) as usize;
    let mut pixels = vec![255u8; px * px];
    for y in 0..code.size() {
        for x in 0..code.size() {
            if !code.get_module(x, y) {
                continue;
            }
            for row in (y + QUIET) * scale..(y + QUIET + 1) * scale {
                let start = row as usize * px + ((x + QUIET) * scale) as usize;
                pixels[start..start + scale as usize].fill(0);
            }
        }
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, px as u32, px as u32);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&pixels).ok()?;
    writer.finish().ok()?;
    Some(out)
}

/// Where the picture goes: a path with no extension gets `.png`; one the user
/// typed with another extension stays as typed.
fn with_png(mut path: PathBuf) -> PathBuf {
    if path.extension().is_none() {
        path.set_extension("png");
    }
    path
}

/// Writes `bytes` to `<path>.tmp`, then renames it over `path`, so a failed
/// write never leaves a cut-off file where a good one was. The temp file is
/// removed on failure.
fn write_whole(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let temp = path.with_file_name(name);
    let result = std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Asks where to save the code for `text` (`ask` gets [`SUGGESTED_NAME`]) and
/// writes it; says so when it cannot. Cancelling does nothing. Blocks on `ask`.
pub fn save_qr(text: &str, ask: &dyn Fn(&str) -> Option<PathBuf>, notify: &dyn Notifier) {
    let Some(path) = ask(SUGGESTED_NAME) else { return };
    let path = with_png(path);
    let saved = qr_png(text).is_some_and(|png| write_whole(&path, &png).is_ok());
    if !saved {
        let _ = notify.show(TITLE, COULD_NOT_SAVE);
    }
}

pub struct QrProvider {
    loader: Arc<Loader<Option<String>>>,
    source: Arc<dyn ClipboardText>,
    clipboard: Arc<dyn Clipboard>,
}

impl QrProvider {
    pub fn new(source: Arc<dyn ClipboardText>, clipboard: Arc<dyn Clipboard>, on_ready: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::with_slow(source, clipboard, on_ready, SLOW)
    }

    fn with_slow(
        source: Arc<dyn ClipboardText>,
        clipboard: Arc<dyn Clipboard>,
        on_ready: Arc<dyn Fn() + Send + Sync>,
        slow: std::time::Duration,
    ) -> Self {
        Self { loader: Loader::new(on_ready, NEVER_STALE, slow), source, clipboard }
    }

    /// Drops the clipboard text read, so the next `qr` reads again (the bar hid).
    pub fn forget(&self) {
        self.loader.clear();
    }
}

fn inert(title: &str) -> ResultItem {
    ResultItem {
        id: format!("inert:{title}"),
        provider: "qr",
        title: title.to_string(),
        icon: "builtin:clipboard".into(),
        ..Default::default()
    }
}

fn code_row(text: &str) -> ResultItem {
    match qr_svg(text) {
        Some(svg) => ResultItem {
            id: format!("qr:{text}"),
            provider: "qr",
            title: one_line(text, MAX_PREVIEW),
            icon: "builtin:clipboard".into(),
            panel: svg,
            actions: vec![Action::SavePicture],
            ..Default::default()
        },
        None => inert(TOO_LONG),
    }
}

impl Provider for QrProvider {
    fn name(&self) -> &'static str {
        "qr"
    }

    /// Memory only: the clipboard is read on a loader thread.
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let typed = q.text.trim();
        if !typed.is_empty() {
            return vec![code_row(typed)];
        }
        let source = Arc::clone(&self.source);
        let load = self.loader.get("clipboard", move || source.text().map_err(|_| ReadError::Denied));
        match load {
            Load::Pending => vec![inert(READING)],
            Load::Ready(text) => match text.as_deref() {
                Some(t) if !t.trim().is_empty() => vec![code_row(t)],
                _ => vec![inert(NOTHING)],
            },
            // A busy clipboard: not kept, so the next query reads again.
            Load::NotFound | Load::Denied => {
                self.loader.clear();
                vec![inert(CANT_READ)]
            }
            // Still reading: its answer replaces this once it comes.
            Load::Slow => vec![inert(CANT_READ)],
        }
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        match id.strip_prefix("qr:") {
            Some(text) => {
                self.clipboard.set_text(text)?;
                Ok(Outcome::Hide)
            }
            None => Ok(Outcome::Keep),
        }
    }

    fn act(&self, id: &str, action: Action) -> Result<Outcome, String> {
        match (action, id.strip_prefix("qr:")) {
            (Action::SavePicture, Some(text)) => Ok(Outcome::HideThen { then: Then::SaveQr { text: text.to_string() } }),
            _ => Err("can't do that here".to_string()),
        }
    }

    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn route(&self) -> Option<Route> {
        Some(ROUTE)
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: "qr / qr some text".into(),
            about: "show the clipboard (or the typed text) as a QR code".into(),
            fill: "qr".into(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::{FakeClipboard, FakeText};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::Mutex;
    use std::time::Duration;

    struct Qr {
        p: QrProvider,
        rx: Receiver<()>,
        text: Arc<FakeText>,
        clip: Arc<FakeClipboard>,
    }

    fn qr(clipboard_text: Option<&str>) -> Qr {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let text = Arc::new(FakeText::new(clipboard_text));
        let clip = Arc::new(FakeClipboard::default());
        let p = QrProvider::new(
            text.clone(),
            clip.clone(),
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        Qr { p, rx, text, clip }
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    /// The rows once read: waits out a first `Reading clipboard…`.
    fn ready(t: &Qr, text: &str) -> Vec<ResultItem> {
        let rows = t.p.query(&q(text));
        if rows.first().is_some_and(|r| r.title == READING) {
            t.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready within 1 s");
            return t.p.query(&q(text));
        }
        rows
    }

    #[test]
    fn svg_is_a_square_code_with_a_quiet_zone() {
        let svg = qr_svg("hello").unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.contains("viewBox=\"0 0 29 29\""));
        assert!(svg.contains("<rect width=\"29\" height=\"29\" fill=\"#fff\"/>"));
        assert!(svg.contains("M4,4h1v1h-1z"), "the finder pattern's corner module");
    }

    #[test]
    fn svg_never_contains_the_text() {
        let svg = qr_svg("<script>x</script>").unwrap();
        assert!(!svg.contains("script"));
    }

    #[test]
    fn capacity_is_the_low_ecc_limit() {
        assert!(qr_svg(&"a".repeat(2953)).is_some());
        assert!(qr_svg(&"a".repeat(2954)).is_none());
    }

    #[test]
    fn typed_text_is_encoded_without_reading_the_clipboard() {
        let t = qr(Some("clip"));
        let rows = t.p.query(&q("hi"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "hi");
        assert_eq!(rows[0].id, "qr:hi");
        assert!(!rows[0].panel.is_empty());
        assert_eq!(t.text.calls(), 0);
    }

    #[test]
    fn bare_qr_reads_the_clipboard_in_the_background() {
        let long = "x".repeat(100);
        let t = qr(Some(&long));
        let rows = t.p.query(&q(""));
        assert_eq!(rows[0].title, "Reading clipboard\u{2026}");
        assert!(rows[0].panel.is_empty());
        t.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready");
        let rows = t.p.query(&q(""));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, format!("{}\u{2026}", "x".repeat(80)));
        assert_eq!(rows[0].id, format!("qr:{long}"));
        assert!(!rows[0].panel.is_empty());
    }

    #[test]
    fn qr_and_spaces_is_the_bare_form() {
        let t = qr(Some("clip"));
        let rows = ready(&t, "   ");
        assert_eq!(rows[0].title, "clip");
    }

    #[test]
    fn empty_or_non_text_clipboard_says_so() {
        for content in [None, Some("  ")] {
            let t = qr(content);
            let rows = ready(&t, "");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].title, "Nothing to show \u{2014} copy some text first");
            assert!(rows[0].panel.is_empty());
            assert_eq!(t.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Keep));
        }
    }

    /// Fails while `busy`; blocks while `gate` holds a receiver and nothing
    /// was sent on it.
    struct Flaky {
        busy: std::sync::atomic::AtomicBool,
        gate: Mutex<Option<Receiver<()>>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl ClipboardText for Flaky {
        fn text(&self) -> Result<Option<String>, String> {
            use std::sync::atomic::Ordering::SeqCst;
            self.calls.fetch_add(1, SeqCst);
            if let Some(gate) = self.gate.lock().unwrap().take() {
                let _ = gate.recv();
            }
            if self.busy.load(SeqCst) {
                Err("busy".into())
            } else {
                Ok(Some("hello".into()))
            }
        }
    }

    fn flaky(busy: bool, gate: Option<Receiver<()>>, slow: Duration) -> (Arc<Flaky>, QrProvider, Receiver<()>) {
        let source = Arc::new(Flaky {
            busy: busy.into(),
            gate: Mutex::new(gate),
            calls: Default::default(),
        });
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let p = QrProvider::with_slow(
            source.clone(),
            Arc::new(FakeClipboard::default()),
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
            slow,
        );
        (source, p, rx)
    }

    #[test]
    fn a_busy_clipboard_says_so_and_is_read_again() {
        use std::sync::atomic::Ordering::SeqCst;
        let (source, p, rx) = flaky(true, None, Duration::from_secs(3));
        assert_eq!(p.query(&q(""))[0].title, READING);
        rx.recv_timeout(Duration::from_secs(1)).expect("on_ready");
        let rows = p.query(&q(""));
        assert_eq!(rows[0].title, "Can't read the clipboard");
        assert_eq!(p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Keep));
        source.busy.store(false, SeqCst);
        assert_eq!(p.query(&q(""))[0].title, READING, "the failure was not kept");
        rx.recv_timeout(Duration::from_secs(1)).expect("on_ready");
        assert_eq!(p.query(&q(""))[0].title, "hello");
        assert_eq!(source.calls.load(SeqCst), 2);
    }

    #[test]
    fn a_slow_clipboard_says_so_until_it_answers() {
        let (open, gate) = mpsc::channel();
        let (source, p, rx) = flaky(false, Some(gate), Duration::from_millis(30));
        assert_eq!(p.query(&q(""))[0].title, READING);
        rx.recv_timeout(Duration::from_secs(1)).expect("on_ready at slow");
        assert_eq!(p.query(&q(""))[0].title, "Can't read the clipboard");
        open.send(()).unwrap();
        rx.recv_timeout(Duration::from_secs(1)).expect("on_ready when read");
        assert_eq!(p.query(&q(""))[0].title, "hello");
        assert_eq!(source.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn too_long_says_so() {
        let t = qr(None);
        let rows = t.p.query(&q(&"a".repeat(3000)));
        assert_eq!(rows[0].title, "Too long for a QR code");
        assert!(rows[0].panel.is_empty());
        let t = qr(Some(&"a".repeat(3000)));
        assert_eq!(ready(&t, "")[0].title, "Too long for a QR code");
    }

    #[test]
    fn enter_copies_the_text() {
        let t = qr(None);
        let rows = t.p.query(&q("hi"));
        assert_eq!(t.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(*t.clip.0.lock().unwrap(), vec!["hi".to_string()]);
    }

    #[test]
    fn forget_reads_again_next_time() {
        let t = qr(Some("one"));
        assert_eq!(ready(&t, "")[0].title, "one");
        assert_eq!(t.p.query(&q(""))[0].title, "one");
        assert_eq!(t.text.calls(), 1);
        *t.text.0.lock().unwrap() = Some("two".into());
        t.p.forget();
        assert_eq!(ready(&t, "")[0].title, "two");
        assert_eq!(t.text.calls(), 2);
    }

    #[test]
    fn rows_are_never_learned() {
        let t = qr(Some("clip"));
        let row = &t.p.query(&q("hi"))[0];
        assert_eq!(t.p.rank_key(&row.id), None);
        assert!(t.p.item(&row.id).is_none());
        assert!(!row.pinnable);
        assert_eq!(row.actions, vec![Action::SavePicture]);
        assert!(t.p.query(&q(&"a".repeat(3000)))[0].actions.is_empty());
        assert_eq!(t.p.name(), "qr");
        assert_eq!(t.p.route(), Some(Route { prefix: "qr", needs_space: true }));
    }

    fn decode(bytes: &[u8]) -> (png::OutputInfo, Vec<u8>) {
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        (info, buf)
    }

    #[test]
    fn png_is_the_code_scaled_up_with_the_quiet_zone() {
        let (info, px) = decode(&qr_png("hello").unwrap());
        assert_eq!((info.width, info.height), (1044, 1044), "29 modules x 36 pixels");
        let at = |x: usize, y: usize| px[y * 1044 + x];
        assert_eq!(at(4 * 36 + 18, 4 * 36 + 18), 0, "centre of the finder's corner module");
        assert_eq!(at(0, 0), 255);
    }

    #[test]
    fn png_capacity_matches_the_svg() {
        let big = qr_png(&"a".repeat(2953)).unwrap();
        assert!(decode(&big).0.width >= 1024);
        assert!(qr_png(&"a".repeat(2954)).is_none());
    }

    #[test]
    fn save_picture_hides_then_saves() {
        let t = qr(None);
        assert_eq!(
            t.p.act("qr:hi", Action::SavePicture),
            Ok(Outcome::HideThen { then: Then::SaveQr { text: "hi".into() } })
        );
        assert!(t.p.act("inert:x", Action::SavePicture).is_err());
        assert!(t.p.act("qr:hi", Action::Copy).is_err());
    }

    #[test]
    fn save_adds_png_to_a_bare_name_and_asks_with_the_suggestion() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out");
        let asked = Mutex::new(Vec::new());
        let notes = crate::notifications::FakeNotifier::default();
        save_qr(
            "hi",
            &|name| {
                asked.lock().unwrap().push(name.to_string());
                Some(target.clone())
            },
            &notes,
        );
        assert_eq!(*asked.lock().unwrap(), ["qr-code.png"]);
        assert_eq!(decode(&std::fs::read(dir.path().join("out.png")).unwrap()).0.width, 1044);
        assert!(notes.shown().is_empty());
    }

    #[test]
    fn save_keeps_an_extension_the_user_typed() {
        let dir = tempfile::tempdir().unwrap();
        let notes = crate::notifications::FakeNotifier::default();
        save_qr("hi", &|_| Some(dir.path().join("x.jpg")), &notes);
        assert!(dir.path().join("x.jpg").exists());
        assert!(!dir.path().join("x.jpg.png").exists());
    }

    #[test]
    fn cancelling_the_save_does_nothing() {
        let notes = crate::notifications::FakeNotifier::default();
        save_qr("hi", &|_| None, &notes);
        assert!(notes.shown().is_empty());
    }

    #[test]
    fn a_save_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let notes = crate::notifications::FakeNotifier::default();
        save_qr("hi", &|_| Some(dir.path().join("out.png")), &notes);
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["out.png"]);
    }

    #[test]
    fn a_failed_overwrite_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.png");
        std::fs::write(&target, b"old picture").unwrap();
        // The temp path is a directory: the write cannot happen.
        std::fs::create_dir(dir.path().join("out.png.tmp")).unwrap();
        let notes = crate::notifications::FakeNotifier::default();
        save_qr("hi", &|_| Some(target.clone()), &notes);
        assert_eq!(std::fs::read(&target).unwrap(), b"old picture");
        assert_eq!(notes.shown().len(), 1);
    }

    #[test]
    fn a_failed_write_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let notes = crate::notifications::FakeNotifier::default();
        save_qr("hi", &|_| Some(dir.path().join("missing").join("out")), &notes);
        assert_eq!(notes.shown(), [("Flick".to_string(), "Could not save the QR code".to_string())]);
    }
}
