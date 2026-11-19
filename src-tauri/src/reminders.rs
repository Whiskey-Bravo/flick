//! Timers and reminders. Pending ones are kept in `reminders.json` only until
//! they fire or are cancelled; one thread fires each exactly once.

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::dirs::Clock;
use crate::ranking::{load_json, save_json};

/// Characters of reminder text kept.
pub const MAX_TEXT: usize = 200;
/// How far ahead a timer or reminder can be set.
pub const MAX_AHEAD: Duration = Duration::from_secs(7 * 24 * 3600);
/// The longest the thread sleeps before reading the clock again, so a clock
/// jump (the PC slept, the time was changed) is seen without an `add`.
pub const MAX_SLEEP: Duration = Duration::from_secs(30);
/// The notification body of a timer.
pub const TIMER_DONE: &str = "Timer done";
/// The text of a reminder set without one.
pub const NO_TEXT: &str = "Reminder";
const SAVE_FAILED: &str = "Could not save the reminder";
const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Reminder {
    pub id: u64,
    /// Unix seconds.
    pub due: i64,
    pub text: String,
    pub timer: bool,
}

/// Shows a notification with this body.
pub type Fire = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(serde::Serialize)]
struct FileOut<'a> {
    version: u32,
    items: &'a [Reminder],
}

#[derive(serde::Deserialize)]
struct FileIn {
    version: u32,
    items: Vec<Reminder>,
}

/// The notification body: `Timer done` for a timer, else the text.
pub fn body(r: &Reminder) -> String {
    if r.timer {
        TIMER_DONE.to_string()
    } else {
        r.text.clone()
    }
}

/// `t` in Unix seconds, rounded up (a reminder never rings early).
pub fn unix_ceil(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64).saturating_add(i64::from(d.subsec_nanos() > 0)),
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// `t` in Unix seconds, rounded down, and the nanoseconds past that second.
fn unix_floor(t: SystemTime) -> (i64, u32) {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => {
            let d = e.duration();
            match d.subsec_nanos() {
                0 => (-(d.as_secs() as i64), 0),
                n => (-(d.as_secs() as i64) - 1, 1_000_000_000 - n),
            }
        }
    }
}

/// Unix seconds as a time, clamped to what the platform can hold.
pub fn from_unix(secs: i64) -> SystemTime {
    let d = Duration::from_secs(secs.unsigned_abs());
    let t = if secs >= 0 { UNIX_EPOCH.checked_add(d) } else { UNIX_EPOCH.checked_sub(d) };
    t.unwrap_or(UNIX_EPOCH)
}

/// Items in the file due later than now + `MAX_AHEAD` + this were not set by
/// Flick from this clock: a hand edit, or the clock is behind (a CMOS reset, a
/// VM resumed before its time sync). They wait in [`State::ahead`].
const SLACK: Duration = Duration::from_secs(24 * 3600);

/// The latest due, in Unix seconds, that Flick could have set by `now`.
fn latest_due(now: SystemTime) -> i64 {
    unix_ceil(now).saturating_add((MAX_AHEAD + SLACK).as_secs() as i64)
}

struct State {
    /// By due.
    items: Vec<Reminder>,
    /// Items of the file due further ahead than Flick sets (see [`SLACK`]).
    /// Not shown and not fired, but kept in the file: once the clock reaches
    /// them (it was set right), they join `items`.
    ahead: Vec<Reminder>,
    next_id: u64,
    /// The file has been read.
    loaded: bool,
    /// The file does not match `items` (a save failed); the thread retries.
    dirty: bool,
    /// Ids of the items that were already due when the file was read: missed
    /// while Flick was not running. Decided once, at the read: an item set in
    /// this run is never missed, even when the clock is later set back past
    /// that read (it would otherwise ring early, as `Missed: …`).
    overdue: Vec<u64>,
}

pub struct Reminders {
    path: PathBuf,
    now: Clock,
    fire: Fire,
    max_sleep: Duration,
    /// Held across the file's reads and writes.
    state: Mutex<State>,
    /// A copy of `State::items`, for [`Reminders::pending`]: the query path
    /// never waits on `state` while a slow disk reads or writes the file.
    shown: Mutex<Vec<Reminder>>,
    /// Woken by `add` and `cancel`.
    wake: Condvar,
    started: AtomicBool,
}

impl Reminders {
    /// No I/O: the file is read by [`Reminders::start`]'s thread (or the first `add`/`cancel`).
    pub fn new(path: PathBuf, now: Clock, fire: Fire) -> Arc<Self> {
        Self::build(path, now, fire, MAX_SLEEP)
    }

    #[cfg(test)]
    pub fn with_max_sleep(path: PathBuf, now: Clock, fire: Fire, max_sleep: Duration) -> Arc<Self> {
        Self::build(path, now, fire, max_sleep)
    }

    fn build(path: PathBuf, now: Clock, fire: Fire, max_sleep: Duration) -> Arc<Self> {
        Arc::new(Self {
            path,
            now,
            fire,
            max_sleep,
            state: Mutex::new(State {
                items: vec![],
                ahead: vec![],
                next_id: 1,
                loaded: false,
                dirty: false,
                overdue: vec![],
            }),
            shown: Mutex::new(vec![]),
            wake: Condvar::new(),
            started: AtomicBool::new(false),
        })
    }

