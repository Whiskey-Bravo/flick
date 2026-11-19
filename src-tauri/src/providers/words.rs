//! `tr <text>` translates between two languages (German and English unless the
//! config's `tr_languages` says otherwise; `tr >fr <text>` into any one of
//! [`LANGUAGES`]) through MyMemory; `def <word>`
//! shows definitions from English Wiktionary. Online, after typing pauses, and
//! only the typed text is sent; answers are kept in memory only.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::answers::lookup::{Lookup, LookupError, Lookups, PAUSE};
pub use crate::clipboard::Clipboard;
use crate::dirs;
use crate::engine::query_seq;
use crate::net::{Fetch, FetchError};
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem};
use crate::providers::flick::Opener;
use crate::providers::shortcuts::encode_url_component;

const NAME: &str = "words";
const TOP_SCORE: u32 = 1000;
const COPY: &str = "copy:";
const INERT: &str = "inert:";
const ONLINE_OFF: &str = "online-off";
const TRANSLATE_ICON: &str = "builtin:translate";
const DEFINE_ICON: &str = "builtin:define";
/// Longest text `tr` sends, in characters.
const MAX_TRANSLATE: usize = 200;
/// The words this provider claims (with text after them).
pub const WORDS: [&str; 2] = ["tr", "def"];
/// Senses shown per language.
const SENSES: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    German,
    English,
}

impl Lang {
    pub fn name(self) -> &'static str {
        match self {
            Lang::German => "German",
            Lang::English => "English",
        }
    }
}

/// Common German words that are not also everyday English words.
const GERMAN_WORDS: [&str; 64] = [
    "der", "die", "das", "den", "dem", "des", "ein", "eine", "einen", "einem", "einer", "kein", "keine", "ich",
    "du", "er", "sie", "es", "wir", "ihr", "mich", "mir", "dich", "dir", "uns", "euch", "mein", "meine", "dein",
    "deine", "sein", "seine", "und", "oder", "aber", "nicht", "ist", "sind", "bist", "habe", "haben", "wird",
    "auch", "noch", "schon", "sehr", "bitte", "danke", "guten", "gute", "morgen", "tag", "nacht", "abend",
    "hallo", "ja", "nein", "wie", "wo", "warum", "mit", "von", "zu", "nach",
];

/// German when the text has `ä ö ü ß` or a common German word; else English.
pub fn source_language(text: &str) -> Lang {
    let lower = text.to_lowercase();
    if lower.contains(['ä', 'ö', 'ü', 'ß']) {
        return Lang::German;
    }
    let german = lower.split(|c: char| !c.is_alphabetic()).any(|w| GERMAN_WORDS.contains(&w));
    if german {
        Lang::German
    } else {
        Lang::English
    }
}

/// The languages `tr` can translate between: MyMemory's code and the name
/// shown. Settings offers these, and `tr >code` takes these codes.
pub const LANGUAGES: [(&str, &str); 28] = [
    ("ar", "Arabic"),
    ("bg", "Bulgarian"),
    ("zh-CN", "Chinese"),
    ("hr", "Croatian"),
    ("cs", "Czech"),
    ("da", "Danish"),
    ("nl", "Dutch"),
    ("en", "English"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("de", "German"),
    ("el", "Greek"),
    ("hi", "Hindi"),
    ("hu", "Hungarian"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("no", "Norwegian"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("es", "Spanish"),
    ("sv", "Swedish"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
];

/// The pair `tr` uses when the config names none.
pub const DEFAULT_PAIR: (&str, &str) = ("de", "en");

/// The listed code that `code` names, in any case (`ZH-cn` is `zh-CN`).
pub fn language_code(code: &str) -> Option<&'static str> {
    LANGUAGES.iter().map(|(c, _)| *c).find(|c| c.eq_ignore_ascii_case(code.trim()))
}

/// The name shown for a language code: the listed name, also for a regional
/// form of a listed code (`de-DE` is German); else the code itself.
pub fn language_name(code: &str) -> String {
    let base = code.split(['-', '_']).next().unwrap_or(code);
    LANGUAGES
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(code) || c.split('-').next().is_some_and(|b| b.eq_ignore_ascii_case(base)))
        .map_or_else(|| code.to_string(), |(_, name)| name.to_string())
}

/// Checks the config's `tr_languages`: empty (the default pair), or two
/// different listed codes.
pub fn validate_languages(pair: &[String]) -> Result<(), String> {
    match pair {
        [] => Ok(()),
        [a, b] => match (language_code(a), language_code(b)) {
            (Some(a), Some(b)) if a != b => Ok(()),
            (Some(_), Some(_)) => Err("tr_languages must name two different languages".into()),
            _ => Err("tr_languages must name two language codes such as \"de\" and \"en\"".into()),
        },
        _ => Err("tr_languages must name exactly two languages".into()),
    }
}

/// How one `tr` text is translated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// From and to are known before asking (German and English: the source
    /// is guessed from the text, as it always was). One request.
    Fixed { from: &'static str, to: &'static str },
    /// Into this language, whatever the text is in (`tr >fr ...`). One
    /// request; the service says what it took the text for.
    Into(&'static str),
    /// Between your language and another: into `mine` first, and when the
    /// service finds the text already is `mine`, from `mine` into `other`
    /// (a second request).
    Between { mine: &'static str, other: &'static str },
}

/// A `>code` in front of the text names the language to translate into.
/// Returns that language and the rest; a code that is not listed leaves the
/// text as it is.
pub fn explicit_target(text: &str) -> (Option<&'static str>, &str) {
    let Some(rest) = text.strip_prefix('>') else { return (None, text) };
    let (code, body) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    match language_code(code) {
        Some(code) => (Some(code), body.trim()),
        None => (None, text),
    }
}

/// The plan for `text` (already without any `>code`), given an explicit
/// target and the configured pair.
pub fn plan_for(text: &str, target: Option<&'static str>, pair: (&'static str, &'static str)) -> Plan {
    if let Some(to) = target {
        return Plan::Into(to);
    }
    let german_english = matches!(pair, ("de", "en") | ("en", "de"));
    if german_english {
        return match source_language(text) {
            Lang::German => Plan::Fixed { from: "de", to: "en" },
            Lang::English => Plan::Fixed { from: "en", to: "de" },
        };
    }
    Plan::Between { mine: pair.0, other: pair.1 }
}

/// MyMemory's address for `text` from `from` (a code, or `autodetect`) into `to`.
pub fn translate_url(text: &str, from: &str, to: &str) -> String {
    format!(
        "https://api.mymemory.translated.net/get?q={}&langpair={}%7C{}",
        encode_url_component(text),
        encode_url_component(from),
        encode_url_component(to)
    )
}

/// A translation and the languages it went between (codes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Translation {
    pub text: String,
    pub from: String,
    pub to: String,
}

/// The language MyMemory took an `autodetect` text for, if it says.
pub fn detected_language(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v["responseData"]["detectedLanguage"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

/// Whether two codes name the same language, ignoring region and case
/// (`de-DE` and `de`).
fn same_language(a: &str, b: &str) -> bool {
    let base = |c: &str| c.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    !base(a).is_empty() && base(a) == base(b)
}

/// The translation in a MyMemory answer.
pub fn parse_translation(body: &[u8]) -> Result<String, LookupError> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|_| LookupError::Bad)?;
    let status_ok = match &v["responseStatus"] {
        serde_json::Value::Number(n) => n.as_u64() == Some(200),
        serde_json::Value::String(s) => s.trim() == "200",
        _ => false,
    };
    let text = v["responseData"]["translatedText"].as_str().map(decode_entities).unwrap_or_default();
    let text = text.trim();
    if status_ok && !text.is_empty() {
        Ok(text.to_string())
    } else {
        Err(LookupError::Bad)
    }
}

/// English Wiktionary's REST definitions of `word`.
pub fn definition_url(word: &str) -> String {
    format!("https://en.wiktionary.org/api/rest_v1/page/definition/{}", encode_url_component(word))
}

/// One meaning of a word.
#[derive(Clone, Debug, PartialEq)]
pub struct Sense {
    pub lang: Lang,
    /// Part of speech, lower case (`noun`).
    pub part: String,
    pub text: String,
}

/// German senses, then English ones, up to three of each; none is `NotFound`.
pub fn parse_definitions(body: &[u8]) -> Result<Vec<Sense>, LookupError> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|_| LookupError::Bad)?;
    if !v.is_object() {
        return Err(LookupError::Bad);
    }
    let mut senses = Vec::new();
    for (key, lang) in [("de", Lang::German), ("en", Lang::English)] {
        let mut taken = 0;
        for entry in v[key].as_array().into_iter().flatten() {
            let part = entry["partOfSpeech"].as_str().unwrap_or("").trim().to_lowercase();
            for def in entry["definitions"].as_array().into_iter().flatten() {
                if taken == SENSES {
                    break;
                }
                let text = plain_text(def["definition"].as_str().unwrap_or(""));
                if !text.is_empty() {
                    senses.push(Sense { lang, part: part.clone(), text });
                    taken += 1;
                }
            }
        }
    }
    if senses.is_empty() {
        Err(LookupError::NotFound)
    } else {
        Ok(senses)
    }
}

