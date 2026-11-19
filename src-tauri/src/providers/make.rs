//! Small things made on the spot: `password` (a random one), `uuid`, `lorem`
//! (filler text) and `hash` (SHA-256 of the clipboard text). Enter copies the
//! result. Everything is made on this PC; nothing is stored, counted or
//! logged, and the clipboard is read (for `hash` only) on a [`Loader`] thread.
//!
//! These rows sit on top of the other results for exactly these words; the
//! words are not taken away from the rest of the search (`password` still
//! finds apps and settings below).

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::clipboard::{Clipboard, ClipboardText};
use crate::dirs::{one_line, Load, Loader, ReadError, NEVER_STALE, SLOW};
use crate::engine::NewestQuery;
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem};

const NAME: &str = "make";
const TOP_SCORE: u32 = 1000;
const COPY: &str = "copy:";
const ICON: &str = "builtin:calc";

/// A password's length when none is typed, and the range a typed one may have.
pub const PASSWORD_DEFAULT: usize = 20;
pub const PASSWORD_MIN: usize = 8;
pub const PASSWORD_MAX: usize = 64;
/// The most paragraphs `lorem` gives.
pub const LOREM_MAX: usize = 10;
/// Characters of a result shown as the row title.
const PREVIEW: usize = 80;

const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
const DIGITS: &[u8] = b"23456789";
const SYMBOLS: &[u8] = b"!#$%&*+-=?@^_";

const LOREM: [&str; 5] = [
    "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.",
    "Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.",
    "Sed ut perspiciatis unde omnis iste natus error sit voluptatem accusantium doloremque laudantium, totam rem aperiam, eaque ipsa quae ab illo inventore veritatis et quasi architecto beatae vitae dicta sunt explicabo.",
    "Nemo enim ipsam voluptatem quia voluptas sit aspernatur aut odit aut fugit, sed quia consequuntur magni dolores eos qui ratione voluptatem sequi nesciunt.",
    "Neque porro quisquam est, qui dolorem ipsum quia dolor sit amet, consectetur, adipisci velit, sed quia non numquam eius modi tempora incidunt ut labore et dolore magnam aliquam quaerat voluptatem.",
];

/// Where random bytes come from (the operating system; a fixed sequence in tests).
pub trait Random: Send + Sync {
    /// Fills `buf`; false when no randomness is to be had.
    fn fill(&self, buf: &mut [u8]) -> bool;
}

/// The operating system's cryptographic random numbers.
pub struct OsRandom;

impl Random for OsRandom {
    fn fill(&self, buf: &mut [u8]) -> bool {
        getrandom::fill(buf).is_ok()
    }
}

/// What a typed text asks this provider for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    Password(usize),
    Uuid,
    Lorem(usize),
    Hash,
}

/// The request in `text`: one of the four words alone, or `password` / `lorem`
/// with a number. Anything else (more words, a number out of range) is not
/// for this provider.
pub fn parse(text: &str) -> Option<Ask> {
    let mut words = text.split_whitespace();
    let word = words.next()?.to_ascii_lowercase();
    let number = words.next();
    if words.next().is_some() {
        return None;
    }
    let count = |default: usize, range: std::ops::RangeInclusive<usize>| match number {
        None => Some(default),
        Some(n) => n.parse::<usize>().ok().filter(|n| range.contains(n)),
    };
    match word.as_str() {
        "password" => count(PASSWORD_DEFAULT, PASSWORD_MIN..=PASSWORD_MAX).map(Ask::Password),
        "lorem" => count(1, 1..=LOREM_MAX).map(Ask::Lorem),
        "uuid" if number.is_none() => Some(Ask::Uuid),
        "hash" if number.is_none() => Some(Ask::Hash),
        _ => None,
    }
}

/// One byte below `n` (at most 256), each value equally likely: bytes that
/// would favour the low values are thrown away.
fn below(n: usize, random: &dyn Random) -> Option<usize> {
    let limit = 256 - 256 % n;
    loop {
        let mut b = [0u8];
        if !random.fill(&mut b) {
            return None;
        }
        if usize::from(b[0]) < limit {
            return Some(usize::from(b[0]) % n);
        }
    }
}

/// A random password of `len` characters with at least one lower-case letter,
/// capital, digit and symbol (look-alikes such as `l`, `I`, `O`, `0` and `1`
/// are left out). `None` when no randomness is to be had.
pub fn password(len: usize, random: &dyn Random) -> Option<String> {
    let all: Vec<u8> = [LOWER, UPPER, DIGITS, SYMBOLS].concat();
    // Bounded: a source that never gives a full mix (only in tests) ends it.
    for _ in 0..1000 {
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push(all[below(all.len(), random)?]);
        }
        let has = |set: &[u8]| out.iter().any(|c| set.contains(c));
        if len < 4 || (has(LOWER) && has(UPPER) && has(DIGITS) && has(SYMBOLS)) {
            return String::from_utf8(out).ok();
        }
    }
    None
}

