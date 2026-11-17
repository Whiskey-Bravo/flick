//! Daily ECB reference rates: parsing, staleness, and a store that refreshes in the background.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::net::Fetch;

/// The ECB's daily reference rates (base EUR).
pub const ECB_URL: &str = "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml";

/// At most one fetch attempt per this long (kept in memory only).
const RETRY_AFTER: SignedDuration = SignedDuration::from_hours(1);

#[derive(Debug, Clone, PartialEq)]
pub struct Rates {
    pub date: Date,
    /// Units of each currency per 1 EUR. EUR itself is implicit.
    pub per_eur: HashMap<String, f64>,
}

impl Rates {
    /// Converts `amount` of `from` into `to`, crossing via EUR.
    pub fn convert(&self, amount: f64, from: &str, to: &str) -> Option<f64> {
        let out = amount / self.per_eur_of(from)? * self.per_eur_of(to)?;
        out.is_finite().then_some(out)
    }

    fn per_eur_of(&self, code: &str) -> Option<f64> {
        let code = code.to_ascii_uppercase();
        if code == "EUR" {
            Some(1.0)
        } else {
            self.per_eur.get(&code).copied()
        }
    }
}

fn valid_rate(rate: f64) -> bool {
    rate.is_finite() && rate > 0.0
}

fn valid_code(code: &str) -> bool {
    code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase())
}

/// Reads the ECB daily file by scanning `<Cube ...>` tags for their attributes.
/// Needs a date and at least one valid rate; bad rates are skipped.
pub fn parse_ecb(xml: &[u8]) -> Option<Rates> {
    let text = std::str::from_utf8(xml).ok()?;
    let mut date = None;
    let mut per_eur = HashMap::new();
    let mut rest = text;
    while let Some(at) = rest.find("<Cube") {
        rest = &rest[at + "<Cube".len()..];
        let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
        if let Some(time) = attr(tag, "time") {
            if date.is_some() {
                break; // A second day (the 90-day file): keep only the newest.
            }
            date = Some(time.parse::<Date>().ok()?);
        } else if let (Some(code), Some(rate)) = (attr(tag, "currency"), attr(tag, "rate")) {
            if let Ok(rate) = rate.parse::<f64>() {
                if date.is_some() && valid_code(code) && valid_rate(rate) {
                    per_eur.insert(code.to_string(), rate);
                }
            }
        }
    }
    let date = date?;
    (!per_eur.is_empty()).then_some(Rates { date, per_eur })
}

/// The value of `name='…'` or `name="…"` inside a tag's text.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(i) = tag[from..].find(name) {
        let start = from + i;
        let preceded = tag[..start].ends_with(|c: char| c.is_ascii_whitespace());
        if preceded {
            if let Some(v) = tag[start + name.len()..].strip_prefix('=') {
                let quote = v.chars().next().filter(|c| *c == '\'' || *c == '"')?;
                let v = &v[1..];
                return v.find(quote).map(|end| &v[..end]);
            }
        }
        from = start + name.len();
    }
    None
}

fn berlin() -> &'static TimeZone {
    static ZONE: OnceLock<TimeZone> = OnceLock::new();
    ZONE.get_or_init(|| {
        TimeZone::get("Europe/Berlin").unwrap_or_else(|_| TimeZone::fixed(jiff::tz::offset(1)))
    })
}

fn working_day(d: Date) -> bool {
    !matches!(d.weekday(), Weekday::Saturday | Weekday::Sunday)
}

/// The last working day (Mon–Fri) whose 16:00 Europe/Berlin has passed.
pub fn latest_publication(now: Timestamp) -> Date {
    let here = now.to_zoned(berlin().clone());
    let mut day = here.date();
    if here.hour() < 16 {
        day = day.yesterday().unwrap_or(day);
    }
    while !working_day(day) {
        match day.yesterday() {
            Ok(d) => day = d,
            Err(_) => break,
        }
    }
    day
}

#[derive(Debug, Clone)]
pub enum RatesState {
    /// Rates to use; possibly stale while a refresh runs (the date shows how old).
    Ready(Arc<Rates>),
    /// No rates yet; a fetch is running.
    Fetching,
    /// The last attempt failed; old rates if any.
    Offline(Option<Arc<Rates>>),
    /// `online = false`: no requests; old rates if any.
    #[allow(dead_code)] // the bar says answers are off rather than using old rates
    Off(Option<Arc<Rates>>),
}

/// On-disk form of `rates.json`.
#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    date: String,
    per_eur: HashMap<String, f64>,
}

