//! Quick answers in the bar: maths, units, currency, time, dates and weather.
//!
//! The query path reads memory only: the rate and weather stores answer from what
//! they hold and start their own background fetches.

use std::sync::{Arc, RwLock};
use std::time::Instant;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::Timestamp;

use crate::answers::calc;
use crate::answers::clock::{self, ClockAsk, Place, Span};
use crate::answers::number::Formats;
use crate::answers::rates::{RatesState, RatesStore};
use crate::answers::units::{self, Ask, Unit};
use crate::answers::weather::{describe, Forecast, WeatherState, WeatherStore};
pub use crate::clipboard::Clipboard;
use crate::launch::Launcher;
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem};
use crate::providers::flick::Opener;
use crate::providers::shortcuts::encode_url_component;

const NAME: &str = "answers";
/// Computed answers; the engine also puts them above every other row.
const TOP_SCORE: u32 = 1000;
/// Bare keywords and bare `20 usd`: like an exact name, so an app named so can win.
const NAME_SCORE: u32 = 900;
/// Placeholders and error rows: Enter does nothing.
const WAIT: &str = "wait";
/// "Set your city": Enter opens the config.
const CONFIG: &str = "config";
const COPY: &str = "copy:";
const OPEN: &str = "open:";

/// Default for `search_engine`.
pub const DUCKDUCKGO: &str = "https://duckduckgo.com/?q={arg}";

/// The current time; injected so tests are fixed in time.
pub type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// The config keys for answers, with defaults already resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct AnswersConfig {
    pub online: bool,
    /// Empty: no home city.
    pub weather_city: String,
    /// Uppercase ISO code for a bare `20 usd`.
    pub currency: String,
    pub fahrenheit: bool,
    /// HTTPS URL containing `{arg}`.
    pub search_engine: String,
}

impl Default for AnswersConfig {
    fn default() -> Self {
        AnswersConfig {
            online: true,
            weather_city: String::new(),
            currency: "EUR".into(),
            fahrenheit: false,
            search_engine: DUCKDUCKGO.into(),
        }
    }
}

pub struct AnswersProvider {
    formats: Arc<dyn Formats>,
    rates: Arc<RatesStore>,
    weather: Arc<WeatherStore>,
    clipboard: Arc<dyn Clipboard>,
    launcher: Arc<dyn Launcher>,
    open_config: Opener,
    clock: Clock,
    /// The PC's zone, read once at start.
    local: TimeZone,
    config: RwLock<AnswersConfig>,
}

enum Icon {
    Calc,
    Currency,
    Time,
    Weather,
}

impl Icon {
    fn key(&self) -> &'static str {
        match self {
            Icon::Calc => "builtin:calc",
            Icon::Currency => "builtin:currency",
            Icon::Time => "builtin:time",
            Icon::Weather => "builtin:weather",
        }
    }
}

/// One answer row: never pinnable, never learned.
fn row(id: String, title: String, subtitle: String, icon: Icon, top: bool) -> ResultItem {
    ResultItem {
        id,
        provider: NAME,
        title,
        subtitle,
        score: if top { TOP_SCORE } else { NAME_SCORE },
        icon: icon.key().into(),
        top,
        ..Default::default()
    }
}

/// A row whose Enter copies `copy`.
fn copy_row(copy: &str, title: String, subtitle: String, icon: Icon, top: bool) -> ResultItem {
    row(format!("{COPY}{copy}"), title, subtitle, icon, top)
}

/// An inert row (placeholder or error).
fn wait_row(title: impl Into<String>, subtitle: impl Into<String>, icon: Icon, top: bool) -> ResultItem {
    row(WAIT.into(), title.into(), subtitle.into(), icon, top)
}

/// Shown result: two decimals from 1 up, three significant digits below, so
/// `8.04672` reads `8.05` and `0.000001` is not rounded away. None when not finite.
fn round_answer(x: f64) -> Option<f64> {
    if !x.is_finite() {
        return None;
    }
    let r = if x == 0.0 {
        0.0
    } else if x.abs() >= 1.0 {
        (x * 100.0).round() / 100.0
    } else {
        let factor = 10f64.powi(2 - x.abs().log10().floor() as i32);
        (x * factor).round() / factor
    };
    r.is_finite().then_some(r + 0.0)
}