/// A random (version 4) UUID, lower case with hyphens.
pub fn uuid(random: &dyn Random) -> Option<String> {
    let mut b = [0u8; 16];
    if !random.fill(&mut b) {
        return None;
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Some(format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]))
}

/// `paragraphs` paragraphs of filler text, separated by blank lines.
pub fn lorem(paragraphs: usize) -> String {
    (0..paragraphs.clamp(1, LOREM_MAX)).map(|i| LOREM[i % LOREM.len()]).collect::<Vec<_>>().join("\r\n\r\n")
}

/// The SHA-256 of `text` (its UTF-8 bytes), as lower-case hex.
pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

pub struct MakeProvider {
    loader: Arc<Loader<Option<String>>>,
    source: Arc<dyn ClipboardText>,
    clipboard: Arc<dyn Clipboard>,
    random: Arc<dyn Random>,
    /// What the row of the newest query copies. Memory only.
    made: NewestQuery<Option<String>>,
}

impl MakeProvider {
    pub fn new(
        source: Arc<dyn ClipboardText>,
        clipboard: Arc<dyn Clipboard>,
        random: Arc<dyn Random>,
        on_ready: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self { loader: Loader::new(on_ready, NEVER_STALE, SLOW), source, clipboard, random, made: NewestQuery::new(None) }
    }

    /// Forgets what was made and the clipboard text read (the bar hid).
    pub fn forget(&self) {
        self.loader.clear();
        self.made.reset(None);
    }

    fn row(&self, value: String, title: String, subtitle: &str) -> Vec<ResultItem> {
        self.made.set(Some(value));
        vec![ResultItem {
            id: format!("{COPY}0"),
            provider: NAME,
            title,
            subtitle: subtitle.to_string(),
            score: TOP_SCORE,
            icon: ICON.into(),
            top: true,
            ..Default::default()
        }]
    }

    fn hash(&self) -> Vec<ResultItem> {
        let source = Arc::clone(&self.source);
        match self.loader.get("clipboard", move || source.text().map_err(|_| ReadError::Denied)) {
            Load::Pending => vec![inert("Reading clipboard\u{2026}")],
            Load::Ready(text) => match text.as_deref() {
                Some(t) if !t.is_empty() => {
                    let hex = sha256_hex(t);
                    let about = format!("SHA-256 of the clipboard text ({} characters) \u{2014} Enter copies it", t.chars().count());
                    self.row(hex.clone(), hex, &about)
                }
                _ => vec![inert("Nothing to hash \u{2014} copy some text first")],
            },
            Load::NotFound | Load::Denied => {
                self.loader.clear();
                vec![inert("Can't read the clipboard")]
            }
            Load::Slow => vec![inert("Can't read the clipboard")],
        }
    }
}

fn inert(title: &str) -> ResultItem {
    ResultItem {
        id: format!("inert:{title}"),
        provider: NAME,
        title: title.to_string(),
        score: TOP_SCORE,
        icon: ICON.into(),
        top: true,
        ..Default::default()
    }
}

