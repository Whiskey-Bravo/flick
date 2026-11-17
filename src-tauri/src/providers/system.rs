//! System provider: volume, mute, media keys, dark and light mode, Wi-Fi,
//! Bluetooth and night light. Typing only builds rows from the cached audio
//! state; everything happens on Enter.

use std::sync::{Arc, Mutex};

use crate::controls::{Audio, Controls, MediaKey, RadioKind};
use crate::fuzzy;
use crate::launch::Launcher;
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem, Then};
use crate::providers::power::POWER_PENALTY;

/// Score of rows built from typed numbers (`vol 30`).
const PARSED_SCORE: u32 = 800;
const STEP: u8 = 10;
/// The "out of range" hint must never crowd out real results.
const WAIT_SCORE: u32 = 100;
const WAIT_TITLE: &str = "Volume goes from 0 to 100";

struct Cmd {
    id: &'static str,
    title: &'static str,
    keywords: &'static [&'static str],
}

const CMDS: &[Cmd] = &[
    Cmd { id: "mute", title: "Mute", keywords: &["mute", "sound off"] },
    Cmd { id: "unmute", title: "Unmute", keywords: &["unmute", "sound on"] },
    Cmd { id: "vol", title: "Volume", keywords: &["vol", "volume"] },
    Cmd { id: "media:next", title: "Next song", keywords: &["next song", "next track"] },
    Cmd {
        id: "media:prev",
        title: "Previous song",
        keywords: &["previous song", "prev song", "previous track"],
    },
    Cmd { id: "media:play", title: "Play / pause", keywords: &["play", "pause", "play pause"] },
    Cmd { id: "theme:dark", title: "Switch to dark mode", keywords: &["dark mode"] },
    Cmd { id: "theme:light", title: "Switch to light mode", keywords: &["light mode"] },
    Cmd { id: "radio:wifi:on", title: "Turn Wi-Fi on", keywords: &["wifi on", "wi-fi on"] },
    Cmd { id: "radio:wifi:off", title: "Turn Wi-Fi off", keywords: &["wifi off", "wi-fi off"] },
    Cmd { id: "radio:bt:on", title: "Turn Bluetooth on", keywords: &["bluetooth on"] },
    Cmd { id: "radio:bt:off", title: "Turn Bluetooth off", keywords: &["bluetooth off"] },
    Cmd { id: "nightlight", title: "Night light settings", keywords: &["night light"] },
    Cmd { id: "startmenu", title: "Start menu", keywords: &["start", "start menu"] },
];

pub struct SystemProvider {
    controls: Arc<dyn Controls>,
    launcher: Arc<dyn Launcher>,
    cache: Mutex<Option<Audio>>,
}

impl SystemProvider {
    pub fn new(controls: Arc<dyn Controls>, launcher: Arc<dyn Launcher>) -> Self {
        Self { controls, launcher, cache: Mutex::new(None) }
    }

    /// Re-read the audio state into the cache (none when it can't be read).
    /// Call off the query path, e.g. when the bar is shown.
    pub fn refresh(&self) {
        let audio = self.controls.audio().ok();
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = audio;
    }

    fn cached(&self) -> Option<Audio> {
        *self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn remember(&self, f: impl FnOnce(&mut Audio)) {
        if let Some(a) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            f(a);
        }
    }

    fn row(&self, id: &str, title: String, subtitle: String, score: u32, pinnable: bool) -> ResultItem {
        ResultItem {
            id: id.to_string(),
            provider: "system",
            title,
            subtitle,
            score,
            icon: "builtin:settings".into(),
            pinnable,
            ..Default::default()
        }
    }

    fn cmd_row(&self, c: &Cmd, score: u32) -> ResultItem {
        let audio = self.cached();
        let (title, subtitle) = match c.id {
            "mute" => (
                c.title.to_string(),
                match audio {
                    Some(a) if a.muted => "Already muted",
                    Some(_) => "Sound is on",
                    None => "Sound",
                }
                .to_string(),
            ),
            "vol" => (
                match audio {
                    Some(a) => format!("Volume {}%", a.volume),
                    None => c.title.to_string(),
                },
                "Sound".to_string(),
            ),
            "startmenu" => (c.title.to_string(), "Ctrl+Esc also opens it".to_string()),
            _ => (c.title.to_string(), "System".to_string()),
        };
        self.row(c.id, title, subtitle, score, true)
    }