/// `mile` / `miles`, `degree Celsius` / `degrees Celsius`, `foot` / `feet`.
fn unit_name(u: &Unit, amount: f64) -> String {
    if amount == 1.0 {
        return u.name.to_string();
    }
    let plural = |word: &str| match word {
        "inch" => "inches".to_string(),
        "kelvin" => "kelvin".to_string(),
        w if w.ends_with("foot") => format!("{}feet", &w[..w.len() - 4]),
        w => format!("{w}s"),
    };
    if let Some(rest) = u.name.strip_prefix("degree ") {
        return format!("degrees {rest}");
    }
    if let Some((first, per)) = u.name.split_once(" per ") {
        return format!("{} per {per}", plural(first));
    }
    plural(u.name)
}

/// Rows name the place: a zone abbreviation with summer time would be wrong half the year.
fn place_name(p: &Place) -> &'static str {
    match p.name {
        "EST" => "New York",
        "PST" => "Los Angeles",
        "CET" => "Central Europe",
        n => n,
    }
}

/// `8 h ahead`, `30 min behind`, `5 h 30 min ahead`, `same time`.
fn offset_text(minutes: i64) -> String {
    if minutes == 0 {
        return "same time".into();
    }
    let (h, m) = (minutes.abs() / 60, minutes.abs() % 60);
    let amount = match (h, m) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    };
    format!("{amount} {}", if minutes > 0 { "ahead" } else { "behind" })
}

/// `in 45 days`, `2 days ago`, `in 1 week`; `number` writes the amount in the locale format.
fn span_words(amount: i64, unit: Span, number: &dyn Fn(f64) -> String) -> String {
    let word = match unit {
        Span::Days => "day",
        Span::Weeks => "week",
        Span::Months => "month",
        Span::Years => "year",
    };
    let n = amount.unsigned_abs();
    let s = if n == 1 { "" } else { "s" };
    let n = number(n as f64);
    if amount < 0 {
        format!("{n} {word}{s} ago")
    } else {
        format!("in {n} {word}{s}")
    }
}

/// `$20`, `20€`: a currency symbol makes it a currency question on its own.
fn has_symbol(text: &str) -> bool {
    text.contains(['$', '€', '£', '¥'])
}

impl AnswersProvider {
    /// Cheap apart from reading the PC's time zone once.
    pub fn new(
        formats: Arc<dyn Formats>,
        rates: Arc<RatesStore>,
        weather: Arc<WeatherStore>,
        clipboard: Arc<dyn Clipboard>,
        launcher: Arc<dyn Launcher>,
        open_config: Opener,
        clock: Clock,
    ) -> Arc<Self> {
        Self::with_zone(formats, rates, weather, clipboard, launcher, open_config, clock, TimeZone::system())
    }

