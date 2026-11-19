//! Opt-in timing log: with the environment variable `FLICK_TIMING` set to a
//! file path, Flick appends one line per measured step to that file, so a
//! change can be judged by numbers. Unset (the normal case) every call here is
//! one relaxed atomic load and nothing else: no clock read, no allocation.
//!
//! A line is `<unix-ms>\t<name>\t<micros>\n`. A query line carries two more
//! fields, the length of the text (characters) and the number of rows.
//! The clock refresh (`live_row`) is the line `tick`. Names are compile-time strings: no typed text, no ids, no paths. The file
//! is written by a background thread, never by the calling thread; each line
//! is flushed as it is written, so a crash loses nothing earlier.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

enum Msg {
    Line(String),
    #[cfg(test)]
    Flush(Sender<()>),
}

/// A running log: a channel to the writer thread.
pub struct Timing {
    tx: Sender<Msg>,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static GLOBAL: OnceLock<Timing> = OnceLock::new();

/// Page timings the `timing_mark` command accepts.
const PAGE_NAMES: [&str; 3] = ["page.shown_to_paint", "page.query_to_paint", "page.tick"];

fn micros_since(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

impl Timing {
    /// Logs to `path` (appended to, created when missing); `None` when the file can't be opened.
    pub fn to_file(path: &std::path::Path) -> Option<Timing> {
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        let (tx, rx) = channel::<Msg>();
        std::thread::Builder::new()
            .name("flick-timing".into())
            .spawn(move || {
                for msg in rx {
                    match msg {
                        Msg::Line(line) => {
                            let _ = file.write_all(line.as_bytes());
                            let _ = file.flush();
                        }
                        #[cfg(test)]
                        Msg::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })
            .ok()?;
        Some(Timing { tx })
    }

    fn line(&self, name: &str, micros: u64, extra: Option<(usize, usize)>) {
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let line = match extra {
            None => format!("{ms}\t{name}\t{micros}\n"),
            Some((len, rows)) => format!("{ms}\t{name}\t{micros}\t{len}\t{rows}\n"),
        };
        let _ = self.tx.send(Msg::Line(line));
    }

    pub fn mark(&self, name: &str, micros: u64) {
        self.line(name, micros, None);
    }

    /// A query: its time, the text's length in characters and the row count (never the text).
    pub fn query(&self, micros: u64, text_len: usize, rows: usize) {
        self.line("query", micros, Some((text_len, rows)));
    }

    /// Waits until everything sent so far is in the file.
    #[cfg(test)]
    pub fn flush(&self) {
        let (tx, rx) = channel();
        if self.tx.send(Msg::Flush(tx)).is_ok() {
            let _ = rx.recv();
        }
    }
}

/// Reads `FLICK_TIMING` once; unset or unopenable leaves timing off.
pub fn init() {
    if let Some(path) = std::env::var_os("FLICK_TIMING").filter(|p| !p.is_empty()) {
        init_at(std::path::Path::new(&path));
    }
}

fn init_at(path: &std::path::Path) {
    if let Some(t) = Timing::to_file(path) {
        if GLOBAL.set(t).is_ok() {
            ENABLED.store(true, Ordering::Relaxed);
        }
    }
}

/// Whether timing is on: one relaxed atomic load.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn global() -> Option<&'static Timing> {
    if enabled() {
        GLOBAL.get()
    } else {
        None
    }
}

pub fn mark(name: &'static str, micros: u64) {
    if let Some(t) = global() {
        t.mark(name, micros);
    }
}

/// The static name for a page timing, or `None` when it is not on the list.
pub fn page_name(name: &str) -> Option<&'static str> {
    PAGE_NAMES.iter().copied().find(|n| *n == name)
}

/// Records the time from its creation to its drop under one name.
pub struct Span(Option<(Sender<Msg>, &'static str, Instant)>);

impl Span {
    pub fn on(t: &Timing, name: &'static str) -> Span {
        Span(Some((t.tx.clone(), name, Instant::now())))
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some((tx, name, start)) = self.0.take() {
            send(&tx, name, micros_since(start));
        }
    }
}

fn send(tx: &Sender<Msg>, name: &str, micros: u64) {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let _ = tx.send(Msg::Line(format!("{ms}\t{name}\t{micros}\n")));
}

/// A span that records nothing.
pub fn span_off() -> Span {
    Span(None)
}

pub fn span(name: &'static str) -> Span {
    match global() {
        Some(t) => Span::on(t, name),
        None => Span(None),
    }
}

/// Consecutive steps of one operation: `step("one")` records `name.one` with
/// the time since the previous step (or the start); the drop records `name`
/// with the whole time.
pub struct Steps(Option<Live>);

struct Live {
    tx: Sender<Msg>,
    name: &'static str,
    start: Instant,
    last: Instant,
}

impl Steps {
    pub fn new(name: &'static str) -> Steps {
        match global() {
            Some(t) => Steps::on(t, name),
            None => Steps(None),
        }
    }

    pub fn on(t: &Timing, name: &'static str) -> Steps {
        let now = Instant::now();
        Steps(Some(Live { tx: t.tx.clone(), name, start: now, last: now }))
    }

    /// Steps that record nothing.
    pub fn off() -> Steps {
        Steps(None)
    }

    pub fn step(&mut self, step: &'static str) {
        if let Some(l) = &mut self.0 {
            let now = Instant::now();
            send(&l.tx, &format!("{}.{step}", l.name), u64::try_from((now - l.last).as_micros()).unwrap_or(u64::MAX));
            l.last = now;
        }
    }

    /// Drops without recording the total.
    pub fn cancel(mut self) {
        self.0 = None;
    }
}

impl Drop for Steps {
    fn drop(&mut self) {
        if let Some(l) = self.0.take() {
            send(&l.tx, l.name, micros_since(l.start));
        }
    }
}

/// Times one query; [`QueryTimer::finish`] writes its line.
pub struct QueryTimer(Option<Instant>);

pub fn query_timer() -> QueryTimer {
    QueryTimer(if enabled() { Some(Instant::now()) } else { None })
}

impl QueryTimer {
    /// Counting the characters happens only when timing is on.
    pub fn finish(self, text: &str, rows: usize) {
        if let (Some(start), Some(t)) = (self.0, global()) {
            t.query(micros_since(start), text.chars().count(), rows);
        }
    }
}

/// Times one clock refresh (`live_row`); [`TickTimer::finish`] writes its line.
pub struct TickTimer(Option<Instant>);

pub fn tick_timer() -> TickTimer {
    TickTimer(if enabled() { Some(Instant::now()) } else { None })
}

impl TickTimer {
    pub fn finish(self) {
        if let (Some(start), Some(t)) = (self.0, global()) {
            t.mark("tick", micros_since(start));
        }
    }
}

/// Whether the page should report its own timings.
#[tauri::command]
pub fn timing_enabled() -> bool {
    enabled()
}

/// A page timing, accepted only for the names on [`PAGE_NAMES`].
#[tauri::command]
pub fn timing_mark(name: String, micros: u64) {
    if let Some(name) = page_name(&name) {
        mark(name, micros);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_log(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("flick-timing-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        dir
    }

    fn lines(path: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(path).unwrap().lines().map(str::to_string).collect()
    }

    #[test]
    fn disabled_marks_nothing() {
        // No test calls `init`, so the process-wide log is off.
        assert!(!enabled());
        mark("a", 5);
        drop(span("s"));
        let mut s = Steps::new("x");
        s.step("one");
        drop(s);
        query_timer().finish("abc", 1);
        assert!(!enabled());
    }

    #[test]
    fn a_mark_is_one_line() {
        let path = temp_log("mark");
        let t = Timing::to_file(&path).unwrap();
        t.mark("a", 5);
        t.flush();
        let l = lines(&path);
        assert_eq!(l.len(), 1);
        let f: Vec<&str> = l[0].split('\t').collect();
        assert_eq!(f.len(), 3);
        assert!(f[0].parse::<u64>().is_ok());
        assert_eq!((f[1], f[2]), ("a", "5"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn steps_write_each_step_then_the_total() {
        let path = temp_log("steps");
        let t = Timing::to_file(&path).unwrap();
        let mut s = Steps::on(&t, "x");
        s.step("one");
        s.step("two");
        drop(s);
        t.flush();
        let names: Vec<String> = lines(&path).iter().map(|l| l.split('\t').nth(1).unwrap().to_string()).collect();
        assert_eq!(names, ["x.one", "x.two", "x"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_span_records_on_drop_and_a_cancelled_steps_does_not() {
        let path = temp_log("span");
        let t = Timing::to_file(&path).unwrap();
        drop(Span::on(&t, "sp"));
        Steps::on(&t, "gone").cancel();
        t.flush();
        let l = lines(&path);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].split('\t').nth(1), Some("sp"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn only_page_names_on_the_list_are_accepted() {
        assert_eq!(page_name("page.tick"), Some("page.tick"));
        assert_eq!(page_name("page.shown_to_paint"), Some("page.shown_to_paint"));
        assert_eq!(page_name("page.query_to_paint"), Some("page.query_to_paint"));
        assert_eq!(page_name("page.evil"), None);
        assert_eq!(page_name("query"), None);
    }

    #[test]
    fn a_query_line_has_five_fields_and_never_the_text() {
        let path = temp_log("query");
        let t = Timing::to_file(&path).unwrap();
        let text = "secretword";
        t.query(42, text.chars().count(), 7);
        t.flush();
        let l = lines(&path);
        assert_eq!(l.len(), 1);
        let f: Vec<&str> = l[0].split('\t').collect();
        assert_eq!(f.len(), 5);
        assert_eq!((f[1], f[2], f[3], f[4]), ("query", "42", "10", "7"));
        assert!(!f.contains(&text));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unopenable_file_leaves_timing_off() {
        let dir = std::env::temp_dir();
        assert!(Timing::to_file(&dir).is_none());
    }
}
