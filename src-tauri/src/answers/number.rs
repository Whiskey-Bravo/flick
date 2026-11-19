//! Number parsing and formatting in the Windows locale.

use jiff::civil::{Date, Time};
use windows::core::PCWSTR;
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::Globalization::{
    GetDateFormatEx, GetLocaleInfoEx, GetTimeFormatEx, DATE_LONGDATE, DATE_SHORTDATE,
    LOCALE_IMEASURE, LOCALE_SDECIMAL, LOCALE_SINTLSYMBOL, LOCALE_STHOUSAND, TIME_FORMAT_FLAGS,
};

/// Parses a typed number. `,` and `.` both work as the decimal separator.
/// `group` is the locale's grouping character. Only one kind of separator:
/// - when it is `group` and every group after the first has exactly three
///   digits (`1,500` in English, `1.000.000` in German), it groups;
/// - otherwise a single one is the decimal separator (`1,5`, `1.000` in English).
///
/// Both kinds present: the last one is the decimal, the other kind groups in threes.
pub fn parse_number(s: &str, group: char) -> Option<f64> {
    plain_number(s, group)?.parse().ok()
}

/// The typed number `s` (read as [`parse_number`] does) in Rust's own form:
/// no grouping, `.` as the decimal point, `-` for a negative one.
pub fn plain_number(s: &str, group: char) -> Option<String> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.') {
        return None;
    }
    let commas = body.matches(',').count();
    let dots = body.matches('.').count();
    let one_kind = (commas == 0) != (dots == 0);
    let normalized = match (commas, dots) {
        (0, 0) => body.to_string(),
        _ if one_kind && body.contains(group) && grouped_in_threes(body.split(group)) => body.replace(group, ""),
        (1, 0) | (0, 1) => {
            let (int, frac) = body.split_once(['.', ','])?;
            if frac.is_empty() {
                return None;
            }
            format!("{}.{}", if int.is_empty() { "0" } else { int }, frac)
        }
        (c, d) if c > 0 && d > 0 => {
            let last = body.rfind(['.', ','])?;
            let decimal = body.as_bytes()[last] as char;
            let grouping = if decimal == '.' { ',' } else { '.' };
            let (int, frac) = (&body[..last], &body[last + 1..]);
            if frac.is_empty() || frac.contains(grouping) {
                return None;
            }
            let groups: Vec<&str> = int.split(grouping).collect();
            let ok = groups.len() >= 2
                && !int.contains(decimal)
                && grouped_in_threes(groups.iter().copied());
            if !ok {
                return None;
            }
            format!("{}.{}", groups.concat(), frac)
        }
        _ => return None,
    };
    Some(if neg { format!("-{normalized}") } else { normalized })
}

/// Digit runs split at a grouping separator: the first has one to three
/// digits, every later one exactly three.
fn grouped_in_threes<'a>(mut groups: impl Iterator<Item = &'a str>) -> bool {
    let all_digits = |g: &str| g.bytes().all(|b| b.is_ascii_digit());
    groups.next().is_some_and(|g| (1..=3).contains(&g.len()) && all_digits(g))
        && groups.all(|g| g.len() == 3 && all_digits(g))
}

/// Significant digits shown for `x`: every digit of an integer below 1e15 (exact
/// in f64), otherwise the fewest (up to 15) that are within 1e-14 of `x`, so
/// binary float noise (`0.1 + 0.2`, `10.1 - 10`) is dropped.
fn precision(x: f64) -> usize {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        return 15;
    }
    (1..15)
        .find(|&p| {
            let rounded: f64 = format!("{:.*e}", p - 1, x).parse().unwrap_or(f64::NAN);
            (rounded - x).abs() < x.abs() * 1e-14
        })
        .unwrap_or(15)
}

