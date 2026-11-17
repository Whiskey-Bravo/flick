//! `connect` / `disconnect`: paired Bluetooth audio devices; `wifi <name>`:
//! saved Wi-Fi networks. Both lists are read on [`Loader`] threads; the query
//! path only reads memory. Nothing is learned; a row can be pinned or put on
//! a direct hotkey (its id is the device's id or the network's name).

use std::sync::Arc;

use crate::devices::{BtAudio, BtDevice, Wifi, WifiProfile};
use crate::dirs::{Load, Loader, ReadError, FRESH, SLOW};
use crate::fuzzy::score;
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem};

const NAME: &str = "devices";
/// The words this provider claims (with text after them for `wifi`).
pub const WORDS: [&str; 3] = ["connect", "disconnect", "wifi"];
const READING_BT: &str = "Reading Bluetooth devices\u{2026}";
const NONE_PAIRED: &str = "No paired Bluetooth audio devices";
const NONE_CONNECTED: &str = "No connected Bluetooth audio devices";
const NO_MATCH: &str = "No device matches that name";
const READING_WIFI: &str = "Reading Wi-Fi networks\u{2026}";
const BT_ICON: &str = "builtin:bluetooth";
/// The title of a stored device row before the device list was read.
const BT_STANDIN: &str = "Bluetooth audio device";
const WIFI_ICON: &str = "builtin:wifi";
/// The id of the one inert row a query may show.
const INERT_ID: &str = "info";

#[derive(Clone, Copy, PartialEq)]
enum Verb {
    Connect,
    Disconnect,
}

impl Verb {
    fn word(self) -> &'static str {
        match self {
            Verb::Connect => "connect",
            Verb::Disconnect => "disconnect",
        }
    }
}

/// The rest of `text` (trimmed) when its first word is `word` (any case),
/// followed by a space or the end.
fn after_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let head = text.get(..word.len())?;
    let rest = &text[word.len()..];
    (head.eq_ignore_ascii_case(word) && (rest.is_empty() || rest.starts_with(' '))).then(|| rest.trim())
}

/// `connect …` / `disconnect …`: the verb and the name typed after it.
fn verb(text: &str) -> Option<(Verb, &str)> {
    [Verb::Connect, Verb::Disconnect]
        .into_iter()
        .find_map(|v| after_word(text, v.word()).map(|rest| (v, rest)))
}

/// `wifi <name>`: the name, unless it is empty or `on`/`off` (the toggles').
fn wifi_name(text: &str) -> Option<&str> {
    let name = after_word(text, "wifi")?;
    let toggle = name.eq_ignore_ascii_case("on") || name.eq_ignore_ascii_case("off");
    (!name.is_empty() && !toggle).then_some(name)
}

/// A device row; `connected` is `None` when the list was not read yet.
fn bt_row(verb: Verb, id: &str, title: String, connected: Option<bool>, score: u32) -> ResultItem {
    ResultItem {
        id: format!("{}:{id}", verb.word()),
        provider: NAME,
        title,
        subtitle: match connected {
            Some(true) => "Connected",
            Some(false) => "Not connected",
            None => "",
        }
        .to_string(),
        score,
        icon: BT_ICON.to_string(),
        pinnable: true,
        ..Default::default()
    }
}

fn wifi_row(name: &str, connected: bool, score: u32) -> ResultItem {
    ResultItem {
        id: format!("wifi:{name}"),
        provider: NAME,
        title: name.to_string(),
        subtitle: if connected { "Connected" } else { "Saved network" }.to_string(),
        score,
        icon: WIFI_ICON.to_string(),
        pinnable: true,
        ..Default::default()
    }
}

fn inert(title: &str, icon: &str) -> ResultItem {
    ResultItem {
        id: INERT_ID.to_string(),
        provider: NAME,
        title: title.to_string(),
        icon: icon.to_string(),
        ..Default::default()
    }
}

