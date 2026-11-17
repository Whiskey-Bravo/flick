//! Number parsing and formatting in the Windows locale.

use jiff::civil::{Date, Time};
use windows::core::PCWSTR;
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::Globalization::{
    GetDateFormatEx, GetLocaleInfoEx, GetTimeFormatEx, DATE_LONGDATE, DATE_SHORTDATE,
    LOCALE_IMEASURE, LOCALE_SDECIMAL, LOCALE_SINTLSYMBOL, LOCALE_STHOUSAND, TIME_NOSECONDS,
};

/// Parses a typed number. `,` and `.` both work as the decimal separator; a single
/// separator is always the decimal one. Grouping is accepted only when both kinds
/// are present: the last one is the decimal, the other kind groups in threes.
pub fn parse_number(s: &str) -> Option<f64> {
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
    let normalized = match (commas, dots) {
        (0, 0) => body.to_string(),
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
            let group = if decimal == '.' { ',' } else { '.' };
            let (int, frac) = (&body[..last], &body[last + 1..]);
            if frac.is_empty() || frac.contains(group) {
                return None;
            }
            let groups: Vec<&str> = int.split(group).collect();
            let ok = groups.len() >= 2
                && groups.iter().enumerate().all(|(i, g)| {
                    !g.contains(decimal)
                        && if i == 0 {
                            (1..=3).contains(&g.len())
                        } else {
                            g.len() == 3
                        }
                });
            if !ok {
                return None;
            }
            format!("{}.{}", groups.concat(), frac)
        }
        _ => return None,
    };
    let v: f64 = normalized.parse().ok()?;
    Some(if neg { -v } else { v })
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
    fn date(&self, d: Date, long: bool) -> String;
    fn time(&self, t: Time) -> String;
    fn fahrenheit(&self) -> bool;
    /// ISO code of the user's currency, when known.
    fn currency(&self) -> Option<String>;
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
        wSecond: 0,
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

    fn time(&self, t: Time) -> String {
        let st = systemtime(Date::constant(2000, 1, 1), t);
        let mut buf = [0u16; 64];
        // SAFETY: st and buf outlive the call; null locale = user default.
        let n = unsafe {
            GetTimeFormatEx(
                PCWSTR::null(),
                TIME_NOSECONDS,
                Some(&st),
                PCWSTR::null(),
                Some(&mut buf),
            )
        };
        if n <= 1 {
            return format!("{:02}:{:02}", t.hour(), t.minute());
        }
        String::from_utf16_lossy(&buf[..n as usize - 1])
    }

    fn fahrenheit(&self) -> bool {
        self.fahrenheit
    }

    fn currency(&self) -> Option<String> {
        self.currency.clone()
    }
}

/// Fixed formats for tests: `Wed 1 Jan 2027` / `01.01.2027`, `HH:MM`, EUR, Celsius.
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

    fn date(&self, d: Date, long: bool) -> String {
        if long {
            d.strftime("%a %-d %b %Y").to_string()
        } else {
            d.strftime("%d.%m.%Y").to_string()
        }
    }

    fn time(&self, t: Time) -> String {
        format!("{:02}:{:02}", t.hour(), t.minute())
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
        assert_eq!(parse_number("5,5"), Some(5.5));
        assert_eq!(parse_number("1.5"), Some(1.5));
        assert_eq!(parse_number("1.234,5"), Some(1234.5));
        assert_eq!(parse_number("1,234.5"), Some(1234.5));
        assert_eq!(parse_number("1.000"), Some(1.0)); // single separator = decimal (spec)
        assert_eq!(parse_number("1.2.3"), None);
        assert_eq!(parse_number("1,23.4"), None); // groups must be threes
        assert_eq!(parse_number(".5"), Some(0.5));
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
        assert_eq!(f.time(Time::constant(9, 5, 0, 0)), "09:05");
    }

    #[test]
    fn windows_formats_produce_output() {
        let f = WindowsFormats::new();
        assert!(!f.number(1234.5).is_empty());
        assert!(!f.date(Date::constant(2027, 1, 1), true).is_empty());
        assert!(!f.time(Time::constant(9, 5, 0, 0)).is_empty());
    }
}