    /// Starts the one thread (a second call does nothing). It reads the file,
    /// fires what fell due while Flick was not running once as `Missed: …`,
    /// then fires each item once when it falls due. It ends once every other
    /// handle is gone.
    pub fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(self);
        let spawned = std::thread::Builder::new().name("reminders".into()).spawn(move || run(weak));
        if spawned.is_err() {
            self.started.store(false, Ordering::SeqCst);
        }
    }

    /// Keeps a reminder due at `due` (text cut to [`MAX_TEXT`] characters) and
    /// saves it; when the save fails nothing is kept.
    pub fn add(&self, due: SystemTime, text: &str, timer: bool) -> Result<Reminder, String> {
        let mut st = self.lock();
        self.load(&mut st);
        let r = Reminder { id: st.next_id, due: unix_ceil(due), text: text.chars().take(MAX_TEXT).collect(), timer };
        let at = st.items.partition_point(|x| x.due <= r.due);
        st.items.insert(at, r.clone());
        if self.save(&st).is_err() {
            // The file is as it was: `dirty` stays what it was too.
            st.items.remove(at);
            return Err(SAVE_FAILED.to_string());
        }
        st.dirty = false;
        st.next_id = st.next_id.saturating_add(1);
        self.publish(&st);
        self.wake.notify_all();
        Ok(r)
    }

    /// Drops a pending item; false when there is none with `id` (it may have
    /// just fired). A cancelled item never fires in this run; if the save
    /// fails the thread retries it until the file no longer has it.
    pub fn cancel(&self, id: u64) -> bool {
        let mut st = self.lock();
        self.load(&mut st);
        let Some(at) = st.items.iter().position(|r| r.id == id) else {
            return false;
        };
        st.items.remove(at);
        self.publish(&st);
        self.persist(&mut st);
        self.wake.notify_all();
        true
    }

    /// Pending items by due. Memory only, and never waits on a file read or write.
    pub fn pending(&self) -> Vec<Reminder> {
        self.shown.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Copies the items for [`Reminders::pending`]; called after each change.
    fn publish(&self, st: &State) {
        *self.shown.lock().unwrap_or_else(PoisonError::into_inner) = st.items.clone();
    }

    /// True once the file has been read.
    #[cfg(test)]
    pub fn ready(&self) -> bool {
        self.lock().loaded
    }

    /// Writes the file once more when the last save failed (Flick is quitting,
    /// and the thread will not retry it): a fired or cancelled item left in the
    /// file would ring again as missed at the next start.
    pub fn flush(&self) {
        let mut st = self.lock();
        if st.dirty {
            self.persist(&mut st);
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reads the file once; a missing or corrupt one gives none (a corrupt
    /// one is kept as `.corrupt`). Items due further ahead than Flick sets wait
    /// in [`State::ahead`]; items due past year 9999 are dropped and repeated
    /// ids renumbered (then the file is rewritten by the thread).
    fn load(&self, st: &mut State) {
        if st.loaded {
            return;
        }
        let read = load_json::<FileIn>(&self.path)
            .filter(|f| f.version == VERSION)
            .map(|f| f.items)
            .unwrap_or_default();
        let now = (self.now)();
        let latest = latest_due(now);
        let possible = |r: &&Reminder| r.due <= latest || jiff::Timestamp::from_second(r.due).is_ok();
        let mut items: Vec<Reminder> = read.iter().filter(possible).cloned().collect();
        let mut changed = items.len() != read.len();
        items.sort_by_key(|r| r.due);
        let mut next = items.iter().map(|r| r.id).max().map_or(1, |m| m.saturating_add(1)).max(st.next_id);
        let mut seen = std::collections::HashSet::new();
        for r in &mut items {
            if !seen.insert(r.id) {
                r.id = next;
                seen.insert(next);
                next = next.saturating_add(1);
                changed = true;
            }
        }
        let read_at = unix_floor(now).0;
        let at = items.partition_point(|r| r.due <= latest);
        st.ahead = items.split_off(at);
        st.overdue = items.iter().filter(|r| r.due <= read_at).map(|r| r.id).collect();
        st.next_id = next;
        st.items = items;
        st.loaded = true;
        st.dirty = changed;
        self.publish(st);
    }

    /// Saves the items and records whether the file now matches them.
    fn persist(&self, st: &mut State) {
        st.dirty = self.save(st).is_err();
    }

    /// Writes the items, with those waiting in `ahead`; none removes the file.
    fn save(&self, st: &State) -> std::io::Result<()> {
        if st.items.is_empty() && st.ahead.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let items: Vec<Reminder> = st.items.iter().chain(&st.ahead).cloned().collect();
        save_json(&self.path, &FileOut { version: VERSION, items: &items })
    }

    /// Takes every item due by now out and saves the rest, before anything
    /// fires, so a crash after the save never repeats one. When the save
    /// fails, the thread retries it within `max_sleep` and at exit (`flush`);
    /// a crash before that leaves the item in the file, and the next start
    /// rings it again as `Missed: …`.
    fn take_due(&self, st: &mut State) -> Vec<Reminder> {
        self.load(st);
        let clock = (self.now)();
        // Items waiting in `ahead` that the clock has caught up with.
        let latest = latest_due(clock);
        if st.ahead.first().is_some_and(|r| r.due <= latest) {
            let at = st.ahead.partition_point(|r| r.due <= latest);
            let caught_up: Vec<Reminder> = st.ahead.drain(..at).collect();
            st.items.extend(caught_up);
            st.items.sort_by_key(|r| r.due);
            self.publish(st);
        }
        // Whole Unix seconds: no time arithmetic that could overflow on an odd due.
        let (now, _) = unix_floor(clock);
        let n = st.items.partition_point(|r| r.due <= now);
        if n == 0 {
            return vec![];
        }
        let due: Vec<Reminder> = st.items.drain(..n).collect();
        self.publish(st);
        self.persist(st);
        due
    }

    /// Takes the items missed while Flick was not running (those already due
    /// when the file was read, see [`State::overdue`]) out and saves the rest.
    /// Only once: later calls take nothing.
    fn take_missed(&self, st: &mut State) -> Vec<Reminder> {
        self.load(st);
        let overdue = std::mem::take(&mut st.overdue);
        if overdue.is_empty() {
            return vec![];
        }
        let (missed, rest): (Vec<Reminder>, Vec<Reminder>) =
            std::mem::take(&mut st.items).into_iter().partition(|r| overdue.contains(&r.id));
        st.items = rest;
        self.publish(st);
        if !missed.is_empty() {
            self.persist(st);
        }
        missed
    }

    /// Waits once, until the earliest item is due, an `add`/`cancel`, or
    /// `max_sleep`; returns what is due then. A save that failed earlier is
    /// retried first (so at least every `max_sleep`).
    fn wait_once(&self) -> Vec<Reminder> {
        let mut st = self.lock();
        if st.dirty {
            self.persist(&mut st);
        }
        let due = self.take_due(&mut st);
        if !due.is_empty() {
            return due;
        }
        let (now, nanos) = unix_floor((self.now)());
        let wait = st
            .items
            .first()
            .map(|r| {
                let secs = u64::try_from(r.due.saturating_sub(now)).unwrap_or(0);
                Duration::from_secs(secs).saturating_sub(Duration::from_nanos(u64::from(nanos)))
            })
            .map_or(self.max_sleep, |w| w.min(self.max_sleep));
        let mut st = self.wake.wait_timeout(st, wait).unwrap_or_else(PoisonError::into_inner).0;
        self.take_due(&mut st)
    }

    /// Shows one notification; a panic in it does not stop the thread.
    fn fire_one(&self, text: &str) {
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| (self.fire)(text)));
    }
}

/// The thread: fires outside the lock, after the save.
fn run(weak: Weak<Reminders>) {
    let Some(me) = weak.upgrade() else { return };
    let missed = {
        let mut st = me.lock();
        // Only what was already overdue when the file was read is "missed";
        // anything set since fires normally, even if this thread starts late
        // or the clock went back meanwhile.
        me.take_missed(&mut st)
    };
    for r in &missed {
        me.fire_one(&format!("Missed: {}", body(r)));
    }
    drop(me);
    loop {
        let Some(me) = weak.upgrade() else { return };
        for r in me.wait_once() {
            me.fire_one(&body(&r));
        }
    }
}

/// `5m`, `90s`, `1h30m`, `2H`; a bare number is minutes. `None` for zero or anything else.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    let secs = if s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse::<u64>().ok()?.checked_mul(60)?
    } else {
        let mut total: u64 = 0;
        let mut num: Option<u64> = None;
        // Units come in order, each at most once: h, then m, then s.
        let mut next_rank = 0;
        for c in s.chars() {
            if let Some(d) = c.to_digit(10) {
                num = Some(num.unwrap_or(0).checked_mul(10)?.checked_add(u64::from(d))?);
                continue;
            }
            let (rank, unit) = match c {
                'h' => (0, 3600),
                'm' => (1, 60),
                's' => (2, 1),
                _ => return None,
            };
            if rank < next_rank {
                return None;
            }
            next_rank = rank + 1;
            total = total.checked_add(num.take()?.checked_mul(unit)?)?;
        }
        if num.is_some() {
            return None;
        }
        total
    };
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// When a reminder is due.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum When {
    In(Duration),
    At { hour: u8, minute: u8, tomorrow: bool },
}

