//! Weather from Open-Meteo for a typed or home city.
//!
//! Only the city name goes to the place search, then only that place's coordinates
//! (rounded to 2 decimals) to the forecast. Never the PC's own position.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::net::Fetch;
use crate::providers::shortcuts::encode_url_component;

const GEOCODE_URL: &str = "https://geocoding-api.open-meteo.com/v1/search?count=1&language=en&name=";
const FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";

/// A typed city is fetched only once its text stayed the same this long.
const PAUSE: Duration = Duration::from_millis(400);
/// A forecast is reused for this long per place and unit.
const FRESH_FOR: Duration = Duration::from_secs(30 * 60);
/// After a failed attempt, the same place is not tried again for this long.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// Longest place name kept from the place search.
const MAX_NAME: usize = 100;

#[derive(Debug, Clone, PartialEq)]
pub struct Forecast {
    /// The place's name as the place search spells it.
    pub place: String,
    pub temp: f64,
    /// WMO weather code.
    pub code: u8,
    pub low: f64,
    pub high: f64,
}

/// A place found by the place search; coordinates rounded to 2 decimals.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

/// Plain words for a WMO weather code.
pub fn describe(code: u8) -> &'static str {
    match code {
        0 => "clear",
        1 => "mostly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51 | 53 | 55 => "drizzle",
        56 | 57 => "freezing drizzle",
        61 => "light rain",
        63 => "rain",
        65 => "heavy rain",
        66 | 67 => "freezing rain",
        71 => "light snow",
        73 => "snow",
        75 => "heavy snow",
        77 => "snow grains",
        80 => "light showers",
        81 => "showers",
        82 => "heavy showers",
        85 | 86 => "snow showers",
        95 => "thunderstorm",
        96 | 99 => "thunderstorm with hail",
        _ => "unknown weather",
    }
}

/// Rounded to 2 decimals (about 1 km); never `-0`.
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0 + 0.0
}

/// The place search for `city`: the only request that carries the typed text.
pub fn geocode_url(city: &str) -> String {
    format!("{GEOCODE_URL}{}", encode_url_component(city))
}

/// The forecast for a place: carries only its rounded coordinates.
pub fn forecast_url(lat: f64, lon: f64, fahrenheit: bool) -> String {
    format!(
        "{FORECAST_URL}?latitude={:.2}&longitude={:.2}&current=temperature_2m,weather_code&daily=temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=1{}",
        round2(lat),
        round2(lon),
        if fahrenheit { "&temperature_unit=fahrenheit" } else { "" }
    )
}

fn valid_place(name: &str, lat: f64, lon: f64) -> Option<Place> {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    let ok = !name.is_empty()
        && lat.is_finite()
        && lon.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon);
    ok.then(|| Place { name, lat: round2(lat), lon: round2(lon) })
}

#[derive(Deserialize)]
struct GeoBody {
    results: Option<Vec<GeoHit>>,
}

#[derive(Deserialize)]
struct GeoHit {
    name: String,
    latitude: f64,
    longitude: f64,
}

/// The first hit of a place search: `Ok(None)` when nothing was found,
/// `Err` when the body is not a usable answer.
pub fn parse_geocode(body: &[u8]) -> Result<Option<Place>, ()> {
    let body: GeoBody = serde_json::from_slice(body).map_err(drop)?;
    match body.results.unwrap_or_default().into_iter().next() {
        None => Ok(None),
        Some(hit) => valid_place(&hit.name, hit.latitude, hit.longitude).map(Some).ok_or(()),
    }
}

#[derive(Deserialize)]
struct ForecastBody {
    current: Current,
    daily: Daily,
}

#[derive(Deserialize)]
struct Current {
    temperature_2m: f64,
    weather_code: f64,
}

#[derive(Deserialize)]
struct Daily {
    temperature_2m_max: Vec<Option<f64>>,
    temperature_2m_min: Vec<Option<f64>>,
}