pub struct DevicesProvider {
    bt: Arc<dyn BtAudio>,
    wifi: Arc<dyn Wifi>,
    bt_list: Arc<Loader<Vec<BtDevice>>>,
    wifi_list: Arc<Loader<Vec<WifiProfile>>>,
}

impl DevicesProvider {
    pub fn new(bt: Arc<dyn BtAudio>, wifi: Arc<dyn Wifi>, on_ready: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            bt,
            wifi,
            bt_list: Loader::new(on_ready.clone(), FRESH, SLOW),
            wifi_list: Loader::new(on_ready, FRESH, SLOW),
        }
    }

    /// The paired devices as known now, never waiting.
    fn devices(&self) -> Load<Vec<BtDevice>> {
        let bt = Arc::clone(&self.bt);
        self.bt_list.get("bt", move || bt.list().map_err(|_| ReadError::Denied))
    }

    /// The saved networks as known now, never waiting.
    fn profiles(&self) -> Load<Vec<WifiProfile>> {
        let wifi = Arc::clone(&self.wifi);
        self.wifi_list.get("wifi", move || wifi.profiles().map_err(|_| ReadError::Denied))
    }

    fn bt_rows(&self, verb: Verb, name: &str) -> Vec<ResultItem> {
        let devices = match self.devices() {
            Load::Pending => return vec![inert(READING_BT, BT_ICON)],
            Load::Ready(devices) => devices,
            Load::NotFound | Load::Denied | Load::Slow => return vec![inert(NONE_PAIRED, BT_ICON)],
        };
        if devices.is_empty() {
            return vec![inert(NONE_PAIRED, BT_ICON)];
        }
        let candidates: Vec<&BtDevice> = devices.iter().filter(|d| verb == Verb::Connect || d.connected).collect();
        if candidates.is_empty() {
            return vec![inert(NONE_CONNECTED, BT_ICON)];
        }
        let rows: Vec<ResultItem> = candidates
            .into_iter()
            .enumerate()
            .filter_map(|(i, d)| {
                let score = if name.is_empty() { 1000u32.saturating_sub(i as u32) } else { score(name, &d.name)? };
                Some(bt_row(verb, &d.id, d.name.clone(), Some(d.connected), score))
            })
            .collect();
        if rows.is_empty() {
            return vec![inert(NO_MATCH, BT_ICON)];
        }
        rows
    }

    fn wifi_rows(&self, name: &str) -> Vec<ResultItem> {
        let profiles = match self.profiles() {
            Load::Pending => return vec![ResultItem { top: true, ..inert(READING_WIFI, WIFI_ICON) }],
            Load::Ready(profiles) => profiles,
            Load::NotFound | Load::Denied | Load::Slow => return vec![],
        };
        profiles
            .iter()
            .filter_map(|p| {
                Some(ResultItem { top: true, ..wifi_row(&p.name, p.connected, score(name, &p.name)?) })
            })
            .collect()
    }

    /// The name of the device `id` from the last list read, for messages.
    fn device_name(&self, id: &str) -> String {
        match self.devices() {
            Load::Ready(devices) => devices.iter().find(|d| d.id == id).map(|d| d.name.clone()),
            _ => None,
        }
        .unwrap_or_else(|| "that device".to_string())
    }
}