/// `H:MM` or `HH:MM`.
fn parse_time(s: &str) -> Option<(u8, u8)> {
    let (h, m) = s.split_once(':')?;
    if !(1..=2).contains(&h.len()) || m.len() != 2 || !h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (hour, minute) = (h.parse::<u8>().ok()?, m.parse::<u8>().ok()?);
    (hour < 24 && minute < 60).then_some((hour, minute))
}

/// The first word and the rest, both trimmed.
pub(crate) fn first_word(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.split_once(char::is_whitespace) {
        Some((w, rest)) => (w, rest.trim()),
        None => (s, ""),
    }
}

/// `<duration> <text>`, `H:MM <text>` or `tomorrow H:MM <text>`; no text gives `Reminder`.
pub fn parse_remind(rest: &str) -> Option<(When, String)> {
    let (word, after) = first_word(rest);
    let (when, text) = if word.eq_ignore_ascii_case("tomorrow") {
        let (time, text) = first_word(after);
        let (hour, minute) = parse_time(time)?;
        (When::At { hour, minute, tomorrow: true }, text)
    } else if let Some((hour, minute)) = parse_time(word) {
        (When::At { hour, minute, tomorrow: false }, after)
    } else {
        (When::In(parse_duration(word)?), after)
    };
    let text = if text.is_empty() { NO_TEXT } else { text };
    Some((when, text.to_string()))
}

