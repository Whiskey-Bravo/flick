//! Fuzzy scoring on normalised (lowercase, diacritic-free) strings.

const SEPARATORS: &[char] = &['-', '_', '.', ',', '(', ')', '&', '/', '+', ':'];

/// Lowercase, strip common Latin diacritics and combining marks, map `ß` to `ss`.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            '\u{300}'..='\u{36f}' => {}
            'ß' => out.push_str("ss"),
            other => out.push(strip_diacritic(other)),
        }
    }
    out
}

fn strip_diacritic(c: char) -> char {
    match c {
        'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' | 'đ' => 'd',
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' | 'ħ' => 'h',
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' => 'i',
        'ĵ' => 'j',
        'ķ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'ţ' | 'ť' | 'ŧ' => 't',
        'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ŵ' => 'w',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

/// Whitespace or one of `SEPARATORS`: what splits words.
pub fn is_word_break(c: char) -> bool {
    c.is_whitespace() || SEPARATORS.contains(&c)
}

/// Split on whitespace and `SEPARATORS`; empty pieces dropped.
pub fn words(s: &str) -> Vec<&str> {
    words_iter(s).collect()
}

/// [`words`] without the `Vec`: the scoring below runs per file name, so it
/// does not allocate.
pub fn words_iter(s: &str) -> impl Iterator<Item = &str> + Clone {
    s.split(is_word_break as fn(char) -> bool).filter(|w| !w.is_empty())
}

/// The chars of `s` without whitespace and `SEPARATORS`: "7-Zip File" -> "7zipfile".
pub(crate) fn squash(s: &str) -> impl Iterator<Item = char> + Clone + '_ {
    s.chars().filter(|&c| !is_word_break(c))
}

/// `s` yields all of `prefix`'s chars first.
fn chars_start_with(mut s: impl Iterator<Item = char>, mut prefix: impl Iterator<Item = char>) -> bool {
    prefix.all(|c| s.next() == Some(c))
}

/// For some k >= 1, `needle` (squashed) is the initials of the first k words
/// followed by a non-empty prefix of word k+1: "vscode" and "vstud" for
/// "Visual Studio Code".
fn initials_then_word_prefix<'a>(
    mut rest: impl Iterator<Item = char> + Clone,
    mut ws: impl Iterator<Item = &'a str>,
) -> bool {
    let Some(mut prev) = ws.next() else { return false };
    for w in ws {
        let Some(initial) = prev.chars().next() else { return false };
        if rest.next() != Some(initial) {
            return false;
        }
        if rest.clone().next().is_some() && chars_start_with(w.chars(), rest.clone()) {
            return true;
        }
        prev = w;
    }
    false
}

/// The first chars of `ws` start with all of `needle`'s chars.
fn initials_start_with<'a>(needle: &str, ws: impl Iterator<Item = &'a str>) -> bool {
    let mut n = needle.chars();
    for w in ws {
        match n.next() {
            None => return true,
            Some(c) if w.chars().next() != Some(c) => return false,
            Some(_) => {}
        }
    }
    n.next().is_none()
}

/// `needle` (`needle_len` chars, at least 1) is a subsequence of `word`
/// starting at `word`'s first char, with at most two extra characters inside
/// the matched span.
fn tight_fuzzy(needle: &str, needle_len: usize, word: &str) -> bool {
    let mut n = needle.chars();
    let mut w = word.chars();
    if w.next() != n.next() {
        return false;
    }
    let mut want = n.next();
    let mut last = 0;
    for (i, c) in w.enumerate() {
        let Some(wanted) = want else { break };
        if c == wanted {
            last = i + 1;
            want = n.next();
        }
    }
    want.is_none() && (last + 1) - needle_len <= 2
}

fn score_single(needle: &str, haystack: &str) -> Option<u32> {
    if needle == haystack {
        return Some(1000);
    }
    if haystack.starts_with(needle) {
        return Some(900);
    }
    let squashed = squash(needle);
    if squashed.clone().next().is_some() && chars_start_with(squash(haystack), squashed.clone()) {
        return Some(850);
    }
    let ws = words_iter(haystack);
    if ws.clone().any(|w| w.starts_with(needle)) {
        return Some(800);
    }
    let n_len = needle.chars().count();
    if n_len >= 2 && initials_start_with(needle, ws.clone()) {
        return Some(750);
    }
    if initials_then_word_prefix(squashed, ws.clone()) {
        return Some(700);
    }
    if haystack.contains(needle) {
        return Some(650);
    }
    if n_len >= 3 && ws.clone().any(|w| tight_fuzzy(needle, n_len, w)) {
        return Some(500);
    }
    None
}

/// Score `needle` against `haystack` in explicit tiers: exact 1000, prefix 900,
/// squashed prefix (separators and spaces ignored) 850, word prefix 800, acronym
/// 750, leading initials plus word prefix 700, substring 650, tight fuzzy 500;
/// `None` = no match.
pub fn score(needle: &str, haystack: &str) -> Option<u32> {
    score_prenormalized(&normalize(needle), &normalize(haystack))
}