/// Formats with up to 15 significant digits (see [`precision`]), no trailing zeros,
/// thousands grouping and scientific notation for very large or very small magnitudes.
pub fn format_number(x: f64, decimal: char, group: char) -> String {
    if !x.is_finite() {
        return x.to_string();
    }
    if x == 0.0 {
        return "0".into();
    }
    let sci = format!("{:.*e}", precision(x) - 1, x.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let sign = if x < 0.0 { "-" } else { "" };

    if x.abs() >= 1e15 || x.abs() < 1e-9 {
        let (first, rest) = digits.split_at(1);
        let m = if rest.is_empty() {
            first.to_string()
        } else {
            format!("{first}{decimal}{rest}")
        };
        return format!("{sign}{m}e{exp}");
    }
    let (int, frac) = if exp >= 0 {
        let n = exp as usize + 1;
        if digits.len() <= n {
            (format!("{digits}{}", "0".repeat(n - digits.len())), String::new())
        } else {
            (digits[..n].to_string(), digits[n..].to_string())
        }
    } else {
        ("0".to_string(), format!("{}{digits}", "0".repeat((-exp - 1) as usize)))
    };
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(group);
        }
        grouped.push(c);
    }
    if frac.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}{decimal}{frac}")
    }
}

/// Locale-dependent output: numbers, dates, times, units and currency defaults.
pub trait Formats: Send + Sync {
    fn number(&self, x: f64) -> String;
    /// The thousands separator, for reading typed numbers (see [`parse_number`]).
    fn group(&self) -> char;
    fn date(&self, d: Date, long: bool) -> String;
    /// The time of day with seconds, in the user's format.
    fn time_seconds(&self, t: Time) -> String;
    fn fahrenheit(&self) -> bool;
    /// ISO code of the user's currency, when known.
    fn currency(&self) -> Option<String>;
}

fn plain_time_seconds(t: Time) -> String {
    format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())
}

/// Whether `formatted` shows the seconds of `t` as two ASCII digits. A format
/// without seconds (or with other digits) would make a ticking row look still.
fn shows_seconds(formatted: &str, t: Time) -> bool {
    formatted.contains(&format!("{:02}", t.second()))
}

/// The user's Windows locale, read once.
pub struct WindowsFormats {
    decimal: char,
    group: char,
    fahrenheit: bool,
    currency: Option<String>,
}

fn locale_info(lctype: u32) -> Option<String> {
    let mut buf = [0u16; 80];
    // SAFETY: a null locale name means the user default; the buffer outlives the call.
    let n = unsafe { GetLocaleInfoEx(PCWSTR::null(), lctype, Some(&mut buf)) };
    if n <= 1 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..n as usize - 1]))
}

fn systemtime(d: Date, t: Time) -> SYSTEMTIME {
    SYSTEMTIME {
        wYear: d.year() as u16,
        wMonth: d.month() as u16,
        wDayOfWeek: 0,
        wDay: d.day() as u16,
        wHour: t.hour() as u16,
        wMinute: t.minute() as u16,
        wSecond: t.second() as u16,
        wMilliseconds: 0,
    }
}

impl WindowsFormats {
    pub fn new() -> Self {
        let first_char = |s: Option<String>, default: char| {
            s.and_then(|s| s.chars().next()).unwrap_or(default)
        };
        let currency = locale_info(LOCALE_SINTLSYMBOL)
            .map(|s| s.trim().to_string())
            .filter(|s| s.len() == 3 && s.chars().all(|c| c.is_ascii_alphabetic()));
        Self {
            decimal: first_char(locale_info(LOCALE_SDECIMAL), '.'),
            group: first_char(locale_info(LOCALE_STHOUSAND), ','),
            fahrenheit: locale_info(LOCALE_IMEASURE).as_deref() == Some("1"),
            currency,
        }
    }
}

impl Formats for WindowsFormats {
    fn number(&self, x: f64) -> String {
        format_number(x, self.decimal, self.group)
    }

    fn group(&self) -> char {
        self.group
    }