impl Provider for DevicesProvider {
    fn name(&self) -> &'static str {
        NAME
    }

    /// Memory only: both lists are read on loader threads. Plain text other
    /// than `wifi <name>` reads nothing.
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if let Some((verb, name)) = verb(text) {
            return self.bt_rows(verb, name);
        }
        match wifi_name(text) {
            Some(name) => self.wifi_rows(name),
            None => vec![],
        }
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        if let Some(device) = id.strip_prefix("connect:") {
            let name = self.device_name(device);
            self.bt.connect(device).map_err(|_| format!("Could not connect {name}"))?;
            self.bt_list.clear();
            return Ok(Outcome::Hide);
        }
        if let Some(device) = id.strip_prefix("disconnect:") {
            let name = self.device_name(device);
            self.bt.disconnect(device).map_err(|_| format!("Could not disconnect {name}"))?;
            self.bt_list.clear();
            return Ok(Outcome::Hide);
        }
        if let Some(network) = id.strip_prefix("wifi:") {
            self.wifi.connect(network).map_err(|_| format!("Could not join {network}"))?;
            self.wifi_list.clear();
            return Ok(Outcome::Hide);
        }
        Ok(Outcome::Keep)
    }

    /// Rows carry typed and read names: never learned.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    /// A stored row (a pin or a direct hotkey), from memory only: the last
    /// list read decides, and a device or network missing from it is gone.
    /// Before any read the row is rebuilt from its id alone.
    fn item(&self, id: &str) -> Option<ResultItem> {
        if let Some(network) = id.strip_prefix("wifi:") {
            return match self.wifi_list.peek("wifi") {
                Some(profiles) => profiles.iter().find(|p| p.name == network).map(|p| wifi_row(&p.name, p.connected, 0)),
                None => Some(wifi_row(network, false, 0)),
            };
        }
        let (verb, device) = [Verb::Connect, Verb::Disconnect]
            .into_iter()
            .find_map(|v| id.strip_prefix(v.word()).and_then(|r| r.strip_prefix(':')).map(|d| (v, d)))?;
        match self.bt_list.peek("bt") {
            Some(devices) => {
                let d = devices.iter().find(|d| d.id == device)?;
                Some(bt_row(verb, &d.id, d.name.clone(), Some(d.connected), 0))
            }
            None => Some(bt_row(verb, device, BT_STANDIN.to_string(), None, 0)),
        }
    }

    fn claims(&self, text: &str) -> bool {
        verb(text.trim()).is_some()
    }

    fn help(&self) -> Vec<HelpLine> {
        let line = |example: &str, about: &str, fill: &str| HelpLine {
            example: example.into(),
            about: about.into(),
            fill: fill.into(),
        };
        vec![
            line("connect headphones", "connect a paired Bluetooth audio device", "connect"),
            line("disconnect headphones", "disconnect a Bluetooth audio device", "disconnect"),
            line("wifi <name>", "join a saved Wi-Fi network", "wifi"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::testing::{FakeBt, FakeWifi};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    const READING_BT: &str = "Reading Bluetooth devices\u{2026}";
    const READING_WIFI: &str = "Reading Wi-Fi networks\u{2026}";

    struct T {
        p: DevicesProvider,
        rx: Receiver<()>,
        bt: Arc<FakeBt>,
        wifi: Arc<FakeWifi>,
    }

    fn signal() -> (Arc<dyn Fn() + Send + Sync>, Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        (
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
            rx,
        )
    }

    fn dev(id: &str, name: &str, connected: bool) -> BtDevice {
        BtDevice { id: id.into(), name: name.into(), connected }
    }

    fn net(name: &str, connected: bool) -> WifiProfile {
        WifiProfile { name: name.into(), connected }
    }

    fn two() -> Vec<BtDevice> {
        vec![dev("bt-1", "WH-1000XM4", true), dev("bt-2", "Speaker", false)]
    }

    fn with(bt: FakeBt, wifi: FakeWifi) -> T {
        let (bt, wifi) = (Arc::new(bt), Arc::new(wifi));
        let (on_ready, rx) = signal();
        let p = DevicesProvider::new(bt.clone(), wifi.clone(), on_ready);
        T { p, rx, bt, wifi }
    }

    fn t(devices: Vec<BtDevice>) -> T {
        with(FakeBt::new(devices), FakeWifi::new(vec![net("Home-5G", false), net("Office", true)]))
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    /// The rows once read: waits out a first `Reading…` row.
    fn ready(t: &T, text: &str) -> Vec<ResultItem> {
        let rows = t.p.query(&q(text));
        if rows.first().is_some_and(|r| r.title == READING_BT || r.title == READING_WIFI) {
            t.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready within 1 s");
            return t.p.query(&q(text));
        }
        rows
    }

    fn shown(rows: &[ResultItem]) -> Vec<(String, String)> {
        rows.iter().map(|r| (r.title.clone(), r.subtitle.clone())).collect()
    }

    fn pair(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    #[test]
    fn bare_connect_lists_every_paired_audio_device() {
        let t = t(two());
        let first = t.p.query(&q("connect"));
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].title, READING_BT);
        t.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready");
        let rows = t.p.query(&q("connect"));
        assert_eq!(shown(&rows), vec![pair("WH-1000XM4", "Connected"), pair("Speaker", "Not connected")]);
        assert!(rows.iter().all(|r| r.icon == "builtin:bluetooth" && r.provider == "devices"));
        assert_eq!(rows[0].id, "connect:bt-1");
        assert_eq!(rows[1].id, "connect:bt-2");
    }

    #[test]
    fn connect_filters_by_name() {
        let t = t(two());
        let rows = ready(&t, "connect speak");
        assert_eq!(shown(&rows), vec![pair("Speaker", "Not connected")]);
        let rows = ready(&t, "Connect wh");
        assert_eq!(shown(&rows), vec![pair("WH-1000XM4", "Connected")]);
        assert_eq!(shown(&ready(&t, "connect zzz")), vec![pair("No device matches that name", "")]);
    }

    #[test]
    fn disconnect_lists_connected_ones_only() {
        let t = t(two());
        let rows = ready(&t, "disconnect");
        assert_eq!(shown(&rows), vec![pair("WH-1000XM4", "Connected")]);
        assert_eq!(rows[0].id, "disconnect:bt-1");
        let none = t_none_connected();
        assert_eq!(shown(&ready(&none, "disconnect")), vec![pair("No connected Bluetooth audio devices", "")]);
    }

    fn t_none_connected() -> T {
        t(vec![dev("bt-2", "Speaker", false)])
    }

    #[test]
    fn enter_connects_or_disconnects_that_device() {
        let t = t(two());
        ready(&t, "connect");
        assert_eq!(t.p.activate("connect:bt-2", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(t.bt.calls(), vec!["connect bt-2"]);
        assert_eq!(t.p.activate("disconnect:bt-1", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(t.bt.calls(), vec!["connect bt-2", "disconnect bt-1"]);
    }

    #[test]
    fn a_failed_connect_says_so() {
        let t = t(two());
        ready(&t, "connect");
        t.bt.fail_next();
        assert_eq!(t.p.activate("connect:bt-2", Modifier::None), Err("Could not connect Speaker".to_string()));
        t.bt.fail_next();
        assert_eq!(
            t.p.activate("disconnect:bt-1", Modifier::None),
            Err("Could not disconnect WH-1000XM4".to_string())
        );
    }

    #[test]
    fn none_paired_says_so() {
        let t = t(vec![]);
        assert_eq!(shown(&ready(&t, "connect")), vec![pair("No paired Bluetooth audio devices", "")]);
        let failing = with(FakeBt::failing(), FakeWifi::new(vec![]));
        assert_eq!(shown(&ready(&failing, "connect x")), vec![pair("No paired Bluetooth audio devices", "")]);
    }

    #[test]
    fn query_never_waits_for_the_device_list() {
        let t = t(two());
        t.bt.block();
        let start = Instant::now();
        let rows = t.p.query(&q("connect"));
        assert!(start.elapsed() < Duration::from_millis(20), "{:?}", start.elapsed());
        assert_eq!(shown(&rows), vec![pair(READING_BT, "")]);
        assert_eq!(t.bt.lists_on(std::thread::current().id()), 0);
        t.bt.release();
    }

    #[test]
    fn wifi_query_never_waits_for_the_profiles() {
        let t = t(two());
        t.wifi.block();
        let start = Instant::now();
        let rows = t.p.query(&q("wifi home"));
        assert!(start.elapsed() < Duration::from_millis(20), "{:?}", start.elapsed());
        assert_eq!(shown(&rows), vec![pair(READING_WIFI, "")]);
        assert!(rows[0].top);
        t.wifi.release();
        t.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready");
        assert_eq!(t.wifi.lists(), 1);
    }

    #[test]
    fn wifi_name_lists_matching_saved_networks() {
        let t = t(two());
        let first = t.p.query(&q("wifi home"));
        assert_eq!(shown(&first), vec![pair(READING_WIFI, "")]);
        let rows = ready(&t, "wifi home");
        assert_eq!(shown(&rows), vec![pair("Home-5G", "Saved network")]);
        assert!(rows[0].top);
        assert_eq!(rows[0].id, "wifi:Home-5G");
        assert_eq!(rows[0].icon, "builtin:wifi");
        assert_eq!(shown(&ready(&t, "WIFI office")), vec![pair("Office", "Connected")]);
        assert!(ready(&t, "wifi nowhere").is_empty());
    }

    #[test]
    fn wifi_on_and_off_are_left_to_the_toggles() {
        let t = t(two());
        for text in ["wifi", "wifi on", "WiFi Off", "wifi   ", "home", "wifihome"] {
            assert!(t.p.query(&q(text)).is_empty(), "{text}");
        }
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(t.wifi.lists(), 0);
        assert_eq!(t.bt.lists(), 0);
    }

    #[test]
    fn enter_joins_the_network() {
        let t = t(two());
        ready(&t, "wifi home");
        assert_eq!(t.p.activate("wifi:Home-5G", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(t.wifi.calls(), vec!["join Home-5G"]);
        t.wifi.fail_next();
        assert_eq!(t.p.activate("wifi:Home-5G", Modifier::None), Err("Could not join Home-5G".to_string()));
    }

    #[test]
    fn a_failed_wifi_read_shows_no_rows() {
        let t = with(FakeBt::new(vec![]), FakeWifi::failing());
        assert!(ready(&t, "wifi home").is_empty());
    }

    #[test]
    fn inert_rows_keep_the_bar() {
        let t = t(vec![]);
        let rows = ready(&t, "connect");
        assert_eq!(t.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Keep));
        assert!(t.bt.calls().is_empty());
    }

    #[test]
    fn only_connect_and_disconnect_are_claimed() {
        let t = t(two());
        for yes in ["connect", "Connect x", "disconnect", "DISCONNECT head phones"] {
            assert!(t.p.claims(yes), "{yes}");
        }
        for no in ["connections", "wifi home", "disconnected", "reconnect", ""] {
            assert!(!t.p.claims(no), "{no}");
        }
    }

    #[test]
    fn rows_are_never_learned_but_can_be_stored() {
        let t = t(two());
        let mut rows = ready(&t, "connect");
        rows.extend(ready(&t, "disconnect"));
        rows.extend(ready(&t, "wifi home"));
        assert_eq!(rows.len(), 4);
        for r in &rows {
            assert_eq!(t.p.rank_key(&r.id), None, "{}", r.id);
            assert!(r.pinnable, "{}", r.id);
            // Rebuilt from the list in memory, for a pin or a direct hotkey.
            let item = t.p.item(&r.id).unwrap_or_else(|| panic!("{}", r.id));
            assert_eq!((item.title.as_str(), item.subtitle.as_str()), (r.title.as_str(), r.subtitle.as_str()));
        }
        assert!(t.p.item("connect:gone").is_none() && t.p.item("wifi:gone").is_none());
        assert!(t.p.item("info").is_none());
    }

    #[test]
    fn help_has_the_three_device_lines() {
        let t = t(vec![]);
        let help: Vec<(String, String, String)> =
            t.p.help().into_iter().map(|h| (h.example, h.about, h.fill)).collect();
        let line = |a: &str, b: &str, c: &str| (a.to_string(), b.to_string(), c.to_string());
        assert_eq!(
            help,
            vec![
                line("connect headphones", "connect a paired Bluetooth audio device", "connect"),
                line("disconnect headphones", "disconnect a Bluetooth audio device", "disconnect"),
                line("wifi <name>", "join a saved Wi-Fi network", "wifi"),
            ]
        );
        assert_eq!(t.p.name(), "devices");
    }
}