impl Stored {
    fn from_rates(r: &Rates) -> Self {
        Stored { version: 1, date: r.date.to_string(), per_eur: r.per_eur.clone() }
    }

    fn into_rates(self) -> Option<Rates> {
        if self.version != 1 {
            return None;
        }
        let date = self.date.parse::<Date>().ok()?;
        let ok = !self.per_eur.is_empty()
            && self.per_eur.iter().all(|(c, r)| valid_code(c) && valid_rate(*r));
        ok.then_some(Rates { date, per_eur: self.per_eur })
    }
}

#[derive(Default)]
struct State {
    rates: Option<Arc<Rates>>,
    /// When the last fetch attempt started (in memory only; a restart resets it).
    last_attempt: Option<Timestamp>,
    /// Counts attempts; a fetch thread only settles `fetching`/`failed` for its own.
    attempt: u64,
    fetching: bool,
    /// The last finished attempt failed.
    failed: bool,
}

/// Currency rates kept in memory, loaded from and saved to `rates.json`,
/// refreshed in the background when a lookup finds them stale.
pub struct RatesStore {
    path: PathBuf,
    fetch: Arc<dyn Fetch>,
    on_ready: Arc<dyn Fn() + Send + Sync>,
    state: Arc<Mutex<State>>,
}

impl RatesStore {
    /// Cheap: no disk access. Call `load` once, off the query path.
    pub fn new(path: PathBuf, fetch: Arc<dyn Fetch>, on_ready: Arc<dyn Fn() + Send + Sync>) -> Arc<Self> {
        Arc::new(RatesStore { path, fetch, on_ready, state: Arc::new(Mutex::new(State::default())) })
    }