impl Provider for MakeProvider {
    fn name(&self) -> &'static str {
        NAME
    }

    /// Memory only; the clipboard (for `hash`) is read on a loader thread.
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let Some(ask) = parse(&q.text) else {
            self.made.set(None);
            return vec![];
        };
        let no_random = || vec![inert("Windows gave no random numbers")];
        match ask {
            Ask::Password(len) => match password(len, self.random.as_ref()) {
                Some(p) => {
                    let about = format!("Random password, {len} characters \u{2014} Enter copies it");
                    self.row(p.clone(), p, &about)
                }
                None => no_random(),
            },
            Ask::Uuid => match uuid(self.random.as_ref()) {
                Some(u) => self.row(u.clone(), u, "Random UUID \u{2014} Enter copies it"),
                None => no_random(),
            },
            Ask::Lorem(n) => {
                let text = lorem(n);
                let about = if n == 1 {
                    "Filler text, 1 paragraph \u{2014} Enter copies it".to_string()
                } else {
                    format!("Filler text, {n} paragraphs \u{2014} Enter copies it")
                };
                let title = one_line(&text, PREVIEW);
                self.row(text, title, &about)
            }
            Ask::Hash => self.hash(),
        }
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        if !id.starts_with(COPY) {
            return Ok(Outcome::Keep);
        }
        let made = self.made.with(Clone::clone);
        match made {
            Some(text) => {
                self.clipboard.set_text(&text)?;
                Ok(Outcome::Hide)
            }
            None => Ok(Outcome::Keep),
        }
    }

    /// Never counted or learned: a password must leave no trace.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![
            HelpLine {
                example: "password / password 32".into(),
                about: "make a random password and copy it".into(),
                fill: "password".into(),
            },
            HelpLine {
                example: "uuid / lorem / hash".into(),
                about: "a random UUID, filler text, or the SHA-256 of the clipboard text".into(),
                fill: "uuid".into(),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::{FakeClipboard, FakeText};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::Mutex;
    use std::time::Duration;

    /// Bytes 0, 1, 2, ... wrapping round: every value in turn.
    #[derive(Default)]
    struct Counting(AtomicUsize);

    impl Random for Counting {
        fn fill(&self, buf: &mut [u8]) -> bool {
            for b in buf {
                *b = self.0.fetch_add(1, Ordering::SeqCst) as u8;
            }
            true
        }
    }

    /// Varied bytes, the same every run (a small xorshift generator).
    struct Mixed(Mutex<u64>);

    impl Default for Mixed {
        fn default() -> Self {
            Mixed(Mutex::new(0x9E37_79B9_7F4A_7C15))
        }
    }

    impl Random for Mixed {
        fn fill(&self, buf: &mut [u8]) -> bool {
            let mut x = self.0.lock().unwrap();
            for b in buf {
                *x ^= *x << 13;
                *x ^= *x >> 7;
                *x ^= *x << 17;
                *b = (*x >> 24) as u8;
            }
            true
        }
    }

    struct Broken;

    impl Random for Broken {
        fn fill(&self, _buf: &mut [u8]) -> bool {
            false
        }
    }

    struct Rig {
        p: MakeProvider,
        rx: Receiver<()>,
        clip: Arc<FakeClipboard>,
    }

    impl Rig {
        /// Everything copied so far.
        fn copied(&self) -> Vec<String> {
            self.clip.0.lock().unwrap().clone()
        }
    }

    fn rig(clipboard_text: Option<&str>, random: Arc<dyn Random>) -> Rig {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let clip = Arc::new(FakeClipboard::default());
        let p = MakeProvider::new(
            Arc::new(FakeText::new(clipboard_text)),
            clip.clone(),
            random,
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        Rig { p, rx, clip }
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    #[test]
    fn only_the_four_words_alone_or_with_a_number_in_range_are_asked() {
        assert_eq!(parse("password"), Some(Ask::Password(20)));
        assert_eq!(parse(" PASSWORD  32 "), Some(Ask::Password(32)));
        assert_eq!(parse("password 8"), Some(Ask::Password(8)));
        assert_eq!(parse("password 64"), Some(Ask::Password(64)));
        assert_eq!(parse("uuid"), Some(Ask::Uuid));
        assert_eq!(parse("lorem"), Some(Ask::Lorem(1)));
        assert_eq!(parse("lorem 3"), Some(Ask::Lorem(3)));
        assert_eq!(parse("hash"), Some(Ask::Hash));
        for not in ["", "pass", "passwords", "password 7", "password 65", "password x", "password 20 3", "uuid 4", "hash it", "lorem 0", "lorem 11", "my password"] {
            assert_eq!(parse(not), None, "{not}");
        }
    }

    #[test]
    fn a_password_has_the_length_and_one_of_each_kind() {
        let random = Mixed::default();
        for len in [8, 20, 64] {
            let p = password(len, &random).unwrap();
            assert_eq!(p.chars().count(), len, "{p}");
            for set in [LOWER, UPPER, DIGITS, SYMBOLS] {
                assert!(p.bytes().any(|c| set.contains(&c)), "{p}");
            }
            assert!(p.bytes().all(|c| [LOWER, UPPER, DIGITS, SYMBOLS].concat().contains(&c)), "{p}");
            assert!(!p.contains(['l', 'I', 'O', '0', '1']), "{p}");
        }
        // Two in a row differ.
        assert_ne!(password(20, &random), password(20, &random));
        assert_eq!(password(20, &Broken), None);
    }

    #[test]
    fn every_character_is_equally_likely() {
        // 256 bytes in turn over an alphabet of 70: the 46 that would favour
        // the first values are thrown away, leaving three of each.
        let random = Counting::default();
        let mut seen = [0usize; 70];
        for _ in 0..210 {
            seen[below(70, &random).unwrap()] += 1;
        }
        assert!(seen.iter().all(|&n| n == 3), "{seen:?}");
        assert_eq!(below(70, &Broken), None);
    }

    #[test]
    fn a_uuid_is_version_four() {
        let u = uuid(&Counting::default()).unwrap();
        assert_eq!(u, "00010203-0405-4607-8809-0a0b0c0d0e0f");
        let parts: Vec<&str> = u.split('-').collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), [8, 4, 4, 4, 12]);
        assert!(parts[2].starts_with('4') && "89ab".contains(&parts[3][..1]));
        assert_eq!(uuid(&Broken), None);
    }

    #[test]
    fn lorem_gives_the_paragraphs_asked_for() {
        assert!(lorem(1).starts_with("Lorem ipsum dolor sit amet"));
        assert!(!lorem(1).contains('\n'));
        assert_eq!(lorem(3).matches("\r\n\r\n").count(), 2);
        assert_eq!(lorem(10).matches("\r\n\r\n").count(), 9);
        assert_eq!(lorem(99), lorem(10));
        assert_eq!(lorem(0), lorem(1));
    }

    #[test]
    fn sha256_matches_the_known_values() {
        assert_eq!(sha256_hex(""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn enter_copies_what_the_row_shows_and_nothing_else_is_answered() {
        let r = rig(None, Arc::new(Mixed::default()));
        assert!(r.p.query(&q("passwor")).is_empty());
        assert!(r.p.query(&q("password manager")).is_empty());
        let rows = r.p.query(&q("password 12"));
        assert_eq!(rows.len(), 1);
        assert!(rows[0].top && rows[0].title.chars().count() == 12, "{:?}", rows[0]);
        assert_eq!(rows[0].subtitle, "Random password, 12 characters \u{2014} Enter copies it");
        assert_eq!(r.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(r.copied(), [rows[0].title.clone()]);
        // Never counted: no key to learn it under.
        assert_eq!(r.p.rank_key(&rows[0].id), None);
        // The whole text of a long result is copied, not its one-line preview.
        let rows = r.p.query(&q("lorem 2"));
        assert!(!rows[0].title.contains('\n'));
        r.p.activate(&rows[0].id, Modifier::None).unwrap();
        assert_eq!(r.copied().last().unwrap(), &lorem(2));
    }

    #[test]
    fn an_older_query_finishing_late_never_changes_what_enter_copies() {
        use crate::engine::as_query;
        let r = rig(None, Arc::new(Mixed::default()));
        let shown = as_query(6, || r.p.query(&q("password 32")));
        // Query 5 (`password`) was typed first but runs after query 6.
        as_query(5, || r.p.query(&q("password")));
        as_query(5, || r.p.query(&q("passwor")));
        assert_eq!(r.p.activate(&shown[0].id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(r.copied(), [shown[0].title.clone()]);
        assert_eq!(shown[0].title.chars().count(), 32);
    }

    #[test]
    fn what_was_made_is_forgotten_when_the_bar_hides_or_the_text_moves_on() {
        let r = rig(None, Arc::new(Counting::default()));
        let rows = r.p.query(&q("uuid"));
        r.p.forget();
        assert_eq!(r.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Keep));
        let rows = r.p.query(&q("uuid"));
        r.p.query(&q("uuidx"));
        assert_eq!(r.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Keep));
        assert!(r.copied().is_empty());
    }

    #[test]
    fn hash_is_of_the_clipboard_text_once_it_is_read() {
        let r = rig(Some("abc"), Arc::new(Counting::default()));
        let first = r.p.query(&q("hash"));
        let rows = if first[0].title.starts_with("Reading") {
            r.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready within 1 s");
            r.p.query(&q("hash"))
        } else {
            first
        };
        assert_eq!(rows[0].title, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(rows[0].subtitle, "SHA-256 of the clipboard text (3 characters) \u{2014} Enter copies it");
        r.p.activate(&rows[0].id, Modifier::None).unwrap();
        assert_eq!(r.copied(), [rows[0].title.clone()]);
    }

    #[test]
    fn hash_with_nothing_copied_says_so() {
        let r = rig(None, Arc::new(Counting::default()));
        let first = r.p.query(&q("hash"));
        let rows = if first[0].title.starts_with("Reading") {
            r.rx.recv_timeout(Duration::from_secs(1)).expect("on_ready within 1 s");
            r.p.query(&q("hash"))
        } else {
            first
        };
        assert_eq!(rows[0].title, "Nothing to hash \u{2014} copy some text first");
        assert_eq!(r.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Keep));
    }

    #[test]
    fn no_randomness_is_said_not_faked() {
        let r = rig(None, Arc::new(Broken));
        for text in ["password", "uuid"] {
            let rows = r.p.query(&q(text));
            assert_eq!(rows[0].title, "Windows gave no random numbers", "{text}");
            assert_eq!(r.p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Keep));
        }
    }
}
