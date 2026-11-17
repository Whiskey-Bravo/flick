//! `timer 5m`, `remind 20m pizza` / `remind 18:30 call mum`, and `timers` to
//! list and cancel pending ones. Rows are never learned; the query reads only
//! memory (the reminder thread reads the file).

use std::sync::Arc;
use std::time::Duration;

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

use crate::dirs::Clock;
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem};
use crate::providers::path::score_in_order;
use crate::reminders::{
    due_at, duration_text, first_word, from_unix, parse_duration, parse_remind, unix_ceil, Reminders, When, MAX_AHEAD,
    MAX_TEXT,
};

const NAME: &str = "timers";
const ICON: &str = "builtin:timer";
const INERT: &str = "inert:";
/// Id prefix of a timer row: `timer:<seconds>`.
const TIMER: &str = "timer:";
const TIMER_USAGE: &str = "Type a time, like timer 5m";
const REMIND_USAGE: &str = "Type a time and a note, like remind 20m pizza";
const TOO_FAR: &str = "Up to 7 days ahead";
const NONE_PENDING: &str = "No timers or reminders";
const STARTS: &str = "Enter starts it";
const WORDS: [&str; 4] = ["timer", "remind", "timers", "reminders"];

pub struct TimersProvider {
    reminders: Arc<Reminders>,
    now: Clock,
    tz: TimeZone,
    on_ready: Arc<dyn Fn() + Send + Sync>,
}

impl TimersProvider {
    pub fn new(reminders: Arc<Reminders>, now: Clock, tz: TimeZone, on_ready: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { reminders, now, tz, on_ready }
    }

    /// Now, in whole seconds rounded up (as dues are).
    fn now_secs(&self) -> i64 {
        unix_ceil((self.now)())
    }

    fn zoned(&self, secs: i64) -> Option<Zoned> {
        Some(Timestamp::from_second(secs).ok()?.to_zoned(self.tz.clone()))
    }

    fn clock_text(&self, secs: i64) -> String {
        self.zoned(secs).map_or_else(String::new, |z| format!("{:02}:{:02}", z.hour(), z.minute()))
    }

    fn timer_row(&self, rest: &str) -> ResultItem {
        let Some(d) = parse_duration(rest) else {
            return inert(TIMER_USAGE);
        };
        if d > MAX_AHEAD {
            return inert(TOO_FAR);
        }
        // The row says when it would ring now; Enter counts from its own moment.
        let due = self.now_secs() + d.as_secs() as i64;
        row(
            format!("{TIMER}{}", d.as_secs()),
            format!("{} \u{2014} rings at {}", timer_label(d), self.clock_text(due)),
            STARTS.into(),
            false,
        )
    }

    fn remind_row(&self, rest: &str) -> ResultItem {
        let Some((when, text)) = parse_remind(rest) else {
            return inert(REMIND_USAGE);
        };
        if matches!(when, When::In(d) if d > MAX_AHEAD) {
            return inert(TOO_FAR);
        }
        let now = self.now_secs();
        let Some(now_z) = self.zoned(now) else {
            return inert(REMIND_USAGE);
        };
        let due_z = due_at(when, &now_z);
        let due = due_z.timestamp().as_second();
        let text: String = text.chars().take(MAX_TEXT).collect();
        let left = duration_text(Duration::from_secs(due.saturating_sub(now).max(0) as u64));
        let tomorrow = now_z.date().tomorrow().ok();
        let day = if Some(due_z.date()) == tomorrow { "tomorrow, " } else { "" };
        row(
            format!("set:{due}:r:{text}"),
            format!("Remind at {} \u{2014} {text}", self.clock_text(due)),
            format!("{day}in {left}"),
            false,
        )
    }

    fn pending_rows(&self) -> Vec<ResultItem> {
        let now = self.now_secs();
        let mut rows: Vec<ResultItem> = self
            .reminders
            .pending()
            .into_iter()
            .map(|r| {
                let left = duration_text(Duration::from_secs(r.due.saturating_sub(now).max(0) as u64));
                row(
                    format!("cancel:{}", r.id),
                    r.text,
                    format!("rings at {} \u{2014} {left} left", self.clock_text(r.due)),
                    true,
                )
            })
            .collect();
        if rows.is_empty() {
            rows.push(inert(NONE_PENDING));
        }
        rows
    }