    /// Reads `rates.json`; missing or corrupt means no rates.
    pub fn load(&self) {
        let loaded = std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Stored>(&bytes).ok())
            .and_then(Stored::into_rates);
        if let Some(rates) = loaded {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.rates.as_ref().is_none_or(|r| r.date < rates.date) {
                st.rates = Some(Arc::new(rates));
            }
        }
    }

    /// Query path: reads memory only. When the rates are older than the latest ECB
    /// publication, `online` is set and no attempt started in the last hour, starts one
    /// fetch on a background thread. A fetch still running after an hour no longer
    /// blocks a new attempt.
    pub fn lookup(&self, now: Timestamp, online: bool) -> RatesState {
        let latest = latest_publication(now);
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let rates = st.rates.clone();
        if !online {
            return RatesState::Off(rates);
        }
        let stale = rates.as_ref().is_none_or(|r| r.date < latest);
        if !stale {
            return rates.map_or(RatesState::Fetching, RatesState::Ready);
        }
        let recent = st.last_attempt.is_some_and(|t| {
            let since = now.duration_since(t);
            since >= SignedDuration::ZERO && since < RETRY_AFTER
        });
        if recent {
            return if st.failed && !st.fetching {
                RatesState::Offline(rates)
            } else {
                rates.map_or(RatesState::Fetching, RatesState::Ready)
            };
        }
        st.attempt += 1;
        let attempt = st.attempt;
        st.fetching = true;
        st.failed = false;
        st.last_attempt = Some(now);
        drop(st);
        if self.spawn_fetch(attempt) {
            rates.map_or(RatesState::Fetching, RatesState::Ready)
        } else {
            (self.on_ready)();
            RatesState::Offline(rates)
        }
    }

    /// One fetch on its own thread: swap in and save on success, then `on_ready` once.
    /// Returns false (and records the failure) when the thread could not start.
    fn spawn_fetch(&self, attempt: u64) -> bool {
        let path = self.path.clone();
        let fetch = self.fetch.clone();
        let on_ready = self.on_ready.clone();
        let state = self.state.clone();
        let spawned = std::thread::Builder::new().name("flick-rates".into()).spawn(move || {
            let got = fetch.get(ECB_URL).ok().and_then(|body| parse_ecb(&body)).map(Arc::new);
            let swapped = {
                let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                if st.attempt == attempt {
                    st.fetching = false;
                    st.failed = got.is_none();
                }
                match &got {
                    Some(rates) if st.rates.as_ref().is_none_or(|r| r.date <= rates.date) => {
                        st.rates = Some(rates.clone());
                        true
                    }
                    _ => false,
                }
            };
            if let (true, Some(rates)) = (swapped, &got) {
                // Not saved = fetched again after a restart; memory still has the rates.
                let _ = crate::ranking::save_json(&path, &Stored::from_rates(rates));
            }
            on_ready();
        });
        if spawned.is_err() {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.attempt == attempt {
                st.fetching = false;
                st.failed = true;
            }
        }
        spawned.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{FakeFetch, FetchError};
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::time::Duration;

    const FIXTURE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<gesmes:Envelope xmlns:gesmes="http://www.gesmes.org/xml/2002-08-01" xmlns="http://www.ecb.int/vocabulary/2002-08-01/eurofxref">
	<gesmes:subject>Reference rates</gesmes:subject>
	<gesmes:Sender>
		<gesmes:name>European Central Bank</gesmes:name>
	</gesmes:Sender>
	<Cube>
		<Cube time='2026-11-17'>
			<Cube currency='USD' rate='1.08'/>
			<Cube currency='GBP' rate='0.86'/>
		</Cube>
	</Cube>
</gesmes:Envelope>"#;

    fn date(y: i16, m: i8, d: i8) -> Date {
        jiff::civil::date(y, m, d)
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// A store in a temp dir with a fake fetcher; `on_ready` calls arrive on the receiver.
    fn store_with(
        dir: &tempfile::TempDir,
        fetch: Arc<FakeFetch>,
    ) -> (Arc<RatesStore>, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let store = RatesStore::new(
            dir.path().join("rates.json"),
            fetch,
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        (store, rx)
    }

    fn write_store(dir: &tempfile::TempDir, date: &str) {
        let json = format!(
            r#"{{"version":1,"date":"{date}","per_eur":{{"USD":1.1,"GBP":0.9}}}}"#
        );
        std::fs::write(dir.path().join("rates.json"), json).unwrap();
    }

    fn wait(rx: &mpsc::Receiver<()>) {
        rx.recv_timeout(Duration::from_secs(1)).expect("on_ready within 1 s");
    }

    #[test]
    fn parses_ecb_daily_file() {
        let r = parse_ecb(FIXTURE).unwrap();
        assert_eq!(r.date, date(2026, 11, 17));
        assert!((r.convert(20.0, "USD", "EUR").unwrap() - 18.518518).abs() < 1e-5);
        assert!((r.convert(10.0, "GBP", "USD").unwrap() - 12.558139).abs() < 1e-5);
        assert_eq!(r.convert(5.0, "EUR", "EUR"), Some(5.0));
        assert_eq!(r.convert(5.0, "EUR", "JPY"), None);
    }

    #[test]
    fn parses_double_quotes_and_rejects_garbage() {
        let r = parse_ecb(br#"<Cube time="2026-11-17"><Cube currency="JPY" rate="160.5"/></Cube>"#)
            .unwrap();
        assert_eq!(r.per_eur.get("JPY"), Some(&160.5));
        assert_eq!(parse_ecb(b""), None);
        assert_eq!(parse_ecb(b"<html>Service unavailable</html>"), None);
        // A date but no rates, or rates but no date.
        assert_eq!(parse_ecb(b"<Cube time='2026-11-17'></Cube>"), None);
        assert_eq!(parse_ecb(b"<Cube currency='USD' rate='1.08'/>"), None);
        // Bad rates are skipped, not trusted.
        let r = parse_ecb(
            b"<Cube time='2026-11-17'><Cube currency='USD' rate='x'/><Cube currency='GBP' rate='-1'/><Cube currency='CHF' rate='0.95'/></Cube>",
        )
        .unwrap();
        assert_eq!(r.per_eur.len(), 1);
        assert_eq!(parse_ecb(&[0xff, 0xfe, 0x00]), None);
    }

    #[test]
    fn publication_day() {
        assert_eq!(latest_publication(ts("2026-11-17T14:00:00Z")), date(2026, 11, 16));
        assert_eq!(latest_publication(ts("2026-11-17T16:00:00Z")), date(2026, 11, 17));
        assert_eq!(latest_publication(ts("2026-11-22T12:00:00Z")), date(2026, 11, 20));
        // Monday morning: last Friday. Summer time: 16:00 Berlin = 14:00 UTC.
        assert_eq!(latest_publication(ts("2026-11-23T09:00:00Z")), date(2026, 11, 20));
        assert_eq!(latest_publication(ts("2026-07-01T14:00:00Z")), date(2026, 7, 1));
        assert_eq!(latest_publication(ts("2026-07-01T13:59:59Z")), date(2026, 6, 30));
    }

    #[test]
    fn fetches_only_when_stale_and_once_per_hour() {
        // Fresh stored rates: no fetch.
        let dir = tempfile::tempdir().unwrap();
        write_store(&dir, "2026-11-17");
        let fetch = Arc::new(FakeFetch::offline());
        let (store, _rx) = store_with(&dir, fetch.clone());
        store.load();
        let now = ts("2026-11-17T18:00:00Z");
        assert!(matches!(store.lookup(now, true), RatesState::Ready(r) if r.date == date(2026, 11, 17)));
        assert_eq!(fetch.urls().len(), 0);

        // Stale: one fetch; again within the hour: still one; after the hour: two.
        let later = ts("2026-11-18T16:00:00Z");
        let first = store.lookup(later, true);
        assert!(matches!(first, RatesState::Ready(r) if r.date == date(2026, 11, 17)), "old rates stay usable");
        wait(&_rx);
        assert_eq!(fetch.urls(), vec![ECB_URL.to_string()]);
        assert!(matches!(
            store.lookup(ts("2026-11-18T16:59:00Z"), true),
            RatesState::Offline(Some(r)) if r.date == date(2026, 11, 17)
        ));
        assert_eq!(fetch.urls().len(), 1);
        store.lookup(ts("2026-11-18T17:00:00Z"), true);
        wait(&_rx);
        assert_eq!(fetch.urls().len(), 2);
    }

    #[test]
    fn no_second_fetch_while_one_runs() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(FakeFetch::offline().with_delay(Duration::from_millis(200)));
        let (store, rx) = store_with(&dir, fetch.clone());
        store.load();
        let now = ts("2026-11-18T16:00:00Z");
        assert!(matches!(store.lookup(now, true), RatesState::Fetching));
        assert!(matches!(store.lookup(now, true), RatesState::Fetching));
        wait(&rx);
        assert_eq!(fetch.urls().len(), 1);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "on_ready exactly once");
    }

    #[test]
    fn offline_without_rates_settles() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(FakeFetch::offline());
        let (store, rx) = store_with(&dir, fetch.clone());
        store.load();
        let now = ts("2026-11-18T16:00:00Z");
        assert!(matches!(store.lookup(now, true), RatesState::Fetching));
        wait(&rx);
        assert!(matches!(store.lookup(now, true), RatesState::Offline(None)));
        assert_eq!(fetch.urls().len(), 1);
    }

    #[test]
    fn bad_status_or_body_counts_as_failure() {
        for answer in [Err(FetchError::Status(503)), Ok(b"<html>oops</html>".to_vec())] {
            let dir = tempfile::tempdir().unwrap();
            let (store, rx) = store_with(&dir, Arc::new(FakeFetch::new(answer)));
            store.load();
            let now = ts("2026-11-18T16:00:00Z");
            store.lookup(now, true);
            wait(&rx);
            assert!(matches!(store.lookup(now, true), RatesState::Offline(None)));
            assert!(!dir.path().join("rates.json").exists());
        }
    }

    #[test]
    fn success_saves_and_swaps_in() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(FakeFetch::offline().respond(ECB_URL, Ok(FIXTURE.to_vec())));
        let (store, rx) = store_with(&dir, fetch.clone());
        store.load();
        let now = ts("2026-11-17T18:00:00Z");
        assert!(matches!(store.lookup(now, true), RatesState::Fetching));
        wait(&rx);
        assert!(matches!(store.lookup(now, true), RatesState::Ready(r) if r.date == date(2026, 11, 17)));
        assert_eq!(fetch.urls().len(), 1);

        // Saved: a fresh store reads it back without fetching.
        let fetch2 = Arc::new(FakeFetch::offline());
        let (store2, _rx2) = store_with(&dir, fetch2.clone());
        store2.load();
        match store2.lookup(now, true) {
            RatesState::Ready(r) => {
                assert_eq!(r.per_eur.get("USD"), Some(&1.08));
                assert_eq!(r.per_eur.get("GBP"), Some(&0.86));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fetch2.urls().len(), 0);
    }

    #[test]
    fn same_publication_is_retried_hourly_until_it_appears() {
        // ECB holiday: the newest file is older than the expected publication.
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(FakeFetch::offline().respond(ECB_URL, Ok(FIXTURE.to_vec())));
        let (store, rx) = store_with(&dir, fetch.clone());
        store.load();
        store.lookup(ts("2026-11-18T16:00:00Z"), true);
        wait(&rx);
        assert!(matches!(
            store.lookup(ts("2026-11-18T16:59:00Z"), true),
            RatesState::Ready(r) if r.date == date(2026, 11, 17)
        ));
        assert_eq!(fetch.urls().len(), 1, "no refetch within the hour");
        store.lookup(ts("2026-11-18T17:00:00Z"), true);
        wait(&rx);
        store.lookup(ts("2026-11-18T17:30:00Z"), true);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(fetch.urls().len(), 2, "exactly one refetch after the hour");
    }

    /// Answers each request with the next scripted body.
    struct Sequence(Mutex<Vec<Vec<u8>>>);

    impl crate::net::Fetch for Sequence {
        fn get_until(&self, _url: &str, _deadline: std::time::Instant) -> Result<Vec<u8>, FetchError> {
            let mut left = self.0.lock().unwrap();
            if left.is_empty() { Err(FetchError::Offline) } else { Ok(left.remove(0)) }
        }
    }

    #[test]
    fn fetched_before_upload_is_retried_after_an_hour() {
        // Just after 16:00 Berlin the ECB may still serve yesterday's file.
        let yesterday = String::from_utf8(FIXTURE.to_vec()).unwrap().replace("2026-11-17", "2026-11-16");
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(Sequence(Mutex::new(vec![yesterday.into_bytes(), FIXTURE.to_vec()])));
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let store = RatesStore::new(
            dir.path().join("rates.json"),
            fetch,
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        store.load();
        let after_four = ts("2026-11-17T15:01:00Z");
        assert!(matches!(store.lookup(after_four, true), RatesState::Fetching));
        wait(&rx);
        assert!(matches!(
            store.lookup(ts("2026-11-17T15:30:00Z"), true),
            RatesState::Ready(r) if r.date == date(2026, 11, 16)
        ));
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "no refetch within the hour");
        assert!(matches!(
            store.lookup(ts("2026-11-17T16:01:00Z"), true),
            RatesState::Ready(r) if r.date == date(2026, 11, 16)
        ));
        wait(&rx);
        assert!(matches!(
            store.lookup(ts("2026-11-17T16:02:00Z"), true),
            RatesState::Ready(r) if r.date == date(2026, 11, 17)
        ));
    }

    #[test]
    fn hung_fetch_does_not_block_retries_forever() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(FakeFetch::offline().with_delay(Duration::from_millis(300)));
        let (store, rx) = store_with(&dir, fetch.clone());
        store.load();
        assert!(matches!(store.lookup(ts("2026-11-18T16:00:00Z"), true), RatesState::Fetching));
        assert!(matches!(store.lookup(ts("2026-11-18T16:30:00Z"), true), RatesState::Fetching));
        assert!(matches!(store.lookup(ts("2026-11-18T17:00:00Z"), true), RatesState::Fetching));
        wait(&rx);
        wait(&rx);
        assert_eq!(fetch.urls().len(), 2);
        // Both finished; the newer attempt's failure is what shows.
        assert!(matches!(store.lookup(ts("2026-11-18T17:10:00Z"), true), RatesState::Offline(None)));
    }

    #[test]
    fn online_off_never_fetches() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(FakeFetch::offline());
        let (store, _rx) = store_with(&dir, fetch.clone());
        store.load();
        assert!(matches!(store.lookup(ts("2026-11-18T16:00:00Z"), false), RatesState::Off(None)));
        write_store(&dir, "2026-11-10");
        store.load();
        assert!(matches!(store.lookup(ts("2026-11-18T16:00:00Z"), false), RatesState::Off(Some(_))));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(fetch.urls().len(), 0);
    }

    #[test]
    fn corrupt_store_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rates.json"), "{").unwrap();
        let (store, _rx) = store_with(&dir, Arc::new(FakeFetch::offline()));
        store.load();
        assert!(matches!(store.lookup(ts("2026-11-18T16:00:00Z"), true), RatesState::Fetching));

        for bad in [
            r#"{"version":2,"date":"2026-11-17","per_eur":{"USD":1.1}}"#,
            r#"{"version":1,"date":"yesterday","per_eur":{"USD":1.1}}"#,
            r#"{"version":1,"date":"2026-11-17","per_eur":{"USD":-1}}"#,
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("rates.json"), bad).unwrap();
            let (store, _rx) = store_with(&dir, Arc::new(FakeFetch::offline()));
            store.load();
            assert!(matches!(store.lookup(ts("2026-11-17T18:00:00Z"), false), RatesState::Off(None)), "{bad}");
        }
    }

    #[test]
    fn missing_store_loads_fine() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _rx) = store_with(&dir, Arc::new(FakeFetch::offline()));
        store.load();
        assert!(matches!(store.lookup(ts("2026-11-17T18:00:00Z"), false), RatesState::Off(None)));
    }
}