    fn date(&self, d: Date, long: bool) -> String {
        let st = systemtime(d, Time::midnight());
        let mut buf = [0u16; 128];
        let flags = if long { DATE_LONGDATE } else { DATE_SHORTDATE };
        // SAFETY: st and buf outlive the call; null locale = user default.
        let n = unsafe {
            GetDateFormatEx(
                PCWSTR::null(),
                flags,
                Some(&st),
                PCWSTR::null(),
                Some(&mut buf),
                PCWSTR::null(),
            )
        };
        if n <= 1 {
            return d.to_string();
        }
        String::from_utf16_lossy(&buf[..n as usize - 1])
    }

    fn time_seconds(&self, t: Time) -> String {
        let st = systemtime(Date::constant(2000, 1, 1), t);
        let mut buf = [0u16; 64];
        // SAFETY: st and buf outlive the call; null locale = user default.
        let n = unsafe {
            GetTimeFormatEx(
                PCWSTR::null(),
                TIME_FORMAT_FLAGS(0),
                Some(&st),
                PCWSTR::null(),
                Some(&mut buf),
            )
        };
        if n <= 1 {
            return plain_time_seconds(t);
        }
        let text = String::from_utf16_lossy(&buf[..n as usize - 1]);
        if shows_seconds(&text, t) {
            text
        } else {
            plain_time_seconds(t)
        }
    }

    fn fahrenheit(&self) -> bool {
        self.fahrenheit
    }

    fn currency(&self) -> Option<String> {
        self.currency.clone()
    }
}

/// Fixed formats for tests: `Wed 1 Jan 2027` / `01.01.2027`, `HH:MM:SS`, EUR, Celsius.
#[cfg(test)]
pub(crate) struct TestFormats {
    pub decimal: char,
    pub group: char,
}

#[cfg(test)]
impl Formats for TestFormats {
    fn number(&self, x: f64) -> String {
        format_number(x, self.decimal, self.group)
    }

    fn group(&self) -> char {
        self.group
    }

    fn date(&self, d: Date, long: bool) -> String {
        if long {
            d.strftime("%a %-d %b %Y").to_string()
        } else {
            d.strftime("%d.%m.%Y").to_string()
        }
    }

    fn time_seconds(&self, t: Time) -> String {
        plain_time_seconds(t)
    }

    fn fahrenheit(&self) -> bool {
        false
    }

