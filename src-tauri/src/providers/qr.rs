//! `qr`: the clipboard (or the typed text) as a QR code the bar draws as a panel.
//! The clipboard is read on a [`Loader`] thread; nothing is stored or logged.

use std::sync::Arc;

use qrcodegen::{QrCode, QrCodeEcc};

use crate::clipboard::{Clipboard, ClipboardText};
use crate::dirs::{one_line, Load, Loader, ReadError, NEVER_STALE, SLOW};
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem, Route};

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

/// `text` as SVG: a white square and one black cell per dark module, with a
/// quiet zone. `None` when the text does not fit a code. Only module
/// positions reach the markup, never the text.
pub fn qr_svg(text: &str) -> Option<String> {
    let code = QrCode::encode_text(text, QrCodeEcc::Low).ok()?;
    let n = code.size();
    let side = n + 2 * QUIET;
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
        assert!(row.actions.is_empty());
        assert_eq!(t.p.name(), "qr");
        assert_eq!(t.p.route(), Some(Route { prefix: "qr", needs_space: true }));
    }
}