/// Text of a bit of HTML: tags dropped, common entities decoded, whitespace collapsed.
pub fn plain_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    decode_entities(&text).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `&amp; &lt; &gt; &quot; &#39; &nbsp;` and numeric entities; others stay as typed.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        // Bytes, not a str slice: byte 12 may fall inside a letter, and `;` is
        // ASCII, so the name always ends on a letter boundary.
        let semicolon = rest.as_bytes()[1..12.min(rest.len())].iter().position(|&b| b == b';');
        let decoded = semicolon.and_then(|end| {
            let name = &rest[1..1 + end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => {
                    let num = name.strip_prefix('#')?;
                    let code = match num.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => num.parse().ok()?,
                    };
                    char::from_u32(code)
                }
            }?;
            Some((c, end + 2))
        });
        match decoded {
            Some((c, len)) => {
                out.push(if c == '\u{a0}' { ' ' } else { c });
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `word` with its first letter's case flipped; `None` when it has no case.
fn flip_first(word: &str) -> Option<String> {
    let mut chars = word.chars();
    let first = chars.next()?;
    let flipped: String = if first.is_lowercase() {
        first.to_uppercase().collect()
    } else if first.is_uppercase() {
        first.to_lowercase().collect()
    } else {
        return None;
    };
    Some(flipped + chars.as_str())
}

fn fetch_translation(url: &str, fetch: &dyn Fetch) -> Result<(String, Option<String>), LookupError> {
    match fetch.get(url) {
        Ok(body) => parse_translation(&body).map(|text| (text, detected_language(&body))),
        Err(FetchError::Offline) => Err(LookupError::Offline),
        Err(_) => Err(LookupError::Bad),
    }
}

fn translate_job(text: &str, plan: &Plan, fetch: &dyn Fetch) -> Result<Translation, LookupError> {
    match plan {
        Plan::Fixed { from, to } => {
            let (text, _) = fetch_translation(&translate_url(text, from, to), fetch)?;
            Ok(Translation { text, from: from.to_string(), to: to.to_string() })
        }
        Plan::Into(to) => {
            let (text, detected) = fetch_translation(&translate_url(text, "autodetect", to), fetch)?;
            Ok(Translation { text, from: detected.unwrap_or_default(), to: to.to_string() })
        }
        Plan::Between { mine, other } => {
            let (first, detected) = fetch_translation(&translate_url(text, "autodetect", mine), fetch)?;
            match detected {
                // Already your language: the other way round.
                Some(d) if same_language(&d, mine) => {
                    let (text, _) = fetch_translation(&translate_url(text, mine, other), fetch)?;
                    Ok(Translation { text, from: mine.to_string(), to: other.to_string() })
                }
                detected => Ok(Translation { text: first, from: detected.unwrap_or_default(), to: mine.to_string() }),
            }
        }
    }
}

/// The senses on Wiktionary's page for `word`; no page is `NotFound`.
fn definitions_of(word: &str, fetch: &dyn Fetch) -> Result<Vec<Sense>, LookupError> {
    match fetch.get(&definition_url(word)) {
        Ok(body) => parse_definitions(&body),
        Err(FetchError::Status(404)) => Err(LookupError::NotFound),
        Err(FetchError::Offline) => Err(LookupError::Offline),
        Err(_) => Err(LookupError::Bad),
    }
}

/// What `def` found for a word: its senses (none means no definition), and
/// whether every page asked for was read. An incomplete answer is shown but
/// not kept, so the next ask looks the word up again.
#[derive(Debug, PartialEq)]
pub struct Definitions {
    pub senses: Vec<Sense>,
    pub complete: bool,
}

/// The word as typed, then (Wiktionary titles being case-sensitive: `gift` is
/// English, `Gift` German) once more with the first letter's case flipped when
/// the first page has no page, no senses, or senses in only one of German and
/// English. A language missing from the first page is taken from the second,
/// German first; when the second has no page or no senses, the first page's
/// answer stands. The second request is sent only while `wanted()` (the word is
/// still the text asked last, and online answers are on). When it is not sent,
/// or fails, the first page's answer stands but is incomplete.
fn define_job(word: &str, fetch: &dyn Fetch, wanted: &dyn Fn() -> bool) -> Result<Definitions, LookupError> {
    let first = definitions_of(word, fetch);
    let has = |senses: &[Sense], lang: Lang| senses.iter().any(|s| s.lang == lang);
    let wants_more = match &first {
        Ok(senses) => !(has(senses, Lang::German) && has(senses, Lang::English)),
        Err(e) => *e == LookupError::NotFound,
    };
    let whole = |senses| Ok(Definitions { senses, complete: true });
    let partial = |senses| Ok(Definitions { senses, complete: false });
    let Some(other) = flip_first(word).filter(|_| wants_more) else {
        return first.and_then(whole);
    };
    if !wanted() {
        // The other page was never read: no definition yet is not "none".
        return match first {
            Ok(senses) => partial(senses),
            Err(LookupError::NotFound) => partial(Vec::new()),
            Err(e) => Err(e),
        };
    }
    let second = definitions_of(&other, fetch);
    let first = match first {
        Ok(senses) => senses,
        Err(_) => return second.and_then(whole),
    };
    let second = match second {
        Ok(senses) => senses,
        Err(LookupError::NotFound) => return whole(first),
        Err(_) => return partial(first),
    };
    let mut merged = Vec::new();
    for lang in [Lang::German, Lang::English] {
        let source = if has(&first, lang) { &first } else { &second };
        merged.extend(source.iter().filter(|s| s.lang == lang).cloned());
    }
    whole(merged)
}

#[derive(Clone, Copy, PartialEq)]
enum Command {
    Translate,
    Define,
}

/// `tr <text>` / `def <word>` (the word in any case) and the trimmed rest, when not empty.
fn split(text: &str) -> Option<(Command, &str)> {
    let text = text.trim();
    let (cmd, len) = if text.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("tr ")) {
        (Command::Translate, 3)
    } else if text.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("def ")) {
        (Command::Define, 4)
    } else {
        return None;
    };
    let rest = text[len..].trim();
    (!rest.is_empty()).then_some((cmd, rest))
}