    /// Like `new` with an explicit local zone, so tests do not depend on the PC.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_zone(
        formats: Arc<dyn Formats>,
        rates: Arc<RatesStore>,
        weather: Arc<WeatherStore>,
        clipboard: Arc<dyn Clipboard>,
        launcher: Arc<dyn Launcher>,
        open_config: Opener,
        clock: Clock,
        local: TimeZone,
    ) -> Arc<Self> {
        Arc::new(AnswersProvider {
            formats,
            rates,
            weather,
            clipboard,
            launcher,
            open_config,
            clock,
            local,
            config: RwLock::new(AnswersConfig::default()),
        })
    }

    /// Memory only. The home city goes to the weather store separately (`set_home`).
    pub fn set_config(&self, c: &AnswersConfig) {
        *self.config.write().unwrap_or_else(|e| e.into_inner()) = c.clone();
    }

    fn config(&self) -> AnswersConfig {
        self.config.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Locale text of a finite number; None for NaN or infinity.
    fn number(&self, x: f64) -> Option<String> {
        x.is_finite().then(|| self.formats.number(x))
    }

    fn calc_row(&self, text: &str) -> Option<ResultItem> {
        let c = calc::evaluate_with(text, &|n| self.formats.number(n))?;
        let shown = self.number(c.value)?;
        Some(copy_row(&shown, format!("= {shown}"), c.shown, Icon::Calc, true))
    }

    fn clock_row(&self, ask: ClockAsk, now: Timestamp, today: Date) -> Option<ResultItem> {
        match ask {
            ClockAsk::LocalTime => {
                let here = now.to_zoned(self.local.clone());
                let time = self.formats.time(here.time());
                let title = format!("{time} {}", here.strftime("%a"));
                Some(copy_row(&time, title, self.formats.date(today, true), Icon::Time, false))
            }
            ClockAsk::TimeIn(place) => {
                let (there, ahead) = clock::time_in_from(place, now, &self.local)?;
                let time = self.formats.time(there.time());
                let title = format!("{time} {}", there.strftime("%a"));
                let subtitle = format!("{} · {}", place_name(place), offset_text(ahead));
                Some(copy_row(&time, title, subtitle, Icon::Time, true))
            }
            ClockAsk::Shift { amount, unit } => {
                let date = self.formats.date(clock::shift(today, amount, unit)?, true);
                Some(copy_row(&date, date.clone(), span_words(amount, unit, &|n| self.formats.number(n)), Icon::Time, true))
            }
            ClockAsk::DaysUntil(date) => {
                let days = today.until(date).ok()?.get_days();
                let n = self.number(f64::from(days.unsigned_abs()))?;
                let title = format!("{n} {}", if days.abs() == 1 { "day" } else { "days" });
                let when = if days < 0 { "since" } else { "until" };
                let subtitle = format!("{when} {}", self.formats.date(date, true));
                Some(copy_row(&n, title, subtitle, Icon::Time, true))
            }
        }
    }

    fn conversion_row(&self, ask: Ask, text: &str, now: Timestamp, cfg: &AnswersConfig) -> Option<ResultItem> {
        match ask {
            Ask::Unit { amount, from, to } => {
                let value = round_answer(units::convert(amount, from, to)?)?;
                let shown = self.number(value)?;
                let subtitle = format!("{} {} → {}", self.number(amount)?, unit_name(from, amount), to.symbol);
                Some(copy_row(&shown, format!("{shown} {}", to.symbol), subtitle, Icon::Calc, true))
            }
            Ask::Money { amount, from, to } => {
                // A bare `20 usd` could be an app's name: it ranks like one.
                let explicit = to.is_some();
                let top = explicit || has_symbol(text);
                let to = to.unwrap_or_else(|| cfg.currency.clone());
                // `20 eur` or `€20` at home: nothing to convert (and no fetch).
                if !explicit && from == to {
                    return None;
                }
                let question = format!("{} {from} → {to}", self.number(amount)?);
                let rates = match self.rates.lookup(now, cfg.online) {
                    RatesState::Off(_) => return Some(wait_row("Online answers are off", question, Icon::Currency, top)),
                    RatesState::Fetching => return Some(wait_row("Getting rates…", question, Icon::Currency, top)),
                    RatesState::Offline(None) => {
                        return Some(wait_row("Can't get rates (offline)", question, Icon::Currency, top))
                    }
                    RatesState::Ready(r) | RatesState::Offline(Some(r)) => r,
                };
                let value = round_answer(rates.convert(amount, &from, &to)?)?;
                let shown = self.number(value)?;
                let subtitle = format!("{question} · rates of {}", self.formats.date(rates.date, false));
                Some(copy_row(&shown, format!("{shown} {to}"), subtitle, Icon::Currency, top))
            }
        }
    }

    /// `rest` is the text after the `weather` keyword.
    fn weather_rows(&self, rest: &str) -> Option<ResultItem> {
        let cfg = self.config();
        let bare = rest.is_empty();
        let city = if bare { cfg.weather_city.trim().to_string() } else { rest.to_string() };
        if city.is_empty() {
            self.weather.cancel_pending();
            return Some(row(
                CONFIG.into(),
                "Set your city: add weather_city to the config".into(),
                "Enter opens the config".into(),
                Icon::Weather,
                false,
            ));
        }
        let top = !bare;
        let state = self.weather.lookup(&city, Instant::now(), cfg.online, cfg.fahrenheit);
        Some(match state {
            WeatherState::Ready(f) => {
                let mut r = self.weather_row(&f, &cfg);
                r.top = top;
                r.score = if top { TOP_SCORE } else { NAME_SCORE };
                r
            }
            WeatherState::Fetching(place) => {
                wait_row(format!("Getting weather for {place}…"), "weather", Icon::Weather, top)
            }
            WeatherState::NotFound(x) => wait_row(format!("No place called \"{x}\""), "weather", Icon::Weather, top),
            WeatherState::Offline => wait_row("Can't get the weather (offline)", city, Icon::Weather, top),
            WeatherState::Off => wait_row("Online answers are off", city, Icon::Weather, top),
        })
    }

    /// `12 °C, light rain` / `London · today 8 to 14 °C`; Enter searches the web
    /// for the place's forecast (Open-Meteo has no public forecast page).
    fn weather_row(&self, f: &Forecast, cfg: &AnswersConfig) -> ResultItem {
        let unit = if cfg.fahrenheit { "°F" } else { "°C" };
        let deg = |x: f64| self.formats.number(x.round() + 0.0);
        let url = cfg.search_engine.replace("{arg}", &encode_url_component(&format!("weather {}", f.place)));
        row(
            format!("{OPEN}{url}"),
            format!("{} {unit}, {}", deg(f.temp), describe(f.code)),
            format!("{} · today {} to {} {unit}", f.place, deg(f.low), deg(f.high)),
            Icon::Weather,
            true,
        )
    }
}