    fn set(&self, rest: &str) -> Result<Outcome, String> {
        let mut parts = rest.splitn(3, ':');
        let (Some(due), Some(kind), Some(text)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(unknown());
        };
        let due: i64 = due.parse().map_err(|_| unknown())?;
        if kind != "r" {
            return Err(unknown());
        }
        self.reminders.add(from_unix(due), text, false)?;
        Ok(Outcome::Hide)
    }

    /// A timer of `secs` seconds, due that long after Enter (not after the row
    /// was shown), labelled with the duration as typed.
    fn start_timer(&self, secs: &str) -> Result<Outcome, String> {
        let secs: u64 = secs.parse().map_err(|_| unknown())?;
        let d = Duration::from_secs(secs);
        if d > MAX_AHEAD {
            return Err(unknown());
        }
        let due = self.now_secs() + secs as i64;
        self.reminders.add(from_unix(due), &timer_label(d), true)?;
        Ok(Outcome::Hide)
    }
}

/// `Timer 5 min`.
fn timer_label(d: Duration) -> String {
    format!("Timer {}", duration_text(d))
}

fn unknown() -> String {
    "unknown timer row".to_string()
}

fn row(id: String, title: String, subtitle: String, confirm: bool) -> ResultItem {
    ResultItem { id, provider: NAME, title, subtitle, confirm, icon: ICON.into(), ..Default::default() }
}

fn inert(title: &str) -> ResultItem {
    row(format!("{INERT}{title}"), title.to_string(), String::new(), false)
}

fn help(example: &str, about: &str, fill: &str) -> HelpLine {
    HelpLine { example: example.into(), about: about.into(), fill: fill.into() }
}