pub struct WordsProvider {
    translations: Arc<Lookups<Translation>>,
    /// The config's pair: your language and the other one.
    pair: Mutex<(&'static str, &'static str)>,
    definitions: Arc<Lookups<Definitions>>,
    clipboard: Arc<dyn Clipboard>,
    open_settings: Opener,
    online: AtomicBool,
    /// Copy text of the answer rows of the latest query, by the `<n>` of their id.
    copies: Mutex<Vec<String>>,
    /// The number (`engine::query_seq`) of the newest query that reached this
    /// provider. Queries overlap: an older one arriving late changes nothing.
    newest: AtomicU64,
    /// The number of the query this provider answered, until that query's
    /// `skipped` (the engine calls `skipped` on every provider after a routed query).
    answered: Mutex<Option<u64>>,
}

impl WordsProvider {
    pub fn new(
        fetch: Arc<dyn Fetch>,
        clipboard: Arc<dyn Clipboard>,
        open_settings: Opener,
        on_ready: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self::build(fetch, clipboard, open_settings, on_ready, PAUSE)
    }

    #[cfg(test)]
    pub fn with_pause(
        fetch: Arc<dyn Fetch>,
        clipboard: Arc<dyn Clipboard>,
        open_settings: Opener,
        on_ready: Arc<dyn Fn() + Send + Sync>,
        pause: Duration,
    ) -> Self {
        Self::build(fetch, clipboard, open_settings, on_ready, pause)
    }

    fn build(
        fetch: Arc<dyn Fetch>,
        clipboard: Arc<dyn Clipboard>,
        open_settings: Opener,
        on_ready: Arc<dyn Fn() + Send + Sync>,
        pause: Duration,
    ) -> Self {
        WordsProvider {
            translations: Lookups::new(fetch.clone(), on_ready.clone(), pause),
            definitions: Lookups::keeping(fetch, on_ready, pause, |d| d.complete),
            pair: Mutex::new(DEFAULT_PAIR),
            clipboard,
            open_settings,
            online: AtomicBool::new(true),
            copies: Mutex::new(Vec::new()),
            newest: AtomicU64::new(0),
            answered: Mutex::new(None),
        }
    }

    /// True for the newest query so far, whose number it keeps; false for an
    /// older query that arrives after a newer one (its answer is dropped by the
    /// bar anyway), which must neither ask nor forget. Calls outside a query
    /// (number 0) always count as the newest.
    fn is_newest(&self) -> bool {
        let seq = query_seq();
        seq == 0 || self.newest.fetch_max(seq, Ordering::SeqCst) <= seq
    }

    /// Follows the config's `tr_languages`: empty or not two listed codes
    /// (config validation prevents that) is the default pair.
    pub fn set_languages(&self, pair: &[String]) {
        let chosen = match pair {
            [a, b] => language_code(a).zip(language_code(b)).filter(|(a, b)| a != b),
            _ => None,
        };
        *self.pair.lock().unwrap() = chosen.unwrap_or(DEFAULT_PAIR);
    }

    /// Follows the `online` config key: off means nothing is ever fetched.
    /// Turning it off waits for a query already past its checks (it holds
    /// `copies`), so nothing that query asked for is sent.
    pub fn set_online(&self, on: bool) {
        if on {
            self.online.store(true, Ordering::Relaxed);
            return;
        }
        let _turn = self.copies.lock().unwrap();
        self.online.store(false, Ordering::Relaxed);
        self.forget_waiting();
    }

    /// The bar hid: drops the waiting text, after any query already past its
    /// checks (it holds `copies`) has asked. Not inside `forget_waiting`, which
    /// runs while `copies` is held.
    pub fn hide(&self) {
        let _turn = self.copies.lock().unwrap();
        self.forget_waiting();
    }

    /// Drops the text waiting for the pause (the bar hid): nothing is sent for it.
    pub fn forget_waiting(&self) {
        self.translations.forget_waiting();
        self.definitions.forget_waiting();
    }

    /// The rows for `text`, once `query` found its query the newest and holds
    /// `copies`.
    fn answer(&self, text: &str, copies: &mut Vec<String>) -> Vec<ResultItem> {
        copies.clear();
        let Some((cmd, rest)) = split(text) else {
            *self.answered.lock().unwrap() = None;
            self.forget_waiting();
            return vec![];
        };
        *self.answered.lock().unwrap() = Some(query_seq());
        match cmd {
            Command::Translate => self.translate(rest, copies),
            Command::Define => self.define(rest, copies),
        }
    }

    fn translate(&self, text: &str, copies: &mut Vec<String>) -> Vec<ResultItem> {
        self.definitions.forget_waiting();
        if text.chars().count() > MAX_TRANSLATE {
            self.translations.forget_waiting();
            return vec![inert("Too long to translate — 200 characters at most", TRANSLATE_ICON)];
        }
        if !self.online.load(Ordering::Relaxed) {
            self.translations.forget_waiting();
            return vec![online_off(TRANSLATE_ICON)];
        }
        let (target, body) = explicit_target(text);
        if body.is_empty() {
            self.translations.forget_waiting();
            return vec![inert("Type the text to translate after the language", TRANSLATE_ICON)];
        }
        let plan = plan_for(body, target, *self.pair.lock().unwrap());
        // The same text under another plan is another lookup.
        let key = format!("{plan:?}\u{0}{body}");
        let owned = body.to_string();
        match self.translations.ask(&key, move |f, _| translate_job(&owned, &plan, f)) {
            Lookup::Waiting => vec![inert("Translating…", TRANSLATE_ICON)],
            Lookup::Ready(t) => {
                copies.push(t.text.clone());
                let from = if t.from.is_empty() { "?".to_string() } else { language_name(&t.from) };
                vec![ResultItem {
                    id: format!("{COPY}0"),
                    provider: NAME,
                    title: dirs::one_line(&t.text, MAX_TRANSLATE),
                    subtitle: format!("{from} → {}", language_name(&t.to)),
                    score: TOP_SCORE,
                    icon: TRANSLATE_ICON.into(),
                    top: true,
                    ..Default::default()
                }]
            }
            Lookup::Failed(_) => vec![inert("Can't reach the translation service", TRANSLATE_ICON)],
        }
    }

    fn define(&self, word: &str, copies: &mut Vec<String>) -> Vec<ResultItem> {
        self.translations.forget_waiting();
        if !self.online.load(Ordering::Relaxed) {
            self.definitions.forget_waiting();
            return vec![online_off(DEFINE_ICON)];
        }
        let owned = word.to_string();
        match self.definitions.ask(word, move |f, wanted| define_job(&owned, f, wanted)) {
            Lookup::Waiting => vec![inert("Looking up…", DEFINE_ICON)],
            Lookup::Ready(found) if found.senses.is_empty() => vec![inert("No definition found", DEFINE_ICON)],
            Lookup::Ready(found) => found
                .senses
                .iter()
                .enumerate()
                .map(|(n, s)| {
                    copies.push(s.text.clone());
                    ResultItem {
                        id: format!("{COPY}{n}"),
                        provider: NAME,
                        title: dirs::one_line(&s.text, 200),
                        subtitle: format!("{} · {}", s.lang.name(), s.part),
                        score: TOP_SCORE.saturating_sub(n as u32),
                        icon: DEFINE_ICON.into(),
                        ..Default::default()
                    }
                })
                .collect(),
            Lookup::Failed(LookupError::NotFound) => vec![inert("No definition found", DEFINE_ICON)],
            Lookup::Failed(_) => vec![inert("Can't reach Wiktionary", DEFINE_ICON)],
        }
    }
}