impl Provider for AnswersProvider {
    fn name(&self) -> &'static str {
        NAME
    }

    /// Weather, then time and dates, then conversions, then maths: the first that
    /// understands the text answers.
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if text.split_whitespace().next().is_some_and(|w| w.eq_ignore_ascii_case("weather")) {
            return self.weather_rows(text["weather".len()..].trim()).into_iter().collect();
        }
        // Only a weather question may still be fetched after the typing pause.
        self.weather.cancel_pending();
        let cfg = self.config();
        let now = (self.clock)();
        let today = now.to_zoned(self.local.clone()).date();
        let answer = if let Some(ask) = clock::parse_clock(text, today) {
            self.clock_row(ask, now, today)
        } else if let Some(ask) = units::parse_conversion(text) {
            self.conversion_row(ask, text, now, &cfg)
        } else {
            self.calc_row(text)
        };
        answer.into_iter().collect()
    }

    /// The bar was cleared: no weather question any more.
    fn empty_query(&self) -> Vec<ResultItem> {
        self.weather.cancel_pending();
        vec![]
    }

    /// The text went to a route, a scope or was blank: no weather question any more.
    fn skipped(&self) {
        self.weather.cancel_pending();
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        if id == WAIT {
            return Ok(Outcome::Keep);
        }
        if id == CONFIG {
            (self.open_config)()?;
            return Ok(Outcome::Hide);
        }
        if let Some(text) = id.strip_prefix(COPY) {
            self.clipboard.set_text(text)?;
            return Ok(Outcome::Hide);
        }
        if let Some(url) = id.strip_prefix(OPEN).filter(|u| u.starts_with("https://")) {
            self.launcher.open(url)?;
            return Ok(Outcome::Hide);
        }
        Err("unknown item".into())
    }

    /// Answers carry typed values: never learned, recorded or pinned.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn help(&self) -> Vec<HelpLine> {
        let line = |example: &str, about: &str| HelpLine {
            example: example.into(),
            about: about.into(),
            fill: example.into(),
        };
        vec![
            line("12*7", "work out a sum; Enter copies the answer"),
            line("5 km in miles", "convert units"),
            line("20 usd in eur", "convert money with the ECB's daily rates"),
            line("time tokyo", "the time in another place"),
            line("in 45 days", "the date in some days, weeks, months or years"),
            line("weather london", "the weather in a city; Enter searches its forecast"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::answers::number::TestFormats;
    use crate::clipboard::FakeClipboard;
    use crate::engine::Engine;
    use crate::history::History;
    use crate::launch::testing::RecordingLauncher;
    use crate::net::FakeFetch;
    use crate::pins::Pins;
    use crate::provider::{Modifier, Outcome, Provider, Query, ResultItem};
    use crate::ranking::Ranking;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::time::Duration;

    struct Rig {
        answers: Arc<AnswersProvider>,
        clipboard: Arc<FakeClipboard>,
        launcher: Arc<RecordingLauncher>,
        opened: Arc<AtomicUsize>,
        fetch: Arc<FakeFetch>,
        ready: mpsc::Receiver<()>,
        _dir: tempfile::TempDir,
    }

    /// 2026-11-17 18:00 UTC: the ECB file of that day is out.
    fn now() -> jiff::Timestamp {
        "2026-11-17T18:00:00Z".parse().unwrap()
    }

    fn config(online: bool, city: &str) -> AnswersConfig {
        AnswersConfig {
            online,
            weather_city: city.into(),
            currency: "EUR".into(),
            fahrenheit: false,
            search_engine: "https://duckduckgo.com/?q={arg}".into(),
        }
    }

    /// German formats, local zone Europe/Berlin, `rates` = date of stored rates (USD 1.08, GBP 0.86).
    fn rig(online: bool, city: &str, rates: Option<&str>) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        if let Some(date) = rates {
            let json = format!(r#"{{"version":1,"date":"{date}","per_eur":{{"USD":1.08,"GBP":0.86}}}}"#);
            std::fs::write(dir.path().join("rates.json"), json).unwrap();
        }
        let fetch = Arc::new(FakeFetch::offline());
        let (tx, ready) = mpsc::channel();
        let tx = Arc::new(Mutex::new(tx));
        let notify = {
            let tx = tx.clone();
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }) as Arc<dyn Fn() + Send + Sync>
        };
        let rates_store = RatesStore::new(dir.path().join("rates.json"), fetch.clone(), notify.clone());
        rates_store.load();
        let weather = WeatherStore::new(dir.path().join("weather.json"), fetch.clone(), notify);
        let clipboard = Arc::new(FakeClipboard::default());
        let launcher = Arc::new(RecordingLauncher::default());
        let opened = Arc::new(AtomicUsize::new(0));
        let counter = opened.clone();
        let answers = AnswersProvider::with_zone(
            Arc::new(TestFormats { decimal: ',', group: '.' }),
            rates_store,
            weather,
            clipboard.clone(),
            launcher.clone(),
            Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            Arc::new(now),
            jiff::tz::TimeZone::get("Europe/Berlin").unwrap(),
        );
        answers.set_config(&config(online, city));
        Rig { answers, clipboard, launcher, opened, fetch, ready, _dir: dir }
    }

    fn ask(r: &Rig, text: &str) -> Vec<ResultItem> {
        r.answers.query(&Query { text: text.into(), scope: None })
    }

    fn one(r: &Rig, text: &str) -> ResultItem {
        let rows = ask(r, text);
        assert_eq!(rows.len(), 1, "{text}: {:?}", rows.iter().map(|r| &r.title).collect::<Vec<_>>());
        rows.into_iter().next().unwrap()
    }

    fn titled(r: &Rig, text: &str) -> (String, String) {
        let row = one(r, text);
        (row.title, row.subtitle)
    }

    /// A plain provider with one row of `score`.
    struct App(&'static str, u32);

    impl Provider for App {
        fn name(&self) -> &'static str {
            "apps"
        }
        fn query(&self, _q: &Query) -> Vec<ResultItem> {
            vec![ResultItem { id: self.0.into(), provider: "apps", title: self.0.into(), score: self.1, ..Default::default() }]
        }
        fn activate(&self, _id: &str, _m: Modifier) -> Result<Outcome, String> {
            Ok(Outcome::Hide)
        }
    }

    fn engine(answers: Arc<AnswersProvider>, app: App, dir: &tempfile::TempDir) -> (Engine, Arc<Ranking>, Arc<History>) {
        let ranking = Arc::new(Ranking::load(dir.path().join("picks.json")));
        let history = Arc::new(History::load(dir.path().join("history.json"), true));
        let pins = Arc::new(Pins::load(dir.path().join("pins.json")));
        let e = Engine::new(vec![Box::new(answers), Box::new(app)], ranking.clone(), history.clone(), pins);
        (e, ranking, history)
    }

    #[test]
    fn maths_row_on_top_and_copies() {
        let r = rig(true, "", None);
        let dir = tempfile::tempdir().unwrap();
        let (e, ranking, _) = engine(r.answers.clone(), App("12*7 notes", 1000), &dir);
        for _ in 0..50 {
            ranking.record("apps", "12*7 notes");
        }
        let rows = e.query("12*7", None);
        assert_eq!((rows[0].title.as_str(), rows[0].provider), ("= 84", "answers"));
        assert_eq!(rows[0].subtitle, "12 * 7");
        assert_eq!(rows[0].icon, "builtin:calc");
        assert!(!rows[0].pinnable);
        assert_eq!(rows[1].provider, "apps");
        assert_eq!(e.activate("answers", &rows[0].id, Modifier::None, "12*7", None), Ok(Outcome::Hide));
        assert_eq!(*r.clipboard.0.lock().unwrap(), ["84"]);
        assert!(r.launcher.calls().is_empty());
    }

    #[test]
    fn german_formatting_in_rows() {
        let r = rig(true, "", None);
        assert_eq!(titled(&r, "5 miles in km"), ("8,05 km".into(), "5 miles → km".into()));
        assert_eq!(titled(&r, "1234,5*1"), ("= 1.234,5".into(), "1.234,5 * 1".into()));
        assert_eq!(titled(&r, "0,1+0,2"), ("= 0,3".into(), "0,1 + 0,2".into()));
        assert_eq!(one(&r, "in 1500 days").subtitle, "in 1.500 days");
        assert_eq!(one(&r, "1500 days ago").subtitle, "1.500 days ago");
        assert_eq!(one(&r, "1 mile in km").subtitle, "1 mile → km");
        assert_eq!(one(&r, "5 miles in km").icon, "builtin:calc");
        let row = one(&r, "5 miles in km");
        assert!(row.top);
        r.answers.activate(&row.id, Modifier::None).unwrap();
        assert_eq!(*r.clipboard.0.lock().unwrap(), ["8,05"]);
        // Tiny results keep three significant digits instead of rounding to 0.
        assert_eq!(one(&r, "1 mm in km").title, "0,000001 km");
    }

    #[test]
    fn non_questions_give_no_rows() {
        let r = rig(true, "", None);
        for text in ["2024", "1.5.3", "pi", "firefox", "timer", "5 km", "weatherman", "1/0"] {
            assert!(ask(&r, text).is_empty(), "{text}");
        }
    }

    #[test]
    fn answers_are_exact_to_fifteen_digits() {
        let r = rig(true, "", None);
        assert_eq!(one(&r, "1 tib in bytes").title, "1.099.511.627.776 B");
        let row = one(&r, "12345678901+1");
        assert_eq!(row.title, "= 12.345.678.902");
        assert_eq!(row.subtitle, "12.345.678.901 + 1");
        r.answers.activate(&row.id, Modifier::None).unwrap();
        assert_eq!(*r.clipboard.0.lock().unwrap(), ["12.345.678.902"]);
        // Float noise stays hidden.
        assert_eq!(titled(&r, "0,1+0,2"), ("= 0,3".into(), "0,1 + 0,2".into()));
        assert_eq!(titled(&r, "1,1-1"), ("= 0,1".into(), "1,1 - 1".into()));
        assert_eq!(titled(&r, "0,3-0,1"), ("= 0,2".into(), "0,3 - 0,1".into()));
        assert_eq!(titled(&r, "10,1-10"), ("= 0,1".into(), "10,1 - 10".into()));
        assert_eq!(one(&r, "10000000000001*1").title, "= 10.000.000.000.001");
    }

    #[test]
    fn trig_rounding_noise_is_zero() {
        let r = rig(true, "", None);
        for text in ["sin(180)", "cos(90)", "tan(180)"] {
            assert_eq!(one(&r, text).title, "= 0", "{text}");
        }
        assert_eq!(one(&r, "sin(30)").title, "= 0,5");
    }

    #[test]
    fn currency_rows_by_state() {
        // Fresh rates: no request.
        let r = rig(true, "", Some("2026-11-17"));
        let row = one(&r, "20 usd in eur");
        assert_eq!(row.title, "18,52 EUR");
        assert_eq!(row.subtitle, "20 USD → EUR · rates of 17.11.2026");
        assert!(row.subtitle.contains("rates of"));
        assert_eq!(row.icon, "builtin:currency");
        assert!(row.top);
        assert!(r.fetch.urls().is_empty());
        assert_eq!(one(&r, "$20 in gbp").title, "15,93 GBP");
        r.answers.activate(&row.id, Modifier::None).unwrap();
        assert_eq!(*r.clipboard.0.lock().unwrap(), ["18,52"]);

        // No rates and offline: a placeholder, then the offline row.
        let r = rig(true, "", None);
        let wait = one(&r, "20 usd in eur");
        assert_eq!((wait.title.as_str(), wait.id.as_str()), ("Getting rates…", "wait"));
        assert!(wait.top);
        assert_eq!(r.answers.activate("wait", Modifier::None), Ok(Outcome::Keep));
        r.ready.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(one(&r, "20 usd in eur").title, "Can't get rates (offline)");

        // Offline with old rates: the old rates, with their date.
        let r = rig(true, "", Some("2026-11-10"));
        one(&r, "20 usd in eur");
        r.ready.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(one(&r, "20 usd in eur").subtitle, "20 USD → EUR · rates of 10.11.2026");

        // online = false: no request at all.
        let r = rig(false, "", Some("2026-11-10"));
        assert_eq!(one(&r, "20 usd in eur").title, "Online answers are off");
        assert!(r.fetch.urls().is_empty());
        assert!(r.clipboard.0.lock().unwrap().is_empty());
    }

    #[test]
    fn bare_money_ranks_like_a_name() {
        let r = rig(true, "", Some("2026-11-17"));
        let row = one(&r, "20 usd");
        assert_eq!(row.title, "18,52 EUR");
        assert!(!row.top);
        assert_eq!(row.score, 900);
        assert!(one(&r, "$20").top);
        // Home currency to itself is not a question, with or without a symbol.
        assert!(ask(&r, "20 eur").is_empty());
        assert!(ask(&r, "€20").is_empty());
        assert!(ask(&r, "20€").is_empty());
        // An explicit target still answers.
        assert_eq!(one(&r, "20 eur in eur").title, "20 EUR");
        let r = rig(true, "", None);
        assert!(ask(&r, "€20").is_empty());
        std::thread::sleep(Duration::from_millis(100));
        assert!(r.fetch.urls().is_empty(), "no ECB download for the home currency alone");
    }

    /// A provider that owns the `>` route and the `proj` scope.
    struct Routed;

    impl Provider for Routed {
        fn name(&self) -> &'static str {
            "sh"
        }
        fn route(&self) -> Option<crate::provider::Route> {
            Some(crate::provider::Route { prefix: ">", needs_space: false })
        }
        fn owns_scope(&self, scope: &str) -> bool {
            scope == "proj"
        }
        fn query(&self, _q: &Query) -> Vec<ResultItem> {
            vec![]
        }
        fn activate(&self, _id: &str, _m: Modifier) -> Result<Outcome, String> {
            Ok(Outcome::Hide)
        }
    }

    #[test]
    fn leaving_weather_by_route_scope_or_blank_drops_the_waiting_city() {
        let r = rig(true, "", None);
        let dir = tempfile::tempdir().unwrap();
        let ranking = Arc::new(Ranking::load(dir.path().join("picks.json")));
        let history = Arc::new(History::load(dir.path().join("history.json"), true));
        let pins = Arc::new(Pins::load(dir.path().join("pins.json")));
        let e = Engine::new(vec![Box::new(r.answers.clone()), Box::new(Routed)], ranking, history, pins);
        let leave: [(&str, Option<&str>); 4] = [("> htop", None), ("help", None), ("   ", None), ("x", Some("proj"))];
        for (text, scope) in leave {
            assert_eq!(e.query("weather lo", None)[0].title, "Getting weather for lo…");
            e.query(text, scope);
            std::thread::sleep(Duration::from_millis(700));
            assert!(r.fetch.urls().is_empty(), "{text:?} {scope:?}: {:?}", r.fetch.urls());
        }
        // Staying on the question still fetches after the pause.
        e.query("weather lo", None);
        r.ready.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(r.fetch.urls().len(), 1);
    }

    #[test]
    fn bare_keywords_rank_like_names() {
        let r = rig(true, "", None);
        let row = one(&r, "time");
        assert!(!row.top);
        assert_eq!(row.score, 900);
        assert_eq!(row.title, "19:00 Tue");
        assert_eq!(row.icon, "builtin:time");
        let dir = tempfile::tempdir().unwrap();
        let (e, _, _) = engine(r.answers.clone(), App("Time Tracker", 950), &dir);
        let rows = e.query("time", None);
        assert_eq!((rows[0].title.as_str(), rows[1].provider), ("Time Tracker", "answers"));
    }

    #[test]
    fn time_and_date_rows() {
        let r = rig(true, "", None);
        assert_eq!(titled(&r, "time tokyo"), ("03:00 Wed".into(), "Tokyo · 8 h ahead".into()));
        assert_eq!(one(&r, "time berlin").subtitle, "Berlin · same time");
        assert_eq!(one(&r, "time london").subtitle, "London · 1 h behind");
        assert_eq!(one(&r, "time india").subtitle, "Delhi · 4 h 30 min ahead");
        // Abbreviations name the place, never "EST".
        assert_eq!(one(&r, "time est").subtitle, "New York · 6 h behind");
        let row = one(&r, "time tokyo");
        assert!(row.top);
        r.answers.activate(&row.id, Modifier::None).unwrap();
        assert_eq!(*r.clipboard.0.lock().unwrap(), ["03:00"]);

        assert_eq!(titled(&r, "in 45 days"), ("Fri 1 Jan 2027".into(), "in 45 days".into()));
        assert_eq!(one(&r, "in 1 week").subtitle, "in 1 week");
        assert_eq!(titled(&r, "2 days ago"), ("Sun 15 Nov 2026".into(), "2 days ago".into()));
        assert_eq!(titled(&r, "days until 24.12."), ("37 days".into(), "until Thu 24 Dec 2026".into()));
        assert_eq!(one(&r, "in 45 days").icon, "builtin:time");
        assert!(ask(&r, "in 100000000 years").is_empty());
    }

    #[test]
    fn weather_without_city_points_to_config() {
        let r = rig(true, "", None);
        let row = one(&r, "weather");
        assert_eq!(row.title, "Set your city: add weather_city to the config");
        assert_eq!(row.id, "config");
        assert_eq!(row.icon, "builtin:weather");
        assert_eq!(r.answers.activate("config", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(r.opened.load(Ordering::SeqCst), 1);
        assert!(r.fetch.urls().is_empty());
    }

    #[test]
    fn weather_rows_by_state() {
        let r = rig(true, "London", None);
        let row = one(&r, "weather");
        assert_eq!(row.title, "Getting weather for London…");
        assert!(!row.top, "bare weather ranks like a name");
        assert_eq!(row.score, 900);
        let row = one(&r, "weather tokyo");
        assert_eq!((row.title.as_str(), row.id.as_str()), ("Getting weather for tokyo…", "wait"));
        assert!(row.top);

        let r = rig(false, "London", None);
        assert_eq!(one(&r, "weather london").title, "Online answers are off");
        assert!(r.fetch.urls().is_empty());
    }

    #[test]
    fn weather_row_texts() {
        let f = Forecast { place: "London".into(), temp: 12.3, code: 61, low: 8.0, high: 14.4 };
        let r = rig(true, "", None);
        let row = r.answers.weather_row(&f, &config(true, ""));
        assert_eq!(row.title, "12 °C, light rain");
        assert_eq!(row.subtitle, "London · today 8 to 14 °C");
        assert_eq!(row.id, "open:https://duckduckgo.com/?q=weather%20London");
        assert_eq!(r.answers.activate(&row.id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(r.launcher.calls(), ["open https://duckduckgo.com/?q=weather%20London"]);
        let cold = Forecast { place: "Oslo".into(), temp: -0.4, code: 0, low: -3.0, high: 1.0 };
        assert_eq!(r.answers.weather_row(&cold, &config(true, "")).title, "0 °C, clear");
    }

    #[test]
    fn answers_never_recorded() {
        let r = rig(true, "", None);
        let dir = tempfile::tempdir().unwrap();
        let (e, _, history) = engine(r.answers.clone(), App("x", 1), &dir);
        let id = e.query("12*7", None)[0].id.clone();
        e.activate("answers", &id, Modifier::None, "12*7", None).unwrap();
        assert!(history.searches().is_empty());
        assert!(history.recents().is_empty());
        assert!(!e.toggle_pin("answers", &id));
        assert!(!dir.path().join("picks.json").exists());
        assert_eq!(r.answers.rank_key(&id), None);
        assert!(r.answers.item(&id).is_none());
    }

    #[test]
    fn help_lists_every_kind() {
        let r = rig(true, "", None);
        let examples: Vec<String> = r.answers.help().into_iter().map(|l| l.example).collect();
        assert_eq!(examples, ["12*7", "5 km in miles", "20 usd in eur", "time tokyo", "in 45 days", "weather london"]);
    }
}