/// When `when` falls after `now`: an `At` time today, or tomorrow when it is
/// not later than now (`tomorrow` always means the next day).
pub fn due_at(when: When, now: &jiff::Zoned) -> jiff::Zoned {
    match when {
        When::In(d) => jiff::SignedDuration::try_from(d)
            .ok()
            .and_then(|d| now.checked_add(d).ok())
            .unwrap_or_else(|| now.clone()),
        When::At { hour, minute, tomorrow } => {
            let on = |date: jiff::civil::Date| {
                date.at(hour as i8, minute as i8, 0, 0).to_zoned(now.time_zone().clone()).ok()
            };
            let next_day = || now.date().tomorrow().ok().and_then(on);
            let due = if tomorrow { next_day() } else { on(now.date()).filter(|z| z > now).or_else(next_day) };
            due.unwrap_or_else(|| now.clone())
        }
    }
}

/// `45 s`, `1 min 30 s`, `5 min`, `1 h`, `1 h 30 min`, `1 d 6 h`: seconds are
/// dropped from an hour up, minutes from a day up.
pub fn duration_text(d: Duration) -> String {
    let s = d.as_secs();
    let pair = |big: u64, big_unit: &str, small: u64, small_unit: &str| {
        if small == 0 {
            format!("{big} {big_unit}")
        } else {
            format!("{big} {big_unit} {small} {small_unit}")
        }
    };
    match s {
        0..60 => format!("{s} s"),
        60..3600 => pair(s / 60, "min", s % 60, "s"),
        3600..86400 => pair(s / 3600, "h", s % 3600 / 60, "min"),
        _ => pair(s / 86400, "d", s % 86400 / 3600, "h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver};

    fn at(s: &str) -> SystemTime {
        SystemTime::from(s.parse::<jiff::Timestamp>().unwrap())
    }

    fn t0() -> SystemTime {
        at("2026-11-17T14:27:00Z")
    }

    struct Rig {
        dir: tempfile::TempDir,
        clock: Arc<Mutex<SystemTime>>,
        rx: Receiver<String>,
        r: Arc<Reminders>,
    }

    impl Rig {
        fn path(&self) -> PathBuf {
            self.dir.path().join("reminders.json")
        }
        fn set_clock(&self, t: SystemTime) {
            *self.clock.lock().unwrap() = t;
        }
        fn advance(&self, d: Duration) {
            let t = *self.clock.lock().unwrap() + d;
            self.set_clock(t);
        }
        fn now(&self) -> SystemTime {
            *self.clock.lock().unwrap()
        }
        fn quiet_for(&self, ms: u64) -> Vec<String> {
            let end = std::time::Instant::now() + Duration::from_millis(ms);
            let mut got = vec![];
            while let Some(left) = end.checked_duration_since(std::time::Instant::now()) {
                match self.rx.recv_timeout(left) {
                    Ok(b) => got.push(b),
                    Err(_) => break,
                }
            }
            got
        }
        fn wait_ready(&self) {
            let end = std::time::Instant::now() + Duration::from_secs(1);
            while !self.r.ready() {
                assert!(std::time::Instant::now() < end, "never read the file");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    fn rig_in(dir: tempfile::TempDir, max_sleep: Option<Duration>) -> Rig {
        let clock = Arc::new(Mutex::new(t0()));
        let c = Arc::clone(&clock);
        let now: Clock = Arc::new(move || *c.lock().unwrap());
        let (tx, rx) = channel::<String>();
        let fire: Fire = Arc::new(move |b: &str| {
            let _ = tx.send(b.to_string());
        });
        let path = dir.path().join("reminders.json");
        let r = match max_sleep {
            Some(m) => Reminders::with_max_sleep(path, now, fire, m),
            None => Reminders::new(path, now, fire),
        };
        Rig { dir, clock, rx, r }
    }

    fn rig(max_sleep: Option<Duration>) -> Rig {
        rig_in(tempfile::tempdir().unwrap(), max_sleep)
    }

    const FAST: Option<Duration> = Some(Duration::from_millis(10));

    #[test]
    fn durations_parse() {
        let s = |n| Some(Duration::from_secs(n));
        assert_eq!(parse_duration("5m"), s(300));
        assert_eq!(parse_duration("90s"), s(90));
        assert_eq!(parse_duration("1h30m"), s(5400));
        assert_eq!(parse_duration("5"), s(300));
        assert_eq!(parse_duration("2H"), s(7200));
        assert_eq!(parse_duration(" 10m "), s(600));
        assert_eq!(parse_duration("1h30m15s"), s(5415));
        for bad in ["0m", "5x", "m", "", "8d", "0", "30m1h", "5m5m", "1h30", "99999999999999999999h"] {
            assert_eq!(parse_duration(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn remind_parses_in_at_and_tomorrow() {
        assert_eq!(parse_remind("20m pizza"), Some((When::In(Duration::from_secs(1200)), "pizza".into())));
        assert_eq!(
            parse_remind("18:30 call mum"),
            Some((When::At { hour: 18, minute: 30, tomorrow: false }, "call mum".into()))
        );
        assert_eq!(
            parse_remind("tomorrow 9:00 dentist"),
            Some((When::At { hour: 9, minute: 0, tomorrow: true }, "dentist".into()))
        );
        assert_eq!(
            parse_remind("Tomorrow 09:05 Dentist  at noon"),
            Some((When::At { hour: 9, minute: 5, tomorrow: true }, "Dentist  at noon".into()))
        );
        assert_eq!(parse_remind("20m"), Some((When::In(Duration::from_secs(1200)), "Reminder".into())));
        for bad in ["25:00 x", "9.00 x", "18:60 x", "18:5 x", "pizza", "", "tomorrow", "tomorrow pizza", "tomorrow 20m x"] {
            assert_eq!(parse_remind(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn at_times_roll_to_tomorrow_when_past() {
        let now = "2026-11-17T19:00:00Z".parse::<jiff::Timestamp>().unwrap().to_zoned(jiff::tz::TimeZone::UTC);
        let due = |hour, minute, tomorrow| due_at(When::At { hour, minute, tomorrow }, &now).timestamp().to_string();
        assert_eq!(due(18, 30, false), "2026-11-18T18:30:00Z");
        assert_eq!(due(19, 30, false), "2026-11-17T19:30:00Z");
        assert_eq!(due(19, 0, false), "2026-11-18T19:00:00Z");
        assert_eq!(due(19, 30, true), "2026-11-18T19:30:00Z");
        let in_20 = due_at(When::In(Duration::from_secs(1200)), &now).timestamp().to_string();
        assert_eq!(in_20, "2026-11-17T19:20:00Z");
    }

    #[test]
    fn duration_text_reads_naturally() {
        let t = |n| duration_text(Duration::from_secs(n));
        assert_eq!(t(45), "45 s");
        assert_eq!(t(90), "1 min 30 s");
        assert_eq!(t(300), "5 min");
        assert_eq!(t(3600), "1 h");
        assert_eq!(t(5400), "1 h 30 min");
        assert_eq!(t(5415), "1 h 30 min");
        assert_eq!(t(30 * 3600), "1 d 6 h");
        assert_eq!(t(24 * 3600 + 60), "1 d");
    }

    #[test]
    fn body_is_timer_done_for_timers() {
        let r = Reminder { id: 1, due: 0, text: "Timer 5 min".into(), timer: true };
        assert_eq!(body(&r), TIMER_DONE);
        assert_eq!(body(&Reminder { timer: false, ..r }), "Timer 5 min");
    }

    #[test]
    fn a_reminder_fires_once_and_leaves_the_file() {
        let g = rig(FAST);
        g.r.start();
        g.r.add(g.now() + Duration::from_millis(50), "pizza", false).unwrap();
        assert!(g.path().exists());
        assert_eq!(g.quiet_for(100), Vec::<String>::new(), "not due yet");
        g.advance(Duration::from_secs(1));
        assert_eq!(g.rx.recv_timeout(Duration::from_secs(1)).unwrap(), "pizza");
        assert_eq!(g.quiet_for(300), Vec::<String>::new());
        assert!(!g.path().exists());
        assert!(g.r.pending().is_empty());
    }

    #[test]
    fn cancelled_never_fires_even_at_the_due_moment() {
        let g = rig(FAST);
        g.r.start();
        let r = g.r.add(g.now() + Duration::from_millis(100), "pizza", false).unwrap();
        assert!(g.r.cancel(r.id));
        g.advance(Duration::from_secs(1));
        assert_eq!(g.quiet_for(400), Vec::<String>::new());
        assert!(!g.path().exists());
        assert!(!g.r.cancel(r.id));
        assert!(!g.r.cancel(12345));
    }

    #[test]
    fn overdue_at_start_fires_once_as_missed() {
        let dir = tempfile::tempdir().unwrap();
        let past = unix_ceil(t0()) - 60;
        let soon = unix_ceil(t0()) + 1;
        let file = serde_json::json!({"version": 1, "items": [
            {"id": 1, "due": past, "text": "call mum", "timer": false},
            {"id": 2, "due": soon, "text": "Timer 5 min", "timer": true},
        ]});
        std::fs::write(dir.path().join("reminders.json"), file.to_string()).unwrap();
        let g = rig_in(dir, FAST);
        g.r.start();
        assert_eq!(g.rx.recv_timeout(Duration::from_secs(1)).unwrap(), "Missed: call mum");
        assert_eq!(g.quiet_for(100), Vec::<String>::new());
        assert_eq!(g.r.pending().iter().map(|r| r.id).collect::<Vec<_>>(), vec![2]);
        g.advance(Duration::from_secs(1));
        assert_eq!(g.rx.recv_timeout(Duration::from_secs(1)).unwrap(), TIMER_DONE);
        assert_eq!(g.quiet_for(100), Vec::<String>::new());

        // A restart over the same file: nothing is left to fire.
        let again = rig_in(g.dir, FAST);
        again.set_clock(t0() + Duration::from_secs(3600));
        again.r.start();
        again.wait_ready();
        assert_eq!(again.quiet_for(300), Vec::<String>::new());
        assert!(!again.path().exists());
    }

    #[test]
    fn a_clock_jump_still_fires() {
        let g = rig(Some(Duration::from_millis(20)));
        g.r.start();
        g.r.add(g.now() + Duration::from_secs(3600), "pizza", false).unwrap();
        assert_eq!(g.quiet_for(50), Vec::<String>::new());
        // The PC slept: the clock moves without an add or a cancel waking the thread.
        g.advance(Duration::from_secs(3601));
        assert_eq!(g.rx.recv_timeout(Duration::from_millis(200)).unwrap(), "pizza");
        assert_eq!(g.quiet_for(100), Vec::<String>::new());
    }

    #[test]
    fn a_corrupt_file_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("reminders.json"), "not json {").unwrap();
        let g = rig_in(dir, None);
        g.r.start();
        g.wait_ready();
        assert!(g.r.pending().is_empty());
        assert_eq!(g.quiet_for(50), Vec::<String>::new());
        let r = g.r.add(g.now() + Duration::from_secs(60), "pizza", false).unwrap();
        assert_eq!(g.r.pending(), vec![r]);
        let aside = g.dir.path().join("reminders.json.corrupt");
        assert_eq!(std::fs::read_to_string(aside).unwrap(), "not json {", "kept for the user");
    }

    #[test]
    fn pending_never_waits_while_the_file_is_read_or_written() {
        let g = rig(None);
        let r = g.r.add(g.now() + Duration::from_secs(60), "pizza", false).unwrap();
        // The state lock is held, as during a slow save.
        let _busy = g.r.lock();
        let (tx, rx) = channel();
        let reminders = Arc::clone(&g.r);
        std::thread::spawn(move || {
            let _ = tx.send(reminders.pending());
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok(vec![r]));
    }

    #[test]
    fn nothing_is_written_until_add() {
        let g = rig(None);
        assert!(!g.r.ready());
        g.r.start();
        g.wait_ready();
        std::thread::sleep(Duration::from_millis(30));
        assert!(!g.path().exists());
        assert!(g.r.pending().is_empty());
    }

    #[test]
    fn text_is_cut_to_200() {
        let g = rig(None);
        let long = "é".repeat(250);
        let r = g.r.add(g.now() + Duration::from_secs(60), &long, false).unwrap();
        assert_eq!(r.text.chars().count(), MAX_TEXT);
        assert_eq!(g.r.pending()[0].text.chars().count(), MAX_TEXT);
    }

    #[test]
    fn the_file_keeps_version_and_items_by_due() {
        let g = rig(None);
        let late = g.r.add(g.now() + Duration::from_secs(600), "late", false).unwrap();
        let early = g.r.add(g.now() + Duration::from_secs(60), "early", false).unwrap();
        assert_ne!(late.id, early.id);
        assert_eq!(g.r.pending(), vec![early.clone(), late.clone()]);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(g.path()).unwrap()).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["items"].as_array().unwrap().len(), 2);
        assert!(g.r.cancel(early.id));
        assert!(g.r.cancel(late.id));
        assert!(!g.path().exists(), "an empty list removes the file");
    }

    #[test]
    fn a_failed_save_keeps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        // The file's folder is a file, so nothing can be written under it.
        std::fs::write(dir.path().join("blocker"), "x").unwrap();
        let clock: Clock = Arc::new(t0);
        let fire: Fire = Arc::new(|_: &str| {});
        let r = Reminders::new(dir.path().join("blocker").join("reminders.json"), clock, fire);
        let err = r.add(t0() + Duration::from_secs(60), "pizza", false).unwrap_err();
        assert_eq!(err, "Could not save the reminder");
        assert!(r.pending().is_empty());
    }

    #[test]
    fn ids_go_on_from_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = serde_json::json!({"version": 1, "items": [
            {"id": 7, "due": unix_ceil(t0()) + 600, "text": "x", "timer": false},
        ]});
        std::fs::write(dir.path().join("reminders.json"), file.to_string()).unwrap();
        let g = rig_in(dir, None);
        let r = g.r.add(g.now() + Duration::from_secs(60), "y", false).unwrap();
        assert_eq!(r.id, 8);
        assert_eq!(g.r.pending().len(), 2, "add reads the file first, never writes over it");
    }

    #[test]
    fn the_largest_id_in_the_file_never_overflows() {
        let dir = tempfile::tempdir().unwrap();
        let file = serde_json::json!({"version": 1, "items": [
            {"id": u64::MAX, "due": unix_ceil(t0()) + 600, "text": "x", "timer": false},
        ]});
        std::fs::write(dir.path().join("reminders.json"), file.to_string()).unwrap();
        let g = rig_in(dir, None);
        g.r.add(g.now() + Duration::from_secs(60), "y", false).unwrap();
        g.r.add(g.now() + Duration::from_secs(90), "z", false).unwrap();
        assert_eq!(g.r.pending().len(), 3);
    }

    fn write_items(dir: &tempfile::TempDir, items: serde_json::Value) {
        let file = serde_json::json!({"version": 1, "items": items});
        std::fs::write(dir.path().join("reminders.json"), file.to_string()).unwrap();
    }

    fn wait_until(what: &str, ms: u64, ok: impl Fn() -> bool) {
        let end = std::time::Instant::now() + Duration::from_millis(ms);
        while !ok() {
            assert!(std::time::Instant::now() < end, "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn odd_unix_seconds_never_panic() {
        for s in [i64::MAX, i64::MIN, 99_999_999_999_999, -99_999_999_999_999] {
            let _ = unix_ceil(from_unix(s));
        }
        assert_eq!(from_unix(unix_ceil(t0())), t0());
        assert_eq!(unix_floor(UNIX_EPOCH - Duration::from_millis(1500)), (-2, 500_000_000));
    }

    #[test]
    fn absurd_dues_in_a_hand_edited_file_do_not_stop_the_thread() {
        let dir = tempfile::tempdir().unwrap();
        write_items(&dir, serde_json::json!([
            {"id": 1, "due": 99_999_999_999_999_i64, "text": "far", "timer": false},
            {"id": 2, "due": -99_999_999_999_999_i64, "text": "old", "timer": false},
            {"id": 3, "due": unix_ceil(t0()) + 1, "text": "pizza", "timer": false},
        ]));
        let g = rig_in(dir, FAST);
        g.r.start();
        assert_eq!(g.rx.recv_timeout(Duration::from_secs(1)).unwrap(), "Missed: old");
        assert_eq!(g.r.pending().iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["pizza"], "far is dropped");
        // Past year 9999: dropped from the file too.
        g.advance(Duration::from_secs(1));
        assert_eq!(g.rx.recv_timeout(Duration::from_secs(1)).unwrap(), "pizza");
        assert_eq!(g.quiet_for(100), Vec::<String>::new());
        assert!(!g.path().exists());
    }

    #[test]
    fn a_clock_far_behind_at_start_keeps_the_reminders_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let month = unix_ceil(t0()) + 30 * 24 * 3600;
        write_items(&dir, serde_json::json!([
            {"id": 4, "due": month, "text": "dentist", "timer": false},
        ]));
        let g = rig_in(dir, FAST);
        g.r.start();
        g.wait_ready();
        assert!(g.r.pending().is_empty(), "not shown while the clock is behind");
        std::thread::sleep(Duration::from_millis(40));
        // A save for another reason keeps it, and does not reuse its id.
        let pizza = g.r.add(g.now() + Duration::from_secs(3600), "pizza", false).unwrap();
        assert_ne!(pizza.id, 4);
        let saved = std::fs::read_to_string(g.path()).unwrap();
        assert!(saved.contains("dentist") && saved.contains("pizza"), "{saved}");
        // The clock is set right: it shows, and rings when due.
        g.set_clock(from_unix(month - 3600));
        wait_until("never joined the pending items", 500, || g.r.pending().iter().any(|r| r.text == "dentist"));
        assert_eq!(g.rx.recv_timeout(Duration::from_secs(1)).unwrap(), "pizza");
        g.set_clock(from_unix(month));
        assert_eq!(g.rx.recv_timeout(Duration::from_secs(1)).unwrap(), "dentist");
        assert_eq!(g.quiet_for(50), Vec::<String>::new());
        assert!(!g.path().exists());
    }

    #[test]
    fn a_failed_save_on_cancel_is_retried_until_the_file_matches() {
        let g = rig(FAST);
        g.r.start();
        let a = g.r.add(g.now() + Duration::from_secs(60), "first", false).unwrap();
        g.r.add(g.now() + Duration::from_secs(120), "second", false).unwrap();
        // A folder with the file's name: neither the write nor the rename can succeed.
        std::fs::remove_file(g.path()).unwrap();
        std::fs::create_dir(g.path()).unwrap();
        assert!(g.r.cancel(a.id));
        std::thread::sleep(Duration::from_millis(40));
        assert!(g.path().is_dir(), "still blocked");
        std::fs::remove_dir(g.path()).unwrap();
        wait_until("the cancel never reached the file", 500, || {
            std::fs::read_to_string(g.path()).is_ok_and(|s| s.contains("second") && !s.contains("first"))
        });
        assert_eq!(g.quiet_for(50), Vec::<String>::new());
    }

    #[test]
    fn flush_at_exit_writes_a_save_that_failed() {
        // No thread: only the exit flush can write the cancel.
        let g = rig(FAST);
        let a = g.r.add(g.now() + Duration::from_secs(60), "first", false).unwrap();
        g.r.add(g.now() + Duration::from_secs(120), "second", false).unwrap();
        std::fs::remove_file(g.path()).unwrap();
        std::fs::create_dir(g.path()).unwrap();
        assert!(g.r.cancel(a.id));
        std::fs::remove_dir(g.path()).unwrap();
        assert!(!g.path().exists(), "the failed save wrote nothing");
        g.r.flush();
        let saved = std::fs::read_to_string(g.path()).unwrap();
        assert!(saved.contains("second") && !saved.contains("first"), "{saved}");
        // Nothing left to write: a second flush leaves the file alone.
        std::fs::remove_file(g.path()).unwrap();
        g.r.flush();
        assert!(!g.path().exists());
    }

    #[test]
    fn a_failed_save_while_firing_is_retried_so_the_text_leaves_disk() {
        let g = rig(FAST);
        g.r.start();
        g.r.add(g.now() + Duration::from_secs(1), "pizza", false).unwrap();
        // Held open without delete sharing, the file cannot be removed: the
        // save after firing fails until the handle is dropped.
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        let held = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(g.path()).unwrap();
        g.advance(Duration::from_secs(1));
        assert_eq!(g.rx.recv_timeout(Duration::from_secs(1)).unwrap(), "pizza");
        std::thread::sleep(Duration::from_millis(40));
        assert!(g.path().exists(), "still blocked");
        drop(held);
        wait_until("the fired text stayed on disk", 500, || !g.path().exists());
        assert_eq!(g.quiet_for(50), Vec::<String>::new());
    }

    #[test]
    fn one_set_this_run_is_never_missed_even_if_the_thread_starts_late() {
        let g = rig(FAST);
        g.r.add(g.now() + Duration::from_secs(1), "pizza", false).unwrap();
        g.advance(Duration::from_secs(2));
        g.r.start();
        assert_eq!(g.rx.recv_timeout(Duration::from_secs(1)).unwrap(), "pizza");
        assert_eq!(g.quiet_for(100), Vec::<String>::new());
    }

    #[test]
    fn the_clock_set_back_never_makes_one_set_this_run_missed_or_early() {
        let dir = tempfile::tempdir().unwrap();
        write_items(&dir, serde_json::json!([
            {"id": 1, "due": unix_ceil(t0()) - 60, "text": "call mum", "timer": false},
        ]));
        let g = rig_in(dir, FAST);
        // The file is read at t0 (by an add before the thread starts) ...
        g.r.add(g.now() + Duration::from_secs(3600), "later", false).unwrap();
        // ... then the clock is set back an hour, and a reminder is set for
        // five minutes from the new now: still before the moment of the read.
        g.set_clock(t0() - Duration::from_secs(3600));
        g.r.add(g.now() + Duration::from_secs(300), "pizza", false).unwrap();
        g.r.start();
        assert_eq!(g.rx.recv_timeout(Duration::from_secs(1)).unwrap(), "Missed: call mum");
        assert_eq!(g.quiet_for(100), Vec::<String>::new(), "pizza is not due yet and was never missed");
        g.advance(Duration::from_secs(300));
        assert_eq!(g.rx.recv_timeout(Duration::from_secs(1)).unwrap(), "pizza");
        assert_eq!(g.quiet_for(100), Vec::<String>::new());
        assert_eq!(g.r.pending().iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["later"]);
    }

    #[test]
    fn a_panicking_fire_does_not_stop_later_ones() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(Mutex::new(t0()));
        let c = Arc::clone(&clock);
        let now: Clock = Arc::new(move || *c.lock().unwrap());
        let (tx, rx) = channel::<String>();
        let fire: Fire = Arc::new(move |b: &str| {
            assert!(b != "boom", "the notifier broke");
            let _ = tx.send(b.to_string());
        });
        let r = Reminders::with_max_sleep(dir.path().join("reminders.json"), now, fire, Duration::from_millis(10));
        r.start();
        r.add(t0() + Duration::from_secs(1), "boom", false).unwrap();
        r.add(t0() + Duration::from_secs(1), "pizza", false).unwrap();
        r.add(t0() + Duration::from_secs(5), "later", false).unwrap();
        *clock.lock().unwrap() = t0() + Duration::from_secs(1);
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), "pizza");
        *clock.lock().unwrap() = t0() + Duration::from_secs(5);
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), "later");
    }

    #[test]
    fn repeated_ids_in_the_file_are_renumbered() {
        let dir = tempfile::tempdir().unwrap();
        let due = unix_ceil(t0()) + 600;
        write_items(&dir, serde_json::json!([
            {"id": 3, "due": due, "text": "a", "timer": false},
            {"id": 3, "due": due + 1, "text": "b", "timer": false},
        ]));
        let g = rig_in(dir, FAST);
        g.r.start();
        g.wait_ready();
        let ids: Vec<u64> = g.r.pending().iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        // The thread writes the renumbered list back.
        wait_until("the file kept the repeated id", 500, || {
            let s = std::fs::read_to_string(g.path()).unwrap_or_default();
            serde_json::from_str::<serde_json::Value>(&s).is_ok_and(|v| {
                let ids: Vec<u64> = v["items"].as_array().unwrap().iter().map(|i| i["id"].as_u64().unwrap()).collect();
                ids.len() == 2 && ids[0] != ids[1]
            })
        });
        assert!(g.r.cancel(ids[1]));
        assert_eq!(g.r.pending().iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["a"]);
    }
}