    fn currency(&self) -> Option<String> {
        Some("EUR".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_decimal_separators() {
        // English locale: `,` groups.
        let en = |s| parse_number(s, ',');
        assert_eq!(en("5,5"), Some(5.5));
        assert_eq!(en("1.5"), Some(1.5));
        assert_eq!(en("1.234,5"), Some(1234.5));
        assert_eq!(en("1,234.5"), Some(1234.5));
        assert_eq!(en("1.000"), Some(1.0)); // a single separator that is not the group = decimal
        assert_eq!(en("1.2.3"), None);
        assert_eq!(en("1,23.4"), None); // groups must be threes
        assert_eq!(en(".5"), Some(0.5));
    }

    #[test]
    fn the_locales_group_with_three_digits_groups() {
        let en = |s| parse_number(s, ',');
        let de = |s| parse_number(s, '.');
        assert_eq!(en("1,500"), Some(1500.0));
        assert_eq!(en("-1,000,000"), Some(-1e6));
        assert_eq!(de("1.000"), Some(1000.0));
        assert_eq!(de("1.000.000"), Some(1e6));
        // Not the locale's group, or not three digits after it: the decimal.
        assert_eq!(de("1,500"), Some(1.5));
        assert_eq!(en("1,5"), Some(1.5));
        assert_eq!(en("1,5000"), Some(1.5));
        assert_eq!(en(",500"), Some(0.5));
        assert_eq!(de("1.000.00"), None);
        assert_eq!(en("1.000.000"), None);
        // Another group character (a French space): both stay decimals.
        assert_eq!(parse_number("1,500", '\u{202f}'), Some(1.5));
        assert_eq!(parse_number("1.500", '\u{202f}'), Some(1.5));
    }

    #[test]
    fn formats_like_the_locale() {
        assert_eq!(format_number(38.5, ',', '.'), "38,5");
        assert_eq!(format_number(1234.5, ',', '.'), "1.234,5");
        assert_eq!(format_number(1234.5, '.', ','), "1,234.5");
        assert_eq!(format_number(84.0, ',', '.'), "84");
        assert_eq!(format_number(2e16, '.', ','), "2e16");
        assert_eq!(format_number(-0.0, '.', ','), "0");
    }

    #[test]
    fn fifteen_significant_digits() {
        assert_eq!(format_number(1099511627776.0, '.', ','), "1,099,511,627,776");
        assert_eq!(format_number(12345678902.0, ',', '.'), "12.345.678.902");
        assert_eq!(format_number(999999999999999.0, '.', ','), "999,999,999,999,999");
        assert_eq!(format_number(0.1 + 0.2, ',', '.'), "0,3");
        assert_eq!(format_number(1.0 / 3.0, '.', ','), "0.33333333333333");
    }

    #[test]
    fn float_noise_is_dropped() {
        assert_eq!(format_number(1.1 - 1.0, ',', '.'), "0,1");
        assert_eq!(format_number(0.3 - 0.1, ',', '.'), "0,2");
        assert_eq!(format_number(0.1 * 3.0 * 10.0, ',', '.'), "3");
        assert_eq!(format_number(10000000000001.0, ',', '.'), "10.000.000.000.001");
        // Integers below 1e15 are exact in f64: every digit shows.
        assert_eq!(format_number(999999999999991.0, ',', '.'), "999.999.999.999.991");
        assert_eq!(format_number(-(1.1 - 1.0), ',', '.'), "-0,1");
        assert_eq!(format_number(2.0f64.sqrt(), '.', ','), "1.4142135623731");
        // Noise in the 15th digit: 10.1 - 10 is 0.09999999999999964.
        assert_eq!(format_number(10.1 - 10.0, ',', '.'), "0,1");
    }

    #[test]
    fn formats_edges_and_test_formats() {
        assert_eq!(format_number(1.5e16, ',', '.'), "1,5e16");
        assert_eq!(format_number(1e-10, '.', ','), "1e-10");
        assert_eq!(format_number(-1234567.0, '.', ','), "-1,234,567");
        assert_eq!(format_number(0.05, '.', ','), "0.05");
        let f = TestFormats { decimal: ',', group: '.' };
        assert_eq!(f.date(Date::constant(2027, 1, 1), true), "Fri 1 Jan 2027");
        assert_eq!(f.date(Date::constant(2027, 1, 1), false), "01.01.2027");
    }

    #[test]
    fn time_with_seconds() {
        let f = TestFormats { decimal: ',', group: '.' };
        assert_eq!(f.time_seconds(Time::constant(12, 5, 9, 0)), "12:05:09");
        let w = WindowsFormats::new();
        // Whatever the machine's format is, the seconds show.
        assert!(w.time_seconds(Time::constant(12, 5, 9, 0)).contains("09"));
    }

    #[test]
    fn a_time_without_seconds_falls_back_to_plain() {
        let t = Time::constant(12, 5, 9, 0);
        assert!(shows_seconds("12:05:09", t));
        assert!(shows_seconds("12:05:09 PM", t));
        assert!(!shows_seconds("12:05", t));
        assert!(!shows_seconds("12:05 PM", t));
        // Not ASCII digits: the tick would not be readable as seconds.
        assert!(!shows_seconds("\u{0661}\u{0662}:\u{0660}\u{0665}:\u{0660}\u{0669}", t));
    }

    #[test]
    fn windows_formats_produce_output() {
        let f = WindowsFormats::new();
        assert!(!f.number(1234.5).is_empty());
        assert!(!f.date(Date::constant(2027, 1, 1), true).is_empty());
        assert!(!f.time_seconds(Time::constant(9, 5, 0, 0)).is_empty());
    }
}