impl Provider for TimersProvider {
    fn name(&self) -> &'static str {
        NAME
    }

    /// Memory only.
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let (word, rest) = first_word(&q.text);
        let mut rows = match word.to_lowercase().as_str() {
            "timer" => vec![self.timer_row(rest)],
            "remind" => vec![self.remind_row(rest)],
            "timers" | "reminders" => self.pending_rows(),
            _ => vec![],
        };
        score_in_order(&mut rows);
        rows
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        if id.starts_with(INERT) {
            return Ok(Outcome::Keep);
        }
        if let Some(secs) = id.strip_prefix(TIMER) {
            return self.start_timer(secs);
        }
        if let Some(rest) = id.strip_prefix("set:") {
            return self.set(rest);
        }
        if let Some(n) = id.strip_prefix("cancel:") {
            let n: u64 = n.parse().map_err(|_| unknown())?;
            self.reminders.cancel(n);
            (self.on_ready)();
            return Ok(Outcome::Keep);
        }
        Err(unknown())
    }

    /// Rows carry typed text: never learned or saved.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn item(&self, _id: &str) -> Option<ResultItem> {
        None
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![
            help("timer 5m", "a timer that rings with a Windows notification", "timer"),
            help("remind 20m pizza / remind 18:30 call mum", "a reminder after a while or at a time", "remind"),
            help("timers", "pending timers and reminders; Enter twice cancels one", "timers"),
        ]
    }

    /// The first word (any case) is `timer`, `remind`, `timers` or `reminders`,
    /// followed by a space or the end.
    fn claims(&self, text: &str) -> bool {
        let word = text.trim().split(char::is_whitespace).next().unwrap_or("");
        WORDS.iter().any(|w| w.eq_ignore_ascii_case(word))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::SystemTime;

    use crate::reminders::Fire;

    struct Rig {
        _dir: tempfile::TempDir,
        reminders: Arc<Reminders>,
        p: TimersProvider,
        ready: Arc<AtomicUsize>,
        now: Arc<std::sync::Mutex<SystemTime>>,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let t0 = SystemTime::from("2026-11-17T14:27:00Z".parse::<jiff::Timestamp>().unwrap());
        let time = Arc::new(std::sync::Mutex::new(t0));
        let clock = Arc::clone(&time);
        let now: Clock = Arc::new(move || *clock.lock().unwrap());
        let fire: Fire = Arc::new(|_: &str| {});
        let reminders = Reminders::new(dir.path().join("reminders.json"), Arc::clone(&now), fire);
        let ready = Arc::new(AtomicUsize::new(0));
        let r = Arc::clone(&ready);
        let on_ready = Arc::new(move || {
            r.fetch_add(1, Ordering::SeqCst);
        });
        let p = TimersProvider::new(Arc::clone(&reminders), now, jiff::tz::TimeZone::UTC, on_ready);
        Rig { _dir: dir, reminders, p, ready, now: time }
    }

    fn rows(p: &TimersProvider, text: &str) -> Vec<ResultItem> {
        p.query(&Query { text: text.into(), scope: None })
    }

    fn titles(p: &TimersProvider, text: &str) -> Vec<String> {
        rows(p, text).into_iter().map(|r| r.title).collect()
    }

    #[test]
    fn timer_shows_when_it_rings() {
        let g = rig();
        let r = rows(&g.p, "timer 5m");
        assert_eq!(r.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["Timer 5 min \u{2014} rings at 14:32"]);
        assert_eq!(r[0].subtitle, "Enter starts it");
        assert_eq!(r[0].icon, "builtin:timer");
        assert_eq!(r[0].id, "timer:300");
        assert_eq!(r[0].provider, "timers");
    }

    #[test]
    fn a_timer_counts_from_enter_not_from_the_row() {
        let g = rig();
        let id = rows(&g.p, "timer 5m")[0].id.clone();
        // A while passes between the row and Enter.
        *g.now.lock().unwrap() += Duration::from_secs(40);
        assert_eq!(g.p.activate(&id, Modifier::None), Ok(Outcome::Hide));
        let pending = g.reminders.pending();
        let due = "2026-11-17T14:32:40Z".parse::<jiff::Timestamp>().unwrap().as_second();
        assert_eq!((pending[0].due, pending[0].text.as_str()), (due, "Timer 5 min"));
        assert!(pending[0].timer);
        assert_eq!(g.p.activate("timer:999999999", Modifier::None), Err(unknown()));
        assert_eq!(g.p.activate("timer:x", Modifier::None), Err(unknown()));
        assert_eq!(g.p.activate("set:1:t:", Modifier::None), Err(unknown()));
        assert_eq!(g.reminders.pending().len(), 1);
    }

    #[test]
    fn remind_rows() {
        let g = rig();
        let r = rows(&g.p, "remind 20m pizza");
        assert_eq!(r[0].title, "Remind at 14:47 \u{2014} pizza");
        assert_eq!(r[0].subtitle, "in 20 min");
        assert_eq!(r[0].icon, "builtin:timer");
        let due = "2026-11-17T14:47:00Z".parse::<jiff::Timestamp>().unwrap().as_second();
        assert_eq!(r[0].id, format!("set:{due}:r:pizza"));
        let r = rows(&g.p, "remind 13:00 call mum");
        assert_eq!(r[0].title, "Remind at 13:00 \u{2014} call mum");
        assert_eq!(r[0].subtitle, "tomorrow, in 22 h 33 min");
        let r = rows(&g.p, "Remind tomorrow 15:00 dentist");
        assert_eq!(r[0].title, "Remind at 15:00 \u{2014} dentist");
        assert_eq!(r[0].subtitle, "tomorrow, in 1 d");
        assert_eq!(titles(&g.p, "remind 5m"), ["Remind at 14:32 \u{2014} Reminder"]);
        // Only the very next day says "tomorrow".
        let r = rows(&g.p, "remind 72h x");
        assert_eq!(r[0].title, "Remind at 14:27 \u{2014} x");
        assert_eq!(r[0].subtitle, "in 3 d");
        assert_eq!(rows(&g.p, "remind 34h x")[0].subtitle, "in 1 d 10 h");
        assert_eq!(rows(&g.p, "remind 10h x")[0].subtitle, "tomorrow, in 10 h");
    }

    #[test]
    fn bare_or_unparsable_words_say_how() {
        let g = rig();
        let timer_usage = ["Type a time, like timer 5m"];
        let remind_usage = ["Type a time and a note, like remind 20m pizza"];
        assert_eq!(titles(&g.p, "timer"), timer_usage);
        assert_eq!(titles(&g.p, "timer soon"), timer_usage);
        assert_eq!(titles(&g.p, "remind"), remind_usage);
        assert_eq!(titles(&g.p, "remind pizza"), remind_usage);
        assert_eq!(titles(&g.p, "timer 200h"), ["Up to 7 days ahead"]);
        assert_eq!(titles(&g.p, "remind 200h pizza"), ["Up to 7 days ahead"]);
        let r = rows(&g.p, "timer");
        assert_eq!(r[0].id, "inert:Type a time, like timer 5m");
        assert_eq!(r[0].icon, "builtin:timer");
        assert_eq!(g.p.activate(&r[0].id, Modifier::None), Ok(Outcome::Keep));
    }

    #[test]
    fn enter_sets_it_and_hides() {
        let g = rig();
        let id = rows(&g.p, "timer 5m")[0].id.clone();
        assert_eq!(g.p.activate(&id, Modifier::None), Ok(Outcome::Hide));
        let pending = g.reminders.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].text, "Timer 5 min");
        assert!(pending[0].timer);
        let id = rows(&g.p, "remind 20m pizza: hot")[0].id.clone();
        assert_eq!(g.p.activate(&id, Modifier::None), Ok(Outcome::Hide));
        let pending = g.reminders.pending();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[1].text, "pizza: hot");
        assert!(!pending[1].timer);
    }

    #[test]
    fn timers_lists_pending_with_time_left_and_two_enters_cancel() {
        let g = rig();
        let id = rows(&g.p, "timer 5m")[0].id.clone();
        g.p.activate(&id, Modifier::None).unwrap();
        let r = rows(&g.p, "timers");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "Timer 5 min");
        assert_eq!(r[0].subtitle, "rings at 14:32 \u{2014} 5 min left");
        assert!(r[0].confirm);
        assert_eq!(r[0].icon, "builtin:timer");
        assert_eq!(r[0].id, format!("cancel:{}", g.reminders.pending()[0].id));
        assert_eq!(rows(&g.p, "REMINDERS")[0].title, "Timer 5 min");
        assert_eq!(g.p.activate(&r[0].id, Modifier::None), Ok(Outcome::Keep));
        assert!(g.reminders.pending().is_empty());
        assert_eq!(g.ready.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn no_pending_says_so() {
        let g = rig();
        assert_eq!(titles(&g.p, "timers"), ["No timers or reminders"]);
        assert_eq!(titles(&g.p, "reminders"), ["No timers or reminders"]);
    }

    #[test]
    fn only_these_words_are_claimed() {
        let g = rig();
        for yes in ["timer", "Timer 5m", "remind x", "timers", "REMINDERS", "  timer 5m ", " timers"] {
            assert!(g.p.claims(yes), "{yes}");
        }
        for no in ["timeline", "reminder app", "remindme", "timers5", ""] {
            assert!(!g.p.claims(no), "{no}");
        }
    }

    #[test]
    fn rows_are_never_learned() {
        let g = rig();
        for text in ["timer 5m", "remind 20m pizza", "timers"] {
            for r in rows(&g.p, text) {
                assert_eq!(g.p.rank_key(&r.id), None);
                assert!(g.p.item(&r.id).is_none());
            }
        }
        assert_eq!(g.p.rank_key("set:1:r:pizza"), None);
        assert_eq!(g.p.rank_key("timer:300"), None);
        assert_eq!(g.p.name(), "timers");
    }

    #[test]
    fn help_has_three_lines() {
        let g = rig();
        let h = g.p.help();
        let fills: Vec<&str> = h.iter().map(|l| l.fill.as_str()).collect();
        assert_eq!(fills, ["timer", "remind", "timers"]);
        assert_eq!(h[0].example, "timer 5m");
        assert_eq!(h[1].about, "a reminder after a while or at a time");
        assert_eq!(h[2].about, "pending timers and reminders; Enter twice cancels one");
    }
}