/// [`score`] for strings already passed through [`normalize`]; allocates nothing.
pub fn score_prenormalized(needle_normalized: &str, haystack_normalized: &str) -> Option<u32> {
    let (needle, haystack) = (needle_normalized, haystack_normalized);
    if needle.is_empty() {
        return None;
    }
    let single = score_single(needle, haystack);
    if needle.contains(' ') {
        let mut parts = needle.split_whitespace().peekable();
        if parts.peek().is_some() && parts.all(|p| words_iter(haystack).any(|w| w.starts_with(p))) {
            return Some(single.unwrap_or(0).max(800));
        }
    }
    single
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_is_1000() {
        assert_eq!(score("blender", "Blender"), Some(1000));
    }
    #[test]
    fn prefix_beats_midword() {
        assert!(score("bl", "Blender").unwrap() > score("bl", "Tablet").unwrap());
    }
    #[test]
    fn no_match_is_none() {
        assert_eq!(score("xyz", "Blender"), None);
    }
    #[test]
    fn diacritics_and_case() {
        assert!(score("uber", "Über Tool").is_some());
        assert!(score("UBER", "Über Tool").is_some());
        assert!(score("e", "é").is_some());
    }

    #[test]
    fn scattered_subsequence_is_rejected() {
        assert_eq!(score("calc", "Create local library areas"), None);
        assert_eq!(score("bl", "Befehlspalette"), None);
        assert_eq!(score("bl", "Chromium Web Browser (Debian)"), None);
        assert_eq!(score("bl", "DB Browser for SQLite (Debian)"), None);
    }
    #[test]
    fn tiers_are_ordered() {
        let exact = score("blender", "Blender").unwrap();
        let prefix = score("bl", "Blender 4.5").unwrap();
        let word_prefix = score("stud", "Visual Studio Code").unwrap();
        let acronym = score("vsc", "Visual Studio Code").unwrap();
        let substring = score("bl", "Roblox").unwrap();
        let typo = score("blnder", "Blender").unwrap();
        assert!(exact > prefix && prefix > word_prefix && word_prefix > acronym && acronym > substring && substring > typo);
    }
    #[test]
    fn word_prefix_after_separator() {
        assert_eq!(score("debian", "Chromium Web Browser (Debian)"), Some(800));
    }
    #[test]
    fn multi_word_needle_matches_word_prefixes_in_any_order() {
        assert_eq!(score("code vis", "Visual Studio Code"), Some(800));
        assert_eq!(score("vis xyz", "Visual Studio Code"), None);
    }
    #[test]
    fn short_needles_never_fuzzy() {
        assert_eq!(score("bx", "Blender"), None);
    }
    #[test]
    fn empty_needle_is_none() {
        assert_eq!(score("", "Blender"), None);
    }
    #[test]
    fn eszett_and_combining_marks() {
        assert!(score("strasse", "Straße").is_some());
        assert_eq!(normalize("e\u{301}"), normalize("é"));
        assert_eq!(normalize("é"), "e");
    }

    #[test]
    fn squashed_prefix_ignores_separators_and_spaces() {
        assert_eq!(score("7zip", "7-Zip File Manager"), Some(850));
        assert_eq!(score("wifi", "Wi-Fi"), Some(850));
        assert_eq!(score("wifi", "Wi-Fi Analyzer"), Some(850));
        assert_eq!(score("7 zip", "7-Zip File Manager"), Some(850));
    }
    #[test]
    fn leading_initials_plus_word_prefix() {
        assert_eq!(score("vscode", "Visual Studio Code"), Some(700));
        assert_eq!(score("vs code", "Visual Studio Code"), Some(700));
        assert_eq!(score("vstud", "Visual Studio Code"), Some(700));
        // initials must be the leading words, in order
        assert_eq!(score("vcode", "Visual Studio Code"), None);
        assert_eq!(score("scode", "Visual Studio Code"), None);
    }
    #[test]
    fn score_prenormalized_matches_score() {
        let table: [(&str, &str, Option<u32>); 12] = [
            ("blender", "Blender", Some(1000)),
            ("bl", "Blender 4.5", Some(900)),
            ("7zip", "7-Zip File Manager", Some(850)),
            ("debian", "Chromium Web Browser (Debian)", Some(800)),
            ("vsc", "Visual Studio Code", Some(750)),
            ("vscode", "Visual Studio Code", Some(700)),
            ("bl", "Roblox", Some(650)),
            ("blnder", "Blender", Some(500)),
            ("code vis", "Visual Studio Code", Some(800)),
            ("calc", "Create local library areas", None),
            ("UBER", "Über Tool", Some(900)),
            ("strasse", "Straße", Some(1000)),
        ];
        for (needle, haystack, expected) in table {
            assert_eq!(score(needle, haystack), expected, "score({needle:?}, {haystack:?})");
            assert_eq!(
                score_prenormalized(&normalize(needle), &normalize(haystack)),
                expected,
                "score_prenormalized({needle:?}, {haystack:?})"
            );
        }
        assert_eq!(score_prenormalized("", "blender"), None);
    }

    #[test]
    fn new_tiers_sit_between_the_old_ones() {
        let prefix = score("bl", "Blender 4.5").unwrap();
        let squashed = score("7zip", "7-Zip File Manager").unwrap();
        let word_prefix = score("stud", "Visual Studio Code").unwrap();
        let acronym = score("vsc", "Visual Studio Code").unwrap();
        let initials_word = score("vscode", "Visual Studio Code").unwrap();
        let substring = score("bl", "Roblox").unwrap();
        assert!(prefix > squashed && squashed > word_prefix);
        assert!(acronym > initials_word && initials_word > substring);
    }
}