    fn volume_row(&self, rest: &str) -> ResultItem {
        match rest {
            "up" => self.row("vol:+", "Volume up".into(), format!("{STEP}%"), PARSED_SCORE, false),
            "down" => self.row("vol:-", "Volume down".into(), format!("{STEP}%"), PARSED_SCORE, false),
            _ => match parse_percent(rest) {
                Some(n) => self.row(
                    &format!("vol:{n}"),
                    format!("Set volume to {n}%"),
                    "Sound".into(),
                    PARSED_SCORE,
                    false,
                ),
                None => self.row("wait", WAIT_TITLE.into(), "Sound".into(), WAIT_SCORE, false),
            },
        }
    }

    fn radio(&self, kind: RadioKind, on: bool, name: &str, settings: &str) -> Result<Outcome, String> {
        match self.controls.set_radio(kind, on) {
            Ok(()) => Ok(Outcome::Hide),
            Err(reason) => {
                let _ = self.launcher.open(settings);
                Err(format!("Couldn't switch {name}: {reason}. Opened Settings."))
            }
        }
    }
}

/// Digits only, 0..=100.
fn parse_percent(s: &str) -> Option<u8> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u8>().ok().filter(|n| *n <= 100)
}

/// Optional sign, digits, optional decimal part, optional `%`: a number the
/// user is still working on, as opposed to a word like `mixer`.
fn looks_numeric(s: &str) -> bool {
    let s = s.strip_suffix('%').unwrap_or(s);
    let s = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (int, frac) = s.split_once('.').map_or((s, ""), |(i, f)| (i, f));
    !int.is_empty()
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.bytes().all(|b| b.is_ascii_digit())
}

/// Lowercase with single spaces.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn volume_rest(text: &str) -> Option<&str> {
    text.strip_prefix("volume ").or_else(|| text.strip_prefix("vol ")).map(str::trim)
}