/// Today's forecast for `place`, or None when the body is not a usable answer.
pub fn parse_forecast(body: &[u8], place: &str) -> Option<Forecast> {
    let body: ForecastBody = serde_json::from_slice(body).ok()?;
    let first = |v: &[Option<f64>]| v.first().copied().flatten().filter(|t| t.is_finite());
    let code = body.current.weather_code;
    let temp = body.current.temperature_2m;
    if code.fract() != 0.0 || !(0.0..=99.0).contains(&code) || !temp.is_finite() {
        return None;
    }
    Some(Forecast {
        place: place.to_string(),
        temp,
        code: code as u8,
        low: first(&body.daily.temperature_2m_min)?,
        high: first(&body.daily.temperature_2m_max)?,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum WeatherState {
    Ready(Forecast),
    /// A fetch is waiting for the pause or running; the place as known so far.
    Fetching(String),
    /// The place search found nothing for this text (as typed).
    NotFound(String),
    /// The last attempt for this place failed.
    Offline,
    /// `online = false`: no requests.
    Off,
}

/// On-disk form of `weather.json`: only the home city's coordinates.
#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    places: BTreeMap<String, (f64, f64, String)>,
}

/// Normalised city and whether the forecast is in Fahrenheit.
type Key = (String, bool);

#[derive(Clone)]
enum Geo {
    Found(Place),
    Missing,
}

struct Cached {
    forecast: Forecast,
    /// The lookup time that started the fetch.
    at: Instant,
}

/// The city waiting for the typing pause.
struct Pending {
    key: Key,
    city: String,
    /// The lookup's `now`, stamped on the result.
    at: Instant,
    /// Real time the city was first asked for; the pause is measured from here.
    since: Instant,
}

/// One fetch attempt: geocode (unless the place is known), then the forecast.
struct Job {
    key: Key,
    city: String,
    place: Option<Place>,
    at: Instant,
    attempt: u64,
}

#[derive(Default)]
struct State {
    home: Option<String>,
    /// Place search results for this session (plus the stored home city).
    geo: HashMap<String, Geo>,
    forecasts: HashMap<Key, Cached>,
    /// Running attempts; a fetch thread settles only its own.
    in_flight: HashMap<Key, u64>,
    /// When the last attempt for a place failed.
    failed: HashMap<Key, Instant>,
    pending: Option<Pending>,
    /// A timer thread is running for `pending`.
    timer: bool,
    attempts: u64,
    /// The bar is hidden: a query still in flight must not arm a city again.
    hidden: bool,
}

impl State {
    /// Whether `key` still needs a request at `now`.
    fn needs_fetch(&self, key: &Key, now: Instant) -> bool {
        let fresh = self.forecasts.get(key).is_some_and(|c| now.saturating_duration_since(c.at) < FRESH_FOR);
        let failed = self.failed.get(key).is_some_and(|t| now.saturating_duration_since(*t) < RETRY_AFTER);
        let missing = matches!(self.geo.get(&key.0), Some(Geo::Missing));
        !fresh && !failed && !missing && !self.in_flight.contains_key(key)
    }

    fn place(&self, norm: &str) -> Option<Place> {
        match self.geo.get(norm) {
            Some(Geo::Found(p)) => Some(p.clone()),
            _ => None,
        }
    }

    /// Marks `key` in flight and returns the job for it.
    fn begin(&mut self, key: Key, city: String, at: Instant) -> Job {
        self.attempts += 1;
        let attempt = self.attempts;
        self.in_flight.insert(key.clone(), attempt);
        self.failed.remove(&key);
        let place = self.place(&key.0);
        Job { key, city, place, at, attempt }
    }
}

struct Inner {
    path: PathBuf,
    fetch: Arc<dyn Fetch>,
    on_ready: Arc<dyn Fn() + Send + Sync>,
    state: Mutex<State>,
    /// Serialises writes of `weather.json`.
    saving: Mutex<()>,
}

/// Weather kept in memory and fetched in the background: a typed city after the
/// typing pause, the home city right away when its coordinates are stored.
pub struct WeatherStore {
    inner: Arc<Inner>,
}

/// Trims and collapses spaces: the city as shown and sent.
fn tidy(city: &str) -> String {
    city.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl WeatherStore {
    /// Cheap: no disk access. Call `load` once, off the query path.
    pub fn new(path: PathBuf, fetch: Arc<dyn Fetch>, on_ready: Arc<dyn Fn() + Send + Sync>) -> Arc<Self> {
        Arc::new(WeatherStore {
            inner: Arc::new(Inner {
                path,
                fetch,
                on_ready,
                state: Mutex::new(State::default()),
                saving: Mutex::new(()),
            }),
        })
    }

    /// Reads `weather.json`; missing, corrupt or out-of-range entries are ignored.
    pub fn load(&self) {
        let Some(stored) = std::fs::read(&self.inner.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Stored>(&bytes).ok())
            .filter(|s| s.version == 1)
        else {
            return;
        };
        let mut st = self.inner.lock();
        for (key, (lat, lon, name)) in stored.places {
            let key = tidy(&key).to_lowercase();
            if let (false, Some(place)) = (key.is_empty(), valid_place(&name, lat, lon)) {
                st.geo.entry(key).or_insert(Geo::Found(place));
            }
        }
    }

    /// The configured `weather_city` (empty for none). Saves its coordinates when
    /// they are already known, and drops a former home city from `weather.json`.
    /// Not for the query path: reads and may write `weather.json`.
    pub fn set_home(&self, city: &str) {
        let norm = tidy(city).to_lowercase();
        self.inner.lock().home = (!norm.is_empty()).then_some(norm);
        self.inner.save_home();
    }

    /// The bar was hidden (true) or is back (false). Hiding forgets the waiting
    /// city, and until the bar returns no city waits for the pause.
    pub fn set_hidden(&self, hidden: bool) {
        let mut st = self.inner.lock();
        st.hidden = hidden;
        if hidden {
            st.pending = None;
        }
    }

    /// Forgets the city waiting for the pause (the text is no longer a weather question).
    pub fn cancel_pending(&self) {
        self.inner.lock().pending = None;
    }

    /// Query path: memory only. Starts at most one background fetch per place:
    /// the home city with stored coordinates at once, any other city once the same
    /// text has been asked for `PAUSE` (a newer city replaces the waiting one).
    pub fn lookup(&self, city: &str, now: Instant, online: bool, fahrenheit: bool) -> WeatherState {
        let city = tidy(city);
        let norm = city.to_lowercase();
        let mut st = self.inner.lock();
        if !online {
            st.pending = None;
            return WeatherState::Off;
        }
        let key = (norm.clone(), fahrenheit);
        // Only the city asked for now may still be fetched after the pause.
        if st.pending.as_ref().is_some_and(|p| p.key != key) {
            st.pending = None;
        }
        if norm.is_empty() {
            return WeatherState::NotFound(city);
        }
        if let Some(c) = st.forecasts.get(&key) {
            if now.saturating_duration_since(c.at) < FRESH_FOR {
                return WeatherState::Ready(c.forecast.clone());
            }
        }
        let place = match st.geo.get(&norm) {
            Some(Geo::Missing) => return WeatherState::NotFound(city),
            Some(Geo::Found(p)) => Some(p.clone()),
            None => None,
        };
        let shown = place.as_ref().map_or_else(|| city.clone(), |p| p.name.clone());
        if st.in_flight.contains_key(&key) {
            return WeatherState::Fetching(shown);
        }
        if !st.needs_fetch(&key, now) {
            return WeatherState::Offline;
        }
        if place.is_some() && st.home.as_deref() == Some(norm.as_str()) {
            st.pending = None;
            let job = st.begin(key, city, now);
            drop(st);
            return if spawn_fetch(&self.inner, job) { WeatherState::Fetching(shown) } else { WeatherState::Offline };
        }
        if st.hidden {
            return WeatherState::Fetching(shown);
        }
        if st.pending.as_ref().is_none_or(|p| p.key != key) {
            st.pending = Some(Pending { key: key.clone(), city, at: now, since: Instant::now() });
        }
        if st.timer {
            return WeatherState::Fetching(shown);
        }
        st.timer = true;
        drop(st);
        if spawn_timer(&self.inner) {
            WeatherState::Fetching(shown)
        } else {
            let mut st = self.inner.lock();
            st.timer = false;
            st.pending = None;
            st.failed.insert(key, now);
            WeatherState::Offline
        }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Makes `weather.json` hold the home city's coordinates and nothing else: a
    /// former home city is removed even when the new one's coordinates are not known
    /// yet. Writes only when that changes the file.
    fn save_home(&self) {
        let _saving = self.saving.lock().unwrap_or_else(|e| e.into_inner());
        let (home, place) = {
            let st = self.lock();
            let place = st.home.as_deref().and_then(|h| st.place(h));
            (st.home.clone(), place)
        };
        let current = std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Stored>(&bytes).ok())
            .map(|s| s.places)
            .unwrap_or_default();
        let wanted: BTreeMap<_, _> = match (home, place) {
            (Some(home), Some(p)) => BTreeMap::from([(home, (p.lat, p.lon, p.name))]),
            // Coordinates not known yet: keep only an entry for the home city itself.
            (home, _) => current.iter().filter(|(k, _)| Some(*k) == home.as_ref()).map(|(k, v)| (k.clone(), v.clone())).collect(),
        };
        if wanted != current {
            // Not saved = one more place search after a restart.
            let _ = crate::ranking::save_json(&self.path, &Stored { version: 1, places: wanted });
        }
    }
}

/// The single timer thread: waits until the pending city has been unchanged for
/// `PAUSE`, then starts its fetch; exits when nothing is pending.
fn spawn_timer(inner: &Arc<Inner>) -> bool {
    let inner = inner.clone();
    std::thread::Builder::new()
        .name("flick-weather-timer".into())
        .spawn(move || loop {
            let job = {
                let mut st = inner.lock();
                let Some(p) = &st.pending else {
                    st.timer = false;
                    return;
                };
                let wait = PAUSE.saturating_sub(p.since.elapsed());
                if !wait.is_zero() {
                    drop(st);
                    std::thread::sleep(wait);
                    continue;
                }
                let Some(p) = st.pending.take() else { continue };
                st.needs_fetch(&p.key, p.at).then(|| st.begin(p.key, p.city, p.at))
            };
            if let Some(job) = job {
                if !spawn_fetch(&inner, job) {
                    (inner.on_ready)();
                }
            }
        })
        .is_ok()
}

enum Outcome {
    Missing,
    Failed(Option<Place>),
    Got(Place, Forecast),
}

/// Geocode (unless the place is known), then the forecast. Blocking.
fn run(fetch: &dyn Fetch, job: &Job) -> Outcome {
    let place = match &job.place {
        Some(p) => p.clone(),
        None => match fetch.get(&geocode_url(&job.city)).map_err(drop).and_then(|b| parse_geocode(&b)) {
            Err(()) => return Outcome::Failed(None),
            Ok(None) => return Outcome::Missing,
            Ok(Some(p)) => p,
        },
    };
    let forecast = fetch
        .get(&forecast_url(place.lat, place.lon, job.key.1))
        .ok()
        .and_then(|b| parse_forecast(&b, &place.name));
    match forecast {
        Some(f) => Outcome::Got(place, f),
        None => Outcome::Failed(Some(place)),
    }
}

/// One attempt on its own thread: settles its state, saves the home city's new
/// coordinates, then calls `on_ready` once. Returns false (and records the failure)
/// when the thread could not start.
fn spawn_fetch(inner: &Arc<Inner>, job: Job) -> bool {
    let (key, attempt, at) = (job.key.clone(), job.attempt, job.at);
    let thread_inner = inner.clone();
    let spawned = std::thread::Builder::new().name("flick-weather".into()).spawn(move || {
        let inner = thread_inner;
        let outcome = run(inner.fetch.as_ref(), &job);
        let save = {
            let mut st = inner.lock();
            let mine = st.in_flight.get(&job.key) == Some(&job.attempt);
            if mine {
                st.in_flight.remove(&job.key);
            }
            let norm = &job.key.0;
            match outcome {
                Outcome::Missing => {
                    st.geo.insert(norm.clone(), Geo::Missing);
                }
                Outcome::Failed(place) => {
                    if let Some(p) = place {
                        st.geo.insert(norm.clone(), Geo::Found(p));
                    }
                    if mine {
                        st.failed.insert(job.key.clone(), job.at);
                    }
                }
                Outcome::Got(place, forecast) => {
                    st.geo.insert(norm.clone(), Geo::Found(place));
                    if st.forecasts.get(&job.key).is_none_or(|c| c.at <= job.at) {
                        st.forecasts.insert(job.key.clone(), Cached { forecast, at: job.at });
                    }
                }
            }
            job.place.is_none() && st.home.as_deref() == Some(norm.as_str()) && st.place(norm).is_some()
        };
        if save {
            inner.save_home();
        }
        (inner.on_ready)();
    });
    if spawned.is_err() {
        let mut st = inner.lock();
        if st.in_flight.get(&key) == Some(&attempt) {
            st.in_flight.remove(&key);
            st.failed.insert(key, at);
        }
    }
    spawned.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{FakeFetch, FetchError};
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::time::Duration;

    const LONDON_GEO: &[u8] = br#"{"results":[{"id":2643743,"name":"London","latitude":51.50853,"longitude":-0.12574,"country":"United Kingdom"}],"generationtime_ms":0.5}"#;
    const NO_RESULTS: &[u8] = br#"{"generationtime_ms":0.3}"#;
    const LONDON_FORECAST: &[u8] = r#"{"latitude":51.5,"longitude":-0.12,"timezone":"Europe/London","current_units":{"temperature_2m":"°C"},"current":{"time":"2026-11-17T12:00","interval":900,"temperature_2m":12.3,"weather_code":61},"daily":{"time":["2026-11-17"],"temperature_2m_max":[14.0],"temperature_2m_min":[8.0]}}"#.as_bytes();

    fn london_forecast_url(fahrenheit: bool) -> String {
        forecast_url(51.50853, -0.12574, fahrenheit)
    }

    fn london_fetch() -> FakeFetch {
        FakeFetch::offline()
            .respond(&geocode_url("London"), Ok(LONDON_GEO.to_vec()))
            .respond(&london_forecast_url(false), Ok(LONDON_FORECAST.to_vec()))
            .respond(&london_forecast_url(true), Ok(LONDON_FORECAST.to_vec()))
    }

    fn store_with(dir: &tempfile::TempDir, fetch: Arc<FakeFetch>) -> (Arc<WeatherStore>, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let store = WeatherStore::new(
            dir.path().join("weather.json"),
            fetch,
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        (store, rx)
    }

    fn wait(rx: &mpsc::Receiver<()>) {
        rx.recv_timeout(Duration::from_secs(2)).expect("on_ready within 2 s");
    }

    fn london() -> Forecast {
        Forecast { place: "London".into(), temp: 12.3, code: 61, low: 8.0, high: 14.0 }
    }

    #[test]
    fn parses_geocode_and_forecast() {
        let place = parse_geocode(LONDON_GEO).unwrap().unwrap();
        assert_eq!(place.name, "London");
        assert_eq!(parse_forecast(LONDON_FORECAST, &place.name), Some(london()));
        assert_eq!(describe(61), "light rain");
        assert_eq!(describe(0), "clear");
        assert_eq!(describe(2), "partly cloudy");
        assert_eq!(describe(45), "fog");
        assert_eq!(describe(63), "rain");
        assert_eq!(describe(73), "snow");
        assert_eq!(describe(95), "thunderstorm");
        assert_eq!(parse_geocode(b"<html>"), Err(()));
        assert_eq!(parse_forecast(b"{}", "x"), None);
        assert_eq!(parse_forecast(br#"{"current":{"temperature_2m":1,"weather_code":0},"daily":{"temperature_2m_max":[],"temperature_2m_min":[]}}"#, "x"), None);
    }

    #[test]
    fn store_fetches_and_caches() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(london_fetch());
        let (store, rx) = store_with(&dir, fetch.clone());
        let t0 = Instant::now();
        assert_eq!(store.lookup("London", t0, true, false), WeatherState::Fetching("London".into()));
        wait(&rx);
        assert_eq!(fetch.urls(), vec![geocode_url("London"), london_forecast_url(false)]);
        assert_eq!(store.lookup(" london ", t0, true, false), WeatherState::Ready(london()));
        // Cached for 30 minutes per place; a non-home place is not written to disk.
        assert_eq!(store.lookup("LONDON", t0 + Duration::from_secs(29 * 60), true, false), WeatherState::Ready(london()));
        assert_eq!(fetch.urls().len(), 2);
        assert!(!dir.path().join("weather.json").exists());
        // After 30 minutes: forecast only (coordinates kept for the session), shown with the found name.
        let later = Instant::now() + Duration::from_secs(30 * 60);
        assert_eq!(store.lookup("london", later, true, false), WeatherState::Fetching("London".into()));
        wait(&rx);
        assert_eq!(fetch.urls()[2..], [london_forecast_url(false)]);
        // Fahrenheit is its own forecast.
        assert_eq!(store.lookup("london", later, true, true), WeatherState::Fetching("London".into()));
        wait(&rx);
        assert_eq!(fetch.urls()[3..], [london_forecast_url(true)]);
    }

    #[test]
    fn weather_fetch_waits_for_pause() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(FakeFetch::offline());
        let (store, rx) = store_with(&dir, fetch.clone());
        let first = Instant::now();
        for city in ["l", "lo", "lon"] {
            if city != "l" {
                std::thread::sleep(Duration::from_millis(150));
            }
            assert_eq!(store.lookup(city, Instant::now(), true, false), WeatherState::Fetching(city.into()));
        }
        let last = Instant::now();
        std::thread::sleep(Duration::from_millis(200));
        // Measured from the latest city: past 400 ms since `l`, well short of it since `lon`.
        assert!(first.elapsed() > Duration::from_millis(450));
        assert!(fetch.urls().is_empty(), "nothing before the pause after `lon`");
        assert!(last.elapsed() < PAUSE, "test machine too slow to judge the pause");
        wait(&rx);
        assert_eq!(fetch.urls(), vec![geocode_url("lon")]);
        assert!(rx.recv_timeout(Duration::from_millis(1200)).is_err(), "one attempt only");
    }

    #[test]
    fn repeated_lookups_keep_the_first_time() {
        // The bar re-queries the same text; that must not push the fetch further out.
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(FakeFetch::offline());
        let (store, rx) = store_with(&dir, fetch.clone());
        let t0 = Instant::now();
        store.lookup("Tokyo", t0, true, false);
        for _ in 0..6 {
            std::thread::sleep(Duration::from_millis(100));
            store.lookup("tokyo", Instant::now(), true, false);
        }
        wait(&rx);
        assert_eq!(fetch.urls(), vec![geocode_url("Tokyo")]);
    }

    #[test]
    fn unknown_place() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(FakeFetch::offline().respond(&geocode_url("xyz"), Ok(NO_RESULTS.to_vec())));
        let (store, rx) = store_with(&dir, fetch.clone());
        let now = Instant::now();
        assert_eq!(store.lookup("xyz", now, true, false), WeatherState::Fetching("xyz".into()));
        wait(&rx);
        assert_eq!(store.lookup("xyz", now, true, false), WeatherState::NotFound("xyz".into()));
        // Remembered for the session: no second request.
        assert_eq!(store.lookup("XYZ", now + Duration::from_secs(3600), true, false), WeatherState::NotFound("XYZ".into()));
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(fetch.urls().len(), 1);
    }

    #[test]
    fn offline_settles_and_off_never_fetches() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(FakeFetch::new(Err(FetchError::Status(503))));
        let (store, rx) = store_with(&dir, fetch.clone());
        let now = Instant::now();
        store.lookup("Vienna", now, true, false);
        wait(&rx);
        assert_eq!(store.lookup("Vienna", now, true, false), WeatherState::Offline);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(fetch.urls().len(), 1, "no retry loop while the bar re-queries");
        // A while later it tries again.
        assert_eq!(store.lookup("Vienna", now + Duration::from_secs(61), true, false), WeatherState::Fetching("Vienna".into()));
        wait(&rx);
        assert_eq!(fetch.urls().len(), 2);

        let fetch = Arc::new(london_fetch());
        let (store, _rx) = store_with(&dir, fetch.clone());
        store.set_home("London");
        assert_eq!(store.lookup("London", Instant::now(), false, false), WeatherState::Off);
        assert_eq!(store.lookup("Tokyo", Instant::now(), false, false), WeatherState::Off);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(fetch.urls().len(), 0);
    }

    #[test]
    fn going_offline_drops_the_pending_city() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(FakeFetch::offline());
        let (store, _rx) = store_with(&dir, fetch.clone());
        store.lookup("London", Instant::now(), true, false);
        store.lookup("London", Instant::now(), false, false);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(fetch.urls().len(), 0);
        store.lookup("Paris", Instant::now(), true, false);
        store.cancel_pending();
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(fetch.urls().len(), 0);
    }

    #[test]
    fn home_coordinates_are_saved_and_skip_the_geocode() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(london_fetch());
        let (store, rx) = store_with(&dir, fetch.clone());
        store.set_home("London");
        store.lookup("London", Instant::now(), true, false);
        wait(&rx);
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("weather.json")).unwrap()).unwrap();
        assert_eq!(saved, serde_json::json!({"version":1,"places":{"london":[51.51,-0.13,"London"]}}));

        // A new session: the home forecast is one request, without waiting for the pause.
        let fetch = Arc::new(london_fetch());
        let (store, rx) = store_with(&dir, fetch.clone());
        store.load();
        store.set_home("London");
        let now = Instant::now();
        assert_eq!(store.lookup("London", now, true, false), WeatherState::Fetching("London".into()));
        rx.recv_timeout(Duration::from_millis(300)).expect("no pause for the home city");
        assert_eq!(fetch.urls(), vec![london_forecast_url(false)]);
        assert_eq!(store.lookup("London", now, true, false), WeatherState::Ready(london()));
        assert_eq!(store.lookup("London", now + Duration::from_secs(10 * 60), true, false), WeatherState::Ready(london()));
        assert_eq!(fetch.urls().len(), 1);
    }

    #[test]
    fn only_the_home_city_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(london_fetch());
        let (store, rx) = store_with(&dir, fetch.clone());
        store.set_home("Vienna");
        store.lookup("London", Instant::now(), true, false);
        wait(&rx);
        assert!(!dir.path().join("weather.json").exists());
        // Becoming home later saves the coordinates already known.
        store.set_home("London");
        let saved = std::fs::read_to_string(dir.path().join("weather.json")).unwrap();
        assert!(saved.contains("\"london\"") && !saved.contains("vienna"), "{saved}");
    }

    fn saved_places(dir: &tempfile::TempDir) -> serde_json::Value {
        let bytes = std::fs::read(dir.path().join("weather.json")).unwrap();
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["places"].clone()
    }

    #[test]
    fn a_former_home_city_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(london_fetch());
        let (store, rx) = store_with(&dir, fetch.clone());
        store.set_home("London");
        store.lookup("London", Instant::now(), true, false);
        wait(&rx);
        assert_eq!(saved_places(&dir), serde_json::json!({"london":[51.51,-0.13,"London"]}));

        // Home cleared: the old city leaves the file.
        store.set_home("");
        assert_eq!(saved_places(&dir), serde_json::json!({}));
        // Nothing to change: no write (a missing file counts as no places).
        std::fs::remove_file(dir.path().join("weather.json")).unwrap();
        store.set_home("Vienna");
        assert!(!dir.path().join("weather.json").exists());

        // A new home whose coordinates are not known yet also removes the old one.
        store.set_home("London");
        assert_eq!(saved_places(&dir), serde_json::json!({"london":[51.51,-0.13,"London"]}));
        store.set_home("Vienna");
        assert_eq!(saved_places(&dir), serde_json::json!({}));

        // The stored entry for the home city itself stays, even before `load`.
        std::fs::write(dir.path().join("weather.json"), r#"{"version":1,"places":{"tokyo":[35.69,139.69,"Tokyo"]}}"#).unwrap();
        let (fresh, _rx) = store_with(&dir, Arc::new(FakeFetch::offline()));
        fresh.set_home("Tokyo");
        assert_eq!(saved_places(&dir), serde_json::json!({"tokyo":[35.69,139.69,"Tokyo"]}));
    }

    #[test]
    fn an_answered_city_drops_the_waiting_one() {
        // London cached: `londo` waits for the pause, then `london` answers at once.
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(london_fetch());
        let (store, rx) = store_with(&dir, fetch.clone());
        let now = Instant::now();
        store.lookup("London", now, true, false);
        wait(&rx);
        assert_eq!(store.lookup("londo", now, true, false), WeatherState::Fetching("londo".into()));
        assert_eq!(store.lookup("london", now, true, false), WeatherState::Ready(london()));
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(fetch.urls(), vec![geocode_url("London"), london_forecast_url(false)]);

        // Home with stored coordinates: `weather l` then bare `weather` (home).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("weather.json"), r#"{"version":1,"places":{"london":[51.51,-0.13,"London"]}}"#).unwrap();
        let fetch = Arc::new(london_fetch());
        let (store, rx) = store_with(&dir, fetch.clone());
        store.load();
        store.set_home("London");
        assert_eq!(store.lookup("l", Instant::now(), true, false), WeatherState::Fetching("l".into()));
        assert_eq!(store.lookup("London", Instant::now(), true, false), WeatherState::Fetching("London".into()));
        wait(&rx);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(fetch.urls(), vec![london_forecast_url(false)]);

        // Any other early answer (not found, offline) drops it too.
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(FakeFetch::offline().respond(&geocode_url("xyz"), Ok(NO_RESULTS.to_vec())));
        let (store, rx) = store_with(&dir, fetch.clone());
        store.lookup("xyz", Instant::now(), true, false);
        wait(&rx);
        store.lookup("xy", Instant::now(), true, false);
        assert_eq!(store.lookup("xyz", Instant::now(), true, false), WeatherState::NotFound("xyz".into()));
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(fetch.urls(), vec![geocode_url("xyz")]);
    }

    #[test]
    fn a_hidden_bar_drops_and_does_not_rearm_the_waiting_city() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(london_fetch());
        let (store, _rx) = store_with(&dir, fetch.clone());
        let now = Instant::now();
        store.lookup("Londo", now, true, false);
        store.set_hidden(true);
        // A query still in flight when the bar hid asks again: nothing is armed.
        store.lookup("Londo", now, true, false);
        std::thread::sleep(Duration::from_millis(600));
        assert!(fetch.urls().is_empty(), "{:?}", fetch.urls());
        // Shown again: typing waits for the pause and fetches as before.
        store.set_hidden(false);
        store.lookup("London", now, true, false);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(fetch.urls().first(), Some(&geocode_url("London")));
    }

    #[test]
    fn corrupt_or_missing_store_loads_fine() {
        for bad in ["{", r#"{"version":2,"places":{}}"#, r#"{"version":1,"places":{"london":[999,0,"London"]}}"#] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("weather.json"), bad).unwrap();
            let fetch = Arc::new(FakeFetch::offline());
            let (store, _rx) = store_with(&dir, fetch.clone());
            store.load();
            store.set_home("London");
            store.lookup("London", Instant::now(), true, false);
            std::thread::sleep(Duration::from_millis(100));
            assert!(fetch.urls().is_empty(), "{bad}: needs the pause, so no stored coordinates were used");
        }
        let dir = tempfile::tempdir().unwrap();
        let (store, _rx) = store_with(&dir, Arc::new(FakeFetch::offline()));
        store.load();
    }

    #[test]
    fn urls_carry_only_city_or_coordinates() {
        assert_eq!(
            geocode_url("New York"),
            "https://geocoding-api.open-meteo.com/v1/search?count=1&language=en&name=New%20York"
        );
        assert!(forecast_url(48.2082, 16.3738, false).contains("latitude=48.21&longitude=16.37"));
        assert_eq!(
            forecast_url(48.2082, 16.3738, false),
            "https://api.open-meteo.com/v1/forecast?latitude=48.21&longitude=16.37&current=temperature_2m,weather_code&daily=temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=1"
        );
        assert!(forecast_url(48.2082, 16.3738, true).ends_with("&forecast_days=1&temperature_unit=fahrenheit"));
        assert_eq!(geocode_url("Zürich&x=1"), "https://geocoding-api.open-meteo.com/v1/search?count=1&language=en&name=Z%C3%BCrich%26x%3D1");
    }

    /// Touches the network: run by hand with `cargo test --lib answers::weather -- --ignored`.
    #[test]
    #[ignore]
    fn probe_london() {
        use crate::net::{Fetch, WinHttp};
        let place = parse_geocode(&WinHttp.get(&geocode_url("London")).expect("geocode"))
            .expect("geocode json")
            .expect("London found");
        let body = WinHttp.get(&forecast_url(place.lat, place.lon, false)).expect("forecast");
        let f = parse_forecast(&body, &place.name).expect("forecast json");
        println!("{}: {} °C, {} ({}), {} to {} °C", f.place, f.temp, describe(f.code), f.code, f.low, f.high);
    }
}