fn inert(title: &str, icon: &str) -> ResultItem {
    ResultItem {
        id: format!("{INERT}{title}"),
        provider: NAME,
        title: title.to_string(),
        score: TOP_SCORE,
        icon: icon.into(),
        ..Default::default()
    }
}

fn online_off(icon: &str) -> ResultItem {
    ResultItem { id: ONLINE_OFF.into(), ..inert("Online answers are off", icon) }
}

impl Provider for WordsProvider {
    fn name(&self) -> &'static str {
        NAME
    }

    /// Holds `copies` from the newest-query check to the ask, as `skipped` and
    /// `empty_query` do: a newer query's forgetting never falls in between.
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let mut copies = self.copies.lock().unwrap();
        if !self.is_newest() {
            return vec![];
        }
        self.answer(&q.text, &mut copies)
    }

    /// Text that went elsewhere (another route, a scope, help): what was waiting is not sent.
    /// An older query arriving late forgets nothing.
    fn skipped(&self) {
        let _turn = self.copies.lock().unwrap();
        if !self.is_newest() {
            return;
        }
        if self.answered.lock().unwrap().take() != Some(query_seq()) {
            self.forget_waiting();
        }
    }

    /// The bar was cleared (the engine calls neither `query` nor `skipped` then):
    /// what was waiting is not sent. Memory only.
    fn empty_query(&self) -> Vec<ResultItem> {
        let _turn = self.copies.lock().unwrap();
        if self.is_newest() {
            *self.answered.lock().unwrap() = None;
            self.forget_waiting();
        }
        vec![]
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        if id == ONLINE_OFF {
            (self.open_settings)()?;
            return Ok(Outcome::Hide);
        }
        if id.starts_with(INERT) {
            return Ok(Outcome::Keep);
        }
        let n: usize = id.strip_prefix(COPY).and_then(|n| n.parse().ok()).ok_or("unknown item")?;
        let text = self.copies.lock().unwrap().get(n).cloned().ok_or("unknown item")?;
        self.clipboard.set_text(&text)?;
        Ok(Outcome::Hide)
    }

    /// Typed text: never learned, saved or pinned.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn claims(&self, text: &str) -> bool {
        split(text).is_some()
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![
            HelpLine {
                example: "tr guten Morgen".into(),
                about: "translate between German and English (online)".into(),
                fill: "tr".into(),
            },
            HelpLine {
                example: "def serendipity".into(),
                about: "definitions from Wiktionary (online)".into(),
                fill: "def".into(),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::FakeClipboard;
    use crate::net::FakeFetch;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    const P: Duration = Duration::from_millis(100);
    const WAIT: Duration = Duration::from_secs(2);

    const MYMEMORY_GUTEN_MORGEN: &str = r#"{"responseData":{"translatedText":"good morning","match":0.99},"quotaFinished":false,"mtLangSupported":null,"responseDetails":"","responseStatus":200,"responderId":null,"exception_code":null,"matches":[{"id":"454937721","segment":"guten Morgen","translation":"good morning","source":"de-DE","target":"en-GB","quality":"74","reference":null,"usage-count":2,"subject":"","created-by":"MateCat","last-updated-by":"MateCat","create-date":"2023-02-14 10:41:12","last-update-date":"2023-02-14 10:41:12","match":0.99}]}"#;
    const MYMEMORY_ENTITIES: &str = r#"{"responseData":{"translatedText":"it&#39;s &quot;fine&quot; &amp; good","match":0.85},"quotaFinished":false,"responseDetails":"","responseStatus":"200","matches":[]}"#;
    const MYMEMORY_ERROR: &str = r#"{"responseData":{"translatedText":"INVALID LANGUAGE PAIR SPECIFIED. EXAMPLE: LANGPAIR=EN|IT USING 2 LETTER ISO OR RFC3066 LIKE ZH-CN. ALMOST ALL LANGUAGES SUPPORTED BUT SOME MAY HAVE NO CONTENT"},"quotaFinished":null,"mtLangSupported":null,"responseDetails":"INVALID LANGUAGE PAIR SPECIFIED.","responseStatus":"403","responderId":null,"exception_code":null,"matches":""}"#;

    /// Shaped like `rest_v1/page/definition/Haus`: German (2 senses), English
    /// (4 non-empty senses over two parts of speech, one empty), and a language
    /// that is never shown.
    const WIKTIONARY_HAUS: &str = r#"{"de":[{"partOfSpeech":"Noun","language":"German","definitions":[{"definition":"<a rel=\"mw:WikiLink\" href=\"/wiki/house\" title=\"house\">house</a>","parsedExamples":[{"example":"Das <b>Haus</b> ist alt."}],"examples":["Das <b>Haus</b> ist alt."]},{"definition":"<a rel=\"mw:WikiLink\" href=\"/wiki/home\" title=\"home\">home</a>, <a rel=\"mw:WikiLink\" href=\"/wiki/household\" title=\"household\">household</a>"}]}],"en":[{"partOfSpeech":"Noun","language":"English","definitions":[{"definition":"A <i>structure</i> built &amp; serving   as a\n dwelling."},{"definition":""},{"definition":"<span class=\"use-with-mention\">A family</span>&nbsp;line."}]},{"partOfSpeech":"Verb","language":"English","definitions":[{"definition":"To <a href=\"/wiki/keep\">keep</a> within a structure."},{"definition":"To &lt;admit&gt; to residence."}]}],"lb":[{"partOfSpeech":"Noun","language":"Luxembourgish","definitions":[{"definition":"house"}]}]}"#;
    const WIKTIONARY_404: &str = r#"{"type":"https://mediawiki.org/wiki/HyperSwitch/errors/not_found","title":"Not found.","method":"get","detail":"Page or revision not found.","uri":"/en.wiktionary.org/v1/page/definition/haus"}"#;

    const MYMEMORY_DETECTED_DE: &str = r#"{"responseData":{"translatedText":"Bonjour","match":0.85,"detectedLanguage":"de"},"responseStatus":200,"matches":[]}"#;
    const MYMEMORY_DETECTED_FR: &str = r#"{"responseData":{"translatedText":"Bonjour","match":0.85,"detectedLanguage":"fr-FR"},"responseStatus":200,"matches":[]}"#;
    const MYMEMORY_GOOD_MORNING: &str = r#"{"responseData":{"translatedText":"Good morning","match":1},"responseStatus":200,"matches":[]}"#;

    /// Answers each address from a list, and remembers what was asked.
    struct Script(Vec<(String, &'static str)>, Mutex<Vec<String>>);

    impl Fetch for Script {
        fn get_until(&self, url: &str, _deadline: std::time::Instant) -> Result<Vec<u8>, FetchError> {
            self.1.lock().unwrap().push(url.to_string());
            self.0.iter().find(|(u, _)| u == url).map(|(_, body)| body.as_bytes().to_vec()).ok_or(FetchError::Offline)
        }
    }

    fn script(answers: &[(&str, &'static str)]) -> Script {
        Script(answers.iter().map(|(u, b)| (u.to_string(), *b)).collect(), Mutex::new(Vec::new()))
    }

    #[test]
    fn languages_are_listed_once_and_found_in_any_case() {
        let mut codes: Vec<&str> = LANGUAGES.iter().map(|(c, _)| *c).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), LANGUAGES.len());
        assert_eq!(language_code("FR"), Some("fr"));
        assert_eq!(language_code(" zh-cn "), Some("zh-CN"));
        assert_eq!(language_code("klingon"), None);
        assert_eq!(language_name("de"), "German");
        assert_eq!(language_name("de-DE"), "German");
        assert_eq!(language_name("zh-CN"), "Chinese");
        assert_eq!(language_name("xx"), "xx");
        assert!(language_code(DEFAULT_PAIR.0).is_some() && language_code(DEFAULT_PAIR.1).is_some());
    }

    #[test]
    fn a_language_after_an_arrow_is_the_target() {
        assert_eq!(explicit_target(">fr good morning"), (Some("fr"), "good morning"));
        assert_eq!(explicit_target(">ES  hola"), (Some("es"), "hola"));
        assert_eq!(explicit_target(">fr"), (Some("fr"), ""));
        // Not a listed code: the text is left as typed.
        assert_eq!(explicit_target(">xx hello"), (None, ">xx hello"));
        assert_eq!(explicit_target("good > morning"), (None, "good > morning"));
    }

    #[test]
    fn the_plan_follows_the_target_then_the_pair() {
        assert_eq!(plan_for("hello", Some("fr"), DEFAULT_PAIR), Plan::Into("fr"));
        // German and English, in either order: the source is guessed, as before.
        assert_eq!(plan_for("guten Morgen", None, ("de", "en")), Plan::Fixed { from: "de", to: "en" });
        assert_eq!(plan_for("good morning", None, ("en", "de")), Plan::Fixed { from: "en", to: "de" });
        assert_eq!(plan_for("bonjour", None, ("fr", "en")), Plan::Between { mine: "fr", other: "en" });
    }

    #[test]
    fn into_a_language_asks_once_and_names_what_was_detected() {
        let url = translate_url("guten Morgen", "autodetect", "fr");
        assert!(url.ends_with("&langpair=autodetect%7Cfr"), "{url}");
        let f = script(&[(&url, MYMEMORY_DETECTED_DE)]);
        let t = translate_job("guten Morgen", &Plan::Into("fr"), &f).unwrap();
        assert_eq!(t, Translation { text: "Bonjour".into(), from: "de".into(), to: "fr".into() });
        assert_eq!(f.1.lock().unwrap().len(), 1);
    }

    #[test]
    fn between_two_languages_goes_into_yours_or_out_of_it() {
        let plan = Plan::Between { mine: "fr", other: "en" };
        // A foreign text: into your language, one request.
        let into = translate_url("guten Morgen", "autodetect", "fr");
        let f = script(&[(&into, MYMEMORY_DETECTED_DE)]);
        let t = translate_job("guten Morgen", &plan, &f).unwrap();
        assert_eq!((t.text.as_str(), t.from.as_str(), t.to.as_str()), ("Bonjour", "de", "fr"));
        assert_eq!(f.1.lock().unwrap().len(), 1);
        // Already your language (a regional code counts): out into the other one.
        let first = translate_url("bonjour", "autodetect", "fr");
        let second = translate_url("bonjour", "fr", "en");
        let f = script(&[(&first, MYMEMORY_DETECTED_FR), (&second, MYMEMORY_GOOD_MORNING)]);
        let t = translate_job("bonjour", &plan, &f).unwrap();
        assert_eq!((t.text.as_str(), t.from.as_str(), t.to.as_str()), ("Good morning", "fr", "en"));
        assert_eq!(*f.1.lock().unwrap(), [first, second]);
        // The service down: an error, never a guess.
        assert_eq!(translate_job("bonjour", &plan, &script(&[])), Err(LookupError::Offline));
    }

    #[test]
    fn the_pair_setting_takes_two_different_listed_codes() {
        let pair = |a: &str, b: &str| vec![a.to_string(), b.to_string()];
        assert!(validate_languages(&[]).is_ok());
        assert!(validate_languages(&pair("fr", "EN")).is_ok());
        assert!(validate_languages(&pair("fr", "fr")).unwrap_err().contains("two different"));
        assert!(validate_languages(&pair("fr", "xx")).unwrap_err().contains("language codes"));
        assert!(validate_languages(&["fr".to_string()]).unwrap_err().contains("exactly two"));
    }

    const TR_HALLO_WELT: &str = "https://api.mymemory.translated.net/get?q=hallo%20welt&langpair=de%7Cen";
    const TR_GUTEN_MORGEN: &str = "https://api.mymemory.translated.net/get?q=guten%20Morgen&langpair=de%7Cen";
    const DEF_HAUS: &str = "https://en.wiktionary.org/api/rest_v1/page/definition/Haus";
    const DEF_HAUS_LOWER: &str = "https://en.wiktionary.org/api/rest_v1/page/definition/haus";

    struct Rig {
        p: WordsProvider,
        fetch: Arc<FakeFetch>,
        clipboard: Arc<FakeClipboard>,
        opened: Arc<AtomicUsize>,
        ready: mpsc::Receiver<()>,
    }

    fn rig_with(fetch: FakeFetch, pause: Duration) -> Rig {
        let fetch = Arc::new(fetch);
        let clipboard = Arc::new(FakeClipboard::default());
        let opened = Arc::new(AtomicUsize::new(0));
        let counter = opened.clone();
        let (tx, ready) = mpsc::channel();
        let tx = Mutex::new(tx);
        let p = WordsProvider::with_pause(
            fetch.clone(),
            clipboard.clone(),
            Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
            pause,
        );
        Rig { p, fetch, clipboard, opened, ready }
    }

    fn rig(fetch: FakeFetch) -> Rig {
        rig_with(fetch, P)
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    fn titles(rows: &[ResultItem]) -> Vec<&str> {
        rows.iter().map(|r| r.title.as_str()).collect()
    }

    impl Rig {
        fn wait(&self) {
            self.ready.recv_timeout(WAIT).expect("on_ready");
        }
    }

    #[test]
    fn language_guess() {
        for t in ["guten Morgen", "Straße", "ich bin müde", "STRASSE IST GROSS", "ÜBER"] {
            assert_eq!(source_language(t), Lang::German, "{t}");
        }
        for t in ["good morning", "hello", "Kindergarten rules", ""] {
            assert_eq!(source_language(t), Lang::English, "{t}");
        }
        assert_eq!((Lang::German.name(), Lang::English.name()), ("German", "English"));
    }

    #[test]
    fn urls_carry_only_the_text() {
        assert_eq!(translate_url("hallo welt", "de", "en"), TR_HALLO_WELT);
        assert_eq!(
            translate_url("a&b=c?", "en", "de"),
            "https://api.mymemory.translated.net/get?q=a%26b%3Dc%3F&langpair=en%7Cde"
        );
        assert_eq!(definition_url("Straße"), "https://en.wiktionary.org/api/rest_v1/page/definition/Stra%C3%9Fe");
    }

    #[test]
    fn translation_answers_are_parsed() {
        assert_eq!(parse_translation(MYMEMORY_GUTEN_MORGEN.as_bytes()), Ok("good morning".into()));
        assert_eq!(parse_translation(MYMEMORY_ENTITIES.as_bytes()), Ok("it's \"fine\" & good".into()));
        assert_eq!(parse_translation(MYMEMORY_ERROR.as_bytes()), Err(LookupError::Bad));
        assert_eq!(parse_translation(br#"{"responseData":{"translatedText":"  "},"responseStatus":200}"#), Err(LookupError::Bad));
        assert_eq!(parse_translation(b"<html>"), Err(LookupError::Bad));
    }

    #[test]
    fn plain_text_strips_and_decodes() {
        assert_eq!(plain_text("<b>a</b> &amp;&nbsp;b\n\t c"), "a & b c");
        assert_eq!(plain_text("&lt;x&gt; &quot;q&quot; &#39;s &#228; &#xE4; &bogus; & x"), "<x> \"q\" 's ä ä &bogus; & x");
        assert_eq!(plain_text("  "), "");
        // A letter of two bytes across byte 12 after the `&`.
        assert_eq!(plain_text("&#39;слово"), "'слово");
        assert_eq!(plain_text("&#39;s Grüße"), "'s Grüße");
        assert_eq!(plain_text("&ééééééé;"), "&ééééééé;");
    }

    #[test]
    fn only_the_typed_text_is_sent() {
        let r = rig(FakeFetch::new(Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())));
        r.p.query(&q("tr hallo welt"));
        r.wait();
        assert_eq!(r.fetch.urls(), vec![TR_HALLO_WELT.to_string()]);

        let r = rig(FakeFetch::new(Ok(WIKTIONARY_HAUS.as_bytes().to_vec())));
        r.p.query(&q("def Haus"));
        r.wait();
        assert_eq!(r.fetch.urls(), vec![DEF_HAUS.to_string()]);
    }

    #[test]
    fn typing_fast_sends_one_request_for_the_last_text() {
        let r = rig_with(FakeFetch::new(Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())), Duration::from_millis(200));
        for t in ["tr h", "tr ha", "tr hal", "tr hallo"] {
            assert_eq!(titles(&r.p.query(&q(t))), ["Translating…"]);
            std::thread::sleep(Duration::from_millis(30));
        }
        r.wait();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            r.fetch.urls(),
            vec!["https://api.mymemory.translated.net/get?q=hallo&langpair=de%7Cen".to_string()]
        );
    }

    #[test]
    fn waiting_shows_translating_then_the_answer() {
        let r = rig(FakeFetch::offline().respond(TR_GUTEN_MORGEN, Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())));
        let rows = r.p.query(&q("tr guten Morgen"));
        assert_eq!(titles(&rows), ["Translating…"]);
        assert_eq!(rows[0].id, "inert:Translating…");
        assert_eq!(r.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Keep));
        r.wait();
        let rows = r.p.query(&q("tr guten Morgen"));
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(
            (row.title.as_str(), row.subtitle.as_str(), row.icon.as_str(), row.top, row.provider, row.pinnable),
            ("good morning", "German → English", "builtin:translate", true, "words", false)
        );
        assert_eq!(row.id, "copy:0");
    }

    #[test]
    fn english_goes_to_german() {
        let url = "https://api.mymemory.translated.net/get?q=good%20morning&langpair=en%7Cde";
        let body = r#"{"responseData":{"translatedText":"guten Morgen"},"responseStatus":200}"#;
        let r = rig(FakeFetch::offline().respond(url, Ok(body.as_bytes().to_vec())));
        r.p.query(&q("tr good morning"));
        r.wait();
        let rows = r.p.query(&q("tr good morning"));
        assert_eq!((rows[0].title.as_str(), rows[0].subtitle.as_str()), ("guten Morgen", "English → German"));
    }

    #[test]
    fn online_off_never_fetches_and_says_so() {
        let r = rig(FakeFetch::new(Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())));
        r.p.set_online(false);
        for t in ["tr hallo", "def Haus"] {
            let rows = r.p.query(&q(t));
            assert_eq!(titles(&rows), ["Online answers are off"], "{t}");
            assert_eq!(rows[0].id, "online-off");
            let before = r.opened.load(Ordering::SeqCst);
            assert_eq!(r.p.activate("online-off", Modifier::None), Ok(Outcome::Hide));
            assert_eq!(r.opened.load(Ordering::SeqCst), before + 1, "opener once");
        }
        std::thread::sleep(P * 3);
        assert!(r.fetch.urls().is_empty());
    }

    #[test]
    fn turning_online_off_drops_the_waiting_text() {
        let r = rig(FakeFetch::new(Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())));
        r.p.query(&q("tr hallo"));
        r.p.set_online(false);
        std::thread::sleep(P * 3);
        assert!(r.fetch.urls().is_empty());
    }

    #[test]
    fn hiding_drops_the_waiting_text() {
        // Each store on its own: the text waiting last is the one dropped.
        for texts in [["def Haus", "tr hallo"], ["tr hallo", "def Haus"]] {
            let r = rig(FakeFetch::new(Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())));
            for t in texts {
                r.p.query(&q(t));
            }
            r.p.forget_waiting();
            std::thread::sleep(P * 3);
            assert!(r.fetch.urls().is_empty(), "{texts:?}");
        }
    }

    #[test]
    fn clearing_the_bar_drops_the_waiting_text() {
        let r = rig(FakeFetch::new(Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())));
        r.p.query(&q("tr hallo"));
        r.p.skipped();
        assert!(r.p.empty_query().is_empty());
        std::thread::sleep(P * 3);
        assert!(r.fetch.urls().is_empty());
    }

    #[test]
    fn leaving_the_command_drops_the_waiting_text() {
        let r = rig(FakeFetch::new(Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())));
        // Plain text after it: the engine asks this provider, which answers nothing.
        r.p.query(&q("tr hallo"));
        r.p.skipped();
        assert!(r.p.query(&q("notepad")).is_empty());
        // Another route after it: the engine only calls `skipped`.
        r.p.query(&q("def Haus"));
        r.p.skipped();
        r.p.skipped();
        std::thread::sleep(P * 3);
        assert!(r.fetch.urls().is_empty());
        // Its own claimed query is followed by `skipped` too: that one keeps waiting.
        r.p.query(&q("tr hallo"));
        r.p.skipped();
        r.wait();
        assert_eq!(r.fetch.urls().len(), 1);
    }

    #[test]
    fn an_older_query_arriving_late_keeps_the_newer_waiting_text() {
        let r = rig(FakeFetch::new(Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())));
        // Query 2 (`tr hallo welt`) reaches the provider first ...
        crate::engine::as_query(2, || r.p.query(&q("tr hallo welt")));
        // ... then query 1, older, arrives: plain text, a skip, a cleared bar or
        // older words text. None of it may forget or replace query 2's text.
        crate::engine::as_query(1, || {
            assert!(r.p.query(&q("notepad")).is_empty());
            r.p.skipped();
            assert!(r.p.empty_query().is_empty());
            assert!(r.p.query(&q("tr hal")).is_empty());
        });
        // Query 2's own `skipped` keeps its text waiting.
        crate::engine::as_query(2, || r.p.skipped());
        r.wait();
        assert_eq!(r.fetch.urls(), vec![TR_HALLO_WELT.to_string()]);
    }

    #[test]
    fn a_newer_query_elsewhere_still_forgets_the_waiting_text() {
        let r = rig(FakeFetch::new(Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())));
        crate::engine::as_query(5, || {
            r.p.query(&q("tr hallo welt"));
            r.p.skipped();
        });
        // Query 6 went to another route: only `skipped` reaches this provider.
        crate::engine::as_query(6, || r.p.skipped());
        // Query 5's skip arriving late changes nothing either.
        crate::engine::as_query(5, || r.p.skipped());
        std::thread::sleep(P * 3);
        assert!(r.fetch.urls().is_empty());
    }

    #[test]
    fn a_skip_clear_hide_or_online_off_waits_for_a_query_already_past_its_check() {
        type Step = fn(&WordsProvider);
        let steps: [(&str, Step); 4] = [
            ("skipped", |p| p.skipped()),
            ("empty_query", |p| {
                p.empty_query();
            }),
            ("hide", |p| p.hide()),
            ("online off", |p| p.set_online(false)),
        ];
        for (what, step) in steps {
            let r = rig(FakeFetch::new(Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())));
            let p = &r.p;
            let (started_tx, started) = mpsc::channel();
            let (done_tx, done) = mpsc::channel();
            std::thread::scope(|s| {
                // Query 5 holds `copies` and has passed its checks ...
                let mut copies = p.copies.lock().unwrap();
                assert!(crate::engine::as_query(5, || p.is_newest()));
                // ... when query 6 (a clear, text sent elsewhere, a hide, online off) arrives.
                let newer = s.spawn(move || {
                    started_tx.send(()).unwrap();
                    crate::engine::as_query(6, || step(p));
                    done_tx.send(()).unwrap();
                });
                started.recv_timeout(WAIT).expect("the step started");
                // It must wait for query 5; without the lock it is done long before this.
                assert!(done.recv_timeout(Duration::from_millis(200)).is_err(), "{what}: did not wait for the query");
                // Query 5 goes on to ask; then the step must still forget it.
                crate::engine::as_query(5, || p.answer("tr hallo", &mut copies));
                drop(copies);
                newer.join().unwrap();
            });
            std::thread::sleep(P * 3);
            assert!(r.fetch.urls().is_empty(), "{what}: the older text was sent");
        }
    }

    #[test]
    fn service_down_says_so() {
        let r = rig(FakeFetch::offline());
        r.p.query(&q("tr hallo"));
        r.wait();
        assert_eq!(titles(&r.p.query(&q("tr hallo"))), ["Can't reach the translation service"]);
        assert_eq!(titles(&r.p.query(&q("tr hallo"))), ["Translating…"], "asking again fetches again");
        r.wait();
        assert_eq!(r.fetch.urls().len(), 2);

        let r = rig(FakeFetch::new(Ok(MYMEMORY_ERROR.as_bytes().to_vec())));
        r.p.query(&q("tr hallo"));
        r.wait();
        assert_eq!(titles(&r.p.query(&q("tr hallo"))), ["Can't reach the translation service"]);

        let r = rig(FakeFetch::offline());
        r.p.query(&q("def Haus"));
        r.wait();
        assert_eq!(titles(&r.p.query(&q("def Haus"))), ["Can't reach Wiktionary"]);
        let r = rig(FakeFetch::new(Err(FetchError::Status(500))));
        r.p.query(&q("def Haus"));
        r.wait();
        assert_eq!(titles(&r.p.query(&q("def Haus"))), ["Can't reach Wiktionary"]);
    }

    #[test]
    fn too_long_is_refused_without_sending() {
        let r = rig(FakeFetch::new(Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())));
        let long = format!("tr {}", "ä".repeat(201));
        let rows = r.p.query(&q(&long));
        assert_eq!(titles(&rows), ["Too long to translate — 200 characters at most"]);
        assert_eq!(r.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Keep));
        std::thread::sleep(P * 3);
        assert!(r.fetch.urls().is_empty());
        let ok = format!("tr {}", "ä".repeat(200));
        assert_eq!(titles(&r.p.query(&q(&ok))), ["Translating…"]);
    }

    #[test]
    fn enter_copies_the_translation() {
        let r = rig(FakeFetch::new(Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())));
        r.p.query(&q("tr guten Morgen"));
        r.wait();
        let rows = r.p.query(&q("tr guten Morgen"));
        assert_eq!(r.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(*r.clipboard.0.lock().unwrap(), vec!["good morning".to_string()]);
        assert!(r.p.activate("copy:7", Modifier::None).is_err());
    }

    #[test]
    fn definitions_german_first_three_each() {
        let senses = parse_definitions(WIKTIONARY_HAUS.as_bytes()).unwrap();
        assert_eq!(senses.len(), 5);
        assert_eq!(senses[0], Sense { lang: Lang::German, part: "noun".into(), text: "house".into() });
        assert_eq!(senses[1].text, "home, household");
        assert_eq!(senses[2], Sense { lang: Lang::English, part: "noun".into(), text: "A structure built & serving as a dwelling.".into() });
        assert_eq!(senses[3].text, "A family line.");
        assert_eq!(senses[4], Sense { lang: Lang::English, part: "verb".into(), text: "To keep within a structure.".into() });

        let r = rig(FakeFetch::new(Ok(WIKTIONARY_HAUS.as_bytes().to_vec())));
        assert_eq!(titles(&r.p.query(&q("def Haus"))), ["Looking up…"]);
        r.wait();
        let rows = r.p.query(&q("def Haus"));
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].subtitle, "German · noun");
        assert_eq!(rows[2].subtitle, "English · noun");
        assert_eq!(rows[4].subtitle, "English · verb");
        assert!(rows.iter().all(|r| r.icon == "builtin:define" && !r.title.contains('<')));
        assert!(rows.windows(2).all(|w| w[0].score > w[1].score), "kept in order");
        assert_eq!(r.p.activate(&rows[1].id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(*r.clipboard.0.lock().unwrap(), vec!["home, household".to_string()]);
    }

    #[test]
    fn def_retries_once_with_other_capitalisation() {
        let r = rig(
            FakeFetch::offline()
                .respond(DEF_HAUS_LOWER, Err(FetchError::Status(404)))
                .respond(DEF_HAUS, Ok(WIKTIONARY_HAUS.as_bytes().to_vec())),
        );
        r.p.query(&q("def haus"));
        r.wait();
        assert_eq!(r.fetch.urls(), vec![DEF_HAUS_LOWER.to_string(), DEF_HAUS.to_string()]);
        assert_eq!(r.p.query(&q("def haus")).len(), 5);
    }

    /// `rest_v1/page/definition/gift`: English only (plus a Danish entry never shown).
    const WIKTIONARY_GIFT: &str = r#"{"en":[{"partOfSpeech":"Noun","language":"English","definitions":[{"definition":"Something given to another <a rel=\"mw:WikiLink\" href=\"/wiki/voluntarily\" title=\"voluntarily\">voluntarily</a>, without charge.","examples":["She gave me a lovely <b>gift</b>."]},{"definition":"A talent or natural ability."}]},{"partOfSpeech":"Verb","language":"English","definitions":[{"definition":"To give as a gift."}]}],"da":[{"partOfSpeech":"Noun","language":"Danish","definitions":[{"definition":"poison"}]}]}"#;
    /// `rest_v1/page/definition/Gift`: German only.
    const WIKTIONARY_GIFT_UPPER: &str = r#"{"de":[{"partOfSpeech":"Noun","language":"German","definitions":[{"definition":"<a rel=\"mw:WikiLink\" href=\"/wiki/poison\" title=\"poison\">poison</a>, <a rel=\"mw:WikiLink\" href=\"/wiki/toxin\" title=\"toxin\">toxin</a>"},{"definition":"<span class=\"gloss-brac\">(</span>figuratively<span class=\"gloss-brac\">)</span> venom, spite"}]}]}"#;
    const DEF_GIFT: &str = "https://en.wiktionary.org/api/rest_v1/page/definition/gift";
    const DEF_GIFT_UPPER: &str = "https://en.wiktionary.org/api/rest_v1/page/definition/Gift";

    #[test]
    fn no_second_def_request_once_the_text_moved_on() {
        // The first request is in flight (100 ms to 400 ms) when something else happens at 200 ms.
        let moves: [(&str, fn(&WordsProvider)); 4] = [
            ("def gifts", |p| {
                p.query(&q("def gifts"));
            }),
            ("tr hallo", |p| {
                p.query(&q("tr hallo"));
            }),
            ("online off", |p| p.set_online(false)),
            ("bar hidden", |p| p.forget_waiting()),
        ];
        for (what, step) in moves {
            let r = rig(
                FakeFetch::offline()
                    .respond(DEF_GIFT, Ok(WIKTIONARY_GIFT.as_bytes().to_vec()))
                    .respond(DEF_GIFT_UPPER, Ok(WIKTIONARY_GIFT_UPPER.as_bytes().to_vec()))
                    .with_delay(Duration::from_millis(300)),
            );
            r.p.query(&q("def gift"));
            std::thread::sleep(Duration::from_millis(200));
            assert_eq!(r.fetch.urls(), [DEF_GIFT], "{what}: first in flight");
            step(&r.p);
            std::thread::sleep(Duration::from_millis(900));
            let urls = r.fetch.urls();
            assert_eq!(urls[0], DEF_GIFT, "{what}");
            assert!(!urls.iter().any(|u| u == DEF_GIFT_UPPER), "{what}: {urls:?}");
        }
    }

    #[test]
    fn a_word_in_both_languages_shows_both_german_first() {
        let fetch = || {
            FakeFetch::offline()
                .respond(DEF_GIFT, Ok(WIKTIONARY_GIFT.as_bytes().to_vec()))
                .respond(DEF_GIFT_UPPER, Ok(WIKTIONARY_GIFT_UPPER.as_bytes().to_vec()))
        };
        for (typed, urls) in [("gift", [DEF_GIFT, DEF_GIFT_UPPER]), ("Gift", [DEF_GIFT_UPPER, DEF_GIFT])] {
            let r = rig(fetch());
            r.p.query(&q(&format!("def {typed}")));
            r.wait();
            assert_eq!(r.fetch.urls(), urls.map(String::from).to_vec(), "{typed}");
            let rows = r.p.query(&q(&format!("def {typed}")));
            let subtitles: Vec<&str> = rows.iter().map(|r| r.subtitle.as_str()).collect();
            assert_eq!(
                subtitles,
                ["German · noun", "German · noun", "English · noun", "English · noun", "English · verb"],
                "{typed}"
            );
            assert_eq!(rows[0].title, "poison, toxin");
            assert_eq!(rows[2].title, "Something given to another voluntarily, without charge.");
            std::thread::sleep(P * 3);
            assert_eq!(r.fetch.urls().len(), 2, "both kept under the typed word");
        }
    }

    #[test]
    fn the_first_page_stands_when_the_other_case_has_nothing() {
        for second in [Err(FetchError::Status(404)), Err(FetchError::Offline), Ok(WIKTIONARY_404.as_bytes().to_vec())] {
            let r = rig(FakeFetch::offline().respond(DEF_GIFT, Ok(WIKTIONARY_GIFT.as_bytes().to_vec())).respond(DEF_GIFT_UPPER, second.clone()));
            r.p.query(&q("def gift"));
            r.wait();
            let rows = r.p.query(&q("def gift"));
            assert_eq!(rows.len(), 3, "{second:?}");
            assert!(rows.iter().all(|r| r.subtitle.starts_with("English")), "{second:?}");
        }
        // A word without case is asked once.
        let r = rig(FakeFetch::new(Ok(WIKTIONARY_GIFT.as_bytes().to_vec())));
        r.p.query(&q("def 42"));
        r.wait();
        assert_eq!(r.fetch.urls().len(), 1);
    }

    #[test]
    fn a_partial_answer_is_shown_once_and_asked_again() {
        // The other case's page failed: the English senses show, but are not kept.
        let r = rig(
            FakeFetch::offline()
                .respond(DEF_GIFT, Ok(WIKTIONARY_GIFT.as_bytes().to_vec()))
                .respond(DEF_GIFT_UPPER, Err(FetchError::Status(500))),
        );
        r.p.query(&q("def gift"));
        r.wait();
        assert_eq!(r.p.query(&q("def gift")).len(), 3);
        assert_eq!(titles(&r.p.query(&q("def gift"))), ["Looking up…"], "asked again");
        r.wait();
        assert_eq!(r.fetch.urls(), [DEF_GIFT, DEF_GIFT_UPPER, DEF_GIFT, DEF_GIFT_UPPER]);

        // The other case's page was skipped (the text moved on meanwhile).
        let r = rig(
            FakeFetch::offline()
                .respond(DEF_GIFT, Ok(WIKTIONARY_GIFT.as_bytes().to_vec()))
                .respond(DEF_GIFT_UPPER, Ok(WIKTIONARY_GIFT_UPPER.as_bytes().to_vec()))
                .with_delay(Duration::from_millis(300)),
        );
        r.p.query(&q("def gift"));
        std::thread::sleep(Duration::from_millis(200));
        r.p.forget_waiting();
        r.wait();
        assert_eq!(r.fetch.urls(), [DEF_GIFT]);
        assert_eq!(r.p.query(&q("def gift")).len(), 3, "the first page, told once");
        assert_eq!(titles(&r.p.query(&q("def gift"))), ["Looking up…"]);
        r.wait();
        assert_eq!(r.p.query(&q("def gift")).len(), 5, "both pages, now kept");
        assert_eq!(r.p.query(&q("def gift")).len(), 5);
        assert_eq!(r.fetch.urls(), [DEF_GIFT, DEF_GIFT, DEF_GIFT_UPPER]);
    }

    #[test]
    fn unknown_word_says_no_definition_found() {
        let r = rig(FakeFetch::new(Err(FetchError::Status(404))).respond(DEF_HAUS_LOWER, Err(FetchError::Status(404))));
        r.p.query(&q("def haus"));
        r.wait();
        assert_eq!(titles(&r.p.query(&q("def haus"))), ["No definition found"]);
        assert_eq!(r.fetch.urls().len(), 2, "asked once more, then no more");
        assert_eq!(titles(&r.p.query(&q("def haus"))), ["No definition found"], "kept");
        std::thread::sleep(P * 3);
        assert_eq!(r.fetch.urls().len(), 2);
        assert_eq!(parse_definitions(WIKTIONARY_404.as_bytes()), Err(LookupError::NotFound));
        assert_eq!(parse_definitions(b"not json"), Err(LookupError::Bad));
    }

    #[test]
    fn bare_tr_and_def_are_not_claimed() {
        let r = rig(FakeFetch::offline());
        for t in ["tr", "def", "tr   ", "trello", "define x", "", "t r x"] {
            assert!(!r.p.claims(t), "{t}");
            assert!(r.p.query(&q(t)).is_empty(), "{t}");
        }
        for t in ["TR hi", "Def x", "tr hi", "def x"] {
            assert!(r.p.claims(t), "{t}");
        }
        assert_eq!(r.p.name(), "words");
        assert!(r.p.item("copy:0").is_none());
        let help = r.p.help();
        assert_eq!(help.len(), 2);
        assert_eq!(
            (help[0].example.as_str(), help[0].about.as_str(), help[0].fill.as_str()),
            ("tr guten Morgen", "translate between German and English (online)", "tr")
        );
        assert_eq!(
            (help[1].example.as_str(), help[1].about.as_str(), help[1].fill.as_str()),
            ("def serendipity", "definitions from Wiktionary (online)", "def")
        );
    }

    #[test]
    fn rows_are_never_learned() {
        let r = rig(FakeFetch::new(Ok(MYMEMORY_GUTEN_MORGEN.as_bytes().to_vec())));
        r.p.query(&q("tr guten Morgen"));
        r.wait();
        let mut ids: Vec<String> = r.p.query(&q("tr guten Morgen")).into_iter().map(|r| r.id).collect();
        ids.extend(["inert:Translating…".to_string(), "online-off".to_string(), "copy:0".to_string()]);
        for id in ids {
            assert_eq!(r.p.rank_key(&id), None, "{id}");
        }
    }
}