impl Provider for SystemProvider {
    fn name(&self) -> &'static str {
        "system"
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = normalize(&q.text);
        if text.is_empty() {
            return Vec::new();
        }
        let mut needles = vec![text.as_str()];
        if let Some(rest) = volume_rest(&text) {
            if matches!(rest, "up" | "down") || looks_numeric(rest) {
                return vec![self.volume_row(rest)];
            }
            // `volume mute`: the word after it is matched on its own too.
            needles.push(rest);
        }
        let mut hits: Vec<ResultItem> = CMDS
            .iter()
            .filter_map(|c| {
                let best = c
                    .keywords
                    .iter()
                    .flat_map(|k| needles.iter().filter_map(move |n| fuzzy::score(n, k)))
                    .max()?;
                Some(self.cmd_row(c, best.saturating_sub(POWER_PENALTY).max(1)))
            })
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score));
        hits
    }

    fn item(&self, id: &str) -> Option<ResultItem> {
        CMDS.iter().find(|c| c.id == id).map(|c| self.cmd_row(c, 0))
    }

    /// Never learned or kept as a recent: a habitual Enter in the empty bar must not
    /// toggle Wi-Fi or skip a track again. (Pins still work, through `item`.)
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: "mute / vol 30 / next song / dark mode / wifi off".into(),
            about: "sound, media keys, theme, Wi-Fi, Bluetooth and night light".into(),
            fill: String::new(),
        }]
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        match id {
            "wait" => return Ok(Outcome::Keep),
            "vol" => return Ok(Outcome::Hide),
            "mute" | "unmute" => {
                let on = id == "mute";
                self.controls.set_muted(on)?;
                self.remember(|a| a.muted = on);
            }
            "vol:+" | "vol:-" => {
                let now = self.controls.audio()?.volume;
                let n = if id == "vol:+" { now.saturating_add(STEP).min(100) } else { now.saturating_sub(STEP) };
                self.controls.set_volume(n)?;
                self.remember(|a| a.volume = n);
            }
            "media:next" => self.controls.media(MediaKey::Next)?,
            "media:prev" => self.controls.media(MediaKey::Previous)?,
            "media:play" => self.controls.media(MediaKey::PlayPause)?,
            "theme:dark" => self.controls.set_dark(true)?,
            "theme:light" => self.controls.set_dark(false)?,
            "radio:wifi:on" => return self.radio(RadioKind::WiFi, true, "Wi-Fi", "ms-settings:network-wifi"),
            "radio:wifi:off" => return self.radio(RadioKind::WiFi, false, "Wi-Fi", "ms-settings:network-wifi"),
            "radio:bt:on" => return self.radio(RadioKind::Bluetooth, true, "Bluetooth", "ms-settings:bluetooth"),
            "radio:bt:off" => {
                return self.radio(RadioKind::Bluetooth, false, "Bluetooth", "ms-settings:bluetooth")
            }
            "nightlight" => self.launcher.open("ms-settings:nightlight")?,
            "startmenu" => return Ok(Outcome::HideThen { then: Then::StartMenu }),
            _ => {
                let n = id
                    .strip_prefix("vol:")
                    .and_then(parse_percent)
                    .ok_or_else(|| "unknown system action".to_string())?;
                self.controls.set_volume(n)?;
                self.remember(|a| a.volume = n);
            }
        }
        Ok(Outcome::Hide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::testing::RecordingLauncher;

    struct FakeControls {
        calls: Mutex<Vec<String>>,
        audio: Mutex<Result<Audio, String>>,
        radio_error: Mutex<Option<String>>,
    }

    impl FakeControls {
        fn new(audio: Result<Audio, String>) -> Arc<Self> {
            Arc::new(Self { calls: Mutex::default(), audio: Mutex::new(audio), radio_error: Mutex::new(None) })
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn log(&self, s: String) -> Result<(), String> {
            self.calls.lock().unwrap().push(s);
            Ok(())
        }
    }

    impl Controls for FakeControls {
        fn audio(&self) -> Result<Audio, String> {
            self.calls.lock().unwrap().push("audio".into());
            self.audio.lock().unwrap().clone()
        }
        fn set_volume(&self, pct: u8) -> Result<(), String> {
            self.log(format!("set_volume {pct}"))
        }
        fn set_muted(&self, on: bool) -> Result<(), String> {
            self.log(format!("set_muted {on}"))
        }
        fn media(&self, key: MediaKey) -> Result<(), String> {
            self.log(format!("media {key:?}"))
        }
        fn set_dark(&self, dark: bool) -> Result<(), String> {
            self.log(format!("set_dark {dark}"))
        }
        fn set_radio(&self, kind: RadioKind, on: bool) -> Result<(), String> {
            self.log(format!("set_radio {kind:?} {on}"))?;
            match self.radio_error.lock().unwrap().clone() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
    }

    fn setup(audio: Result<Audio, String>) -> (SystemProvider, Arc<FakeControls>, Arc<RecordingLauncher>) {
        let c = FakeControls::new(audio);
        let l = Arc::new(RecordingLauncher::default());
        (SystemProvider::new(c.clone(), l.clone()), c, l)
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    fn only(p: &SystemProvider, text: &str) -> ResultItem {
        let r = p.query(&q(text));
        assert!(!r.is_empty(), "{text}");
        r.into_iter().next().unwrap()
    }

    #[test]
    fn typing_never_changes_anything() {
        let (p, c, l) = setup(Ok(Audio { volume: 40, muted: false }));
        for t in [
            "mute", "sound off", "unmute", "sound on", "vol", "volume", "vol 30", "volume 5", "vol up",
            "vol down", "vol 3.5", "volume mute", "next song", "next track", "previous song", "prev song", "play",
            "pause", "play pause", "dark mode", "light mode", "wifi on", "wi-fi off", "bluetooth on",
            "bluetooth off", "night light",
        ] {
            assert!(!p.query(&q(t)).is_empty(), "{t}");
        }
        let _ = p.empty_query();
        let _ = p.item("mute");
        assert!(c.calls().is_empty());
        assert!(l.calls().is_empty());
    }

    #[test]
    fn vol_30_sets_on_enter() {
        let (p, c, _) = setup(Err("x".into()));
        let r = only(&p, "vol 30");
        assert_eq!((r.id.as_str(), r.title.as_str(), r.score, r.pinnable), ("vol:30", "Set volume to 30%", 800, false));
        assert_eq!(p.activate("vol:30", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(c.calls(), vec!["set_volume 30"]);
    }

    #[test]
    fn volume_out_of_range_does_nothing() {
        let (p, c, _) = setup(Err("x".into()));
        for t in ["vol 300", "vol -5", "vol 3.5", "vol 120%"] {
            let r = p.query(&q(t));
            assert_eq!(r.len(), 1, "{t}");
            assert_eq!((r[0].id.as_str(), r[0].title.as_str()), ("wait", "Volume goes from 0 to 100"));
            assert_eq!(r[0].score, WAIT_SCORE);
            assert_eq!(p.activate(&r[0].id, Modifier::None), Ok(Outcome::Keep));
        }
        assert!(c.calls().is_empty());
    }

    #[test]
    fn words_after_vol_get_no_wait_row() {
        let (p, _, _) = setup(Err("x".into()));
        for t in ["vol abc", "volume mixer", "vol report"] {
            assert!(p.query(&q(t)).iter().all(|r| r.id != "wait"), "{t}");
        }
        let r = p.query(&q("volume mute"));
        assert!(r.iter().any(|r| r.id == "mute"), "volume mute offers Mute");
    }

    #[test]
    fn vol_up_clamps_at_100() {
        let (p, c, _) = setup(Ok(Audio { volume: 95, muted: false }));
        let r = only(&p, "vol up");
        assert_eq!((r.id.as_str(), r.title.as_str(), r.subtitle.as_str()), ("vol:+", "Volume up", "10%"));
        assert_eq!(p.activate("vol:+", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(c.calls(), vec!["audio", "set_volume 100"]);
    }

    #[test]
    fn vol_down_clamps_at_0() {
        let (p, c, _) = setup(Ok(Audio { volume: 4, muted: false }));
        assert_eq!(only(&p, "vol down").id, "vol:-");
        p.activate("vol:-", Modifier::None).unwrap();
        assert_eq!(c.calls(), vec!["audio", "set_volume 0"]);
    }

    #[test]
    fn mute_shows_cached_state() {
        let (p, _, _) = setup(Ok(Audio { volume: 40, muted: true }));
        assert_eq!(only(&p, "mute").subtitle, "Sound");
        p.refresh();
        assert_eq!(only(&p, "mute").subtitle, "Already muted");
        assert_eq!(only(&p, "vol").title, "Volume 40%");
    }

    #[test]
    fn unmuted_cache_says_sound_is_on() {
        let (p, _, _) = setup(Ok(Audio { volume: 40, muted: false }));
        p.refresh();
        assert_eq!(only(&p, "mute").subtitle, "Sound is on");
    }

    #[test]
    fn failed_refresh_caches_nothing() {
        let (p, _, _) = setup(Err("no device".into()));
        p.refresh();
        assert_eq!(only(&p, "vol").title, "Volume");
    }

    #[test]
    fn mute_and_unmute_call_set_muted() {
        let (p, c, _) = setup(Err("x".into()));
        p.activate("mute", Modifier::None).unwrap();
        p.activate("unmute", Modifier::None).unwrap();
        assert_eq!(c.calls(), vec!["set_muted true", "set_muted false"]);
    }

    #[test]
    fn media_and_theme_rows_send_the_right_call() {
        let (p, c, _) = setup(Err("x".into()));
        for (text, id, want) in [
            ("next song", "media:next", "media Next"),
            ("previous track", "media:prev", "media Previous"),
            ("pause", "media:play", "media PlayPause"),
            ("dark mode", "theme:dark", "set_dark true"),
            ("light mode", "theme:light", "set_dark false"),
        ] {
            assert_eq!(only(&p, text).id, id, "{text}");
            assert_eq!(p.activate(id, Modifier::None), Ok(Outcome::Hide));
            assert_eq!(c.calls().last().unwrap(), want);
        }
    }

    #[test]
    fn radio_rows_switch_the_radio() {
        let (p, c, _) = setup(Err("x".into()));
        assert_eq!(only(&p, "wi-fi off").id, "radio:wifi:off");
        assert_eq!(only(&p, "bluetooth on").title, "Turn Bluetooth on");
        assert_eq!(p.activate("radio:bt:on", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(c.calls(), vec!["set_radio Bluetooth true"]);
    }

    #[test]
    fn radio_failure_opens_settings_and_says_so() {
        let (p, c, l) = setup(Err("x".into()));
        *c.radio_error.lock().unwrap() = Some("no radio".into());
        let e = p.activate("radio:wifi:on", Modifier::None).unwrap_err();
        assert_eq!(e, "Couldn't switch Wi-Fi: no radio. Opened Settings.");
        assert!(e.contains("Opened Settings"));
        assert_eq!(l.calls(), vec!["open ms-settings:network-wifi"]);
        let e = p.activate("radio:bt:off", Modifier::None).unwrap_err();
        assert!(e.starts_with("Couldn't switch Bluetooth"));
        assert_eq!(l.calls().last().unwrap(), "open ms-settings:bluetooth");
    }

    #[test]
    fn night_light_opens_its_settings_page() {
        let (p, _, l) = setup(Err("x".into()));
        assert_eq!(only(&p, "night light").id, "nightlight");
        assert_eq!(p.activate("nightlight", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(l.calls(), vec!["open ms-settings:nightlight"]);
    }

    #[test]
    fn start_menu_row_shows_for_start_words() {
        let (p, _, _) = setup(Err("x".into()));
        for text in ["start", "start menu", "start m"] {
            let r = only(&p, text);
            assert_eq!(r.id, "startmenu", "{text}");
            assert_eq!(r.title, "Start menu");
            assert_eq!(r.subtitle, "Ctrl+Esc also opens it");
        }
    }

    #[test]
    fn start_menu_hides_then_opens_start() {
        let (p, c, l) = setup(Err("x".into()));
        assert_eq!(p.activate("startmenu", Modifier::None), Ok(Outcome::HideThen { then: Then::StartMenu }));
        assert!(c.calls().is_empty());
        assert!(l.calls().is_empty());
    }

    #[test]
    fn a_same_named_app_ranks_first() {
        let (p, _, _) = setup(Err("x".into()));
        let r = only(&p, "mute");
        assert_eq!(r.score, fuzzy::score("mute", "mute").unwrap() - POWER_PENALTY);
        assert!(r.score < fuzzy::score("mute", "mute").unwrap());
    }

    #[test]
    fn pinnable_rows_rebuild_and_the_rest_do_not() {
        let (p, _, _) = setup(Err("x".into()));
        for c in CMDS {
            let r = p.item(c.id).unwrap();
            assert!(r.pinnable && r.score == 0, "{}", c.id);
        }
        for id in ["vol:30", "vol:+", "vol:-", "wait", "nope"] {
            assert!(p.item(id).is_none(), "{id}");
        }
        assert_eq!(p.rank_key("vol:30"), None);
        assert_eq!(p.rank_key("mute"), None);
        for c in CMDS {
            assert_eq!(p.rank_key(c.id), None, "{}", c.id);
        }
    }

    #[test]
    fn unknown_id_does_nothing() {
        let (p, c, _) = setup(Err("x".into()));
        assert!(p.activate("vol:500", Modifier::None).is_err());
        assert!(p.activate("nope", Modifier::None).is_err());
        assert!(c.calls().is_empty());
    }
}
