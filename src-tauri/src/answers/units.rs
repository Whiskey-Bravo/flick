//! Unit and currency conversion questions: parsing and the conversion table.

use super::number::parse_number;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Length,
    Mass,
    Temperature,
    Volume,
    Area,
    Speed,
    Data,
    Duration,
}

/// `base = (x + offset) * factor`; the base unit of each kind is m, kg, degree Celsius,
/// l, m2, m/s, byte and s.
#[derive(Debug, PartialEq)]
pub struct Unit {
    pub name: &'static str,
    pub symbol: &'static str,
    pub kind: Kind,
    /// Lowercase spellings accepted in input (singular and plural names, symbols).
    pub aliases: &'static [&'static str],
    factor: f64,
    offset: f64,
}

const fn u(
    name: &'static str,
    symbol: &'static str,
    kind: Kind,
    aliases: &'static [&'static str],
    factor: f64,
) -> Unit {
    Unit { name, symbol, kind, aliases, factor, offset: 0.0 }
}

use Kind::*;

static UNITS: &[Unit] = &[
    // length
    u("millimeter", "mm", Length, &["mm", "millimeter", "millimeters", "millimetre", "millimetres"], 0.001),
    u("centimeter", "cm", Length, &["cm", "centimeter", "centimeters", "centimetre", "centimetres"], 0.01),
    u("meter", "m", Length, &["m", "meter", "meters", "metre", "metres"], 1.0),
    u("kilometer", "km", Length, &["km", "kilometer", "kilometers", "kilometre", "kilometres"], 1000.0),
    u("inch", "in", Length, &["in", "inch", "inches"], 0.0254),
    u("foot", "ft", Length, &["ft", "foot", "feet"], 0.3048),
    u("yard", "yd", Length, &["yd", "yard", "yards"], 0.9144),
    u("mile", "mi", Length, &["mi", "mile", "miles"], 1609.344),
    u("nautical mile", "nmi", Length, &["nmi", "nautical mile", "nautical miles"], 1852.0),
    // mass
    u("milligram", "mg", Mass, &["mg", "milligram", "milligrams"], 1e-6),
    u("gram", "g", Mass, &["g", "gram", "grams", "gramme", "grammes"], 0.001),
    u("kilogram", "kg", Mass, &["kg", "kilogram", "kilograms", "kilo", "kilos"], 1.0),
    u("tonne", "t", Mass, &["t", "tonne", "tonnes", "metric ton", "metric tons"], 1000.0),
    u("ounce", "oz", Mass, &["oz", "ounce", "ounces"], 0.028349523125),
    u("pound", "lb", Mass, &["lb", "lbs", "pound", "pounds"], 0.45359237),
    u("stone", "st", Mass, &["st", "stone", "stones"], 6.35029318),
    // temperature
    Unit {
        name: "degree Celsius",
        symbol: "°C",
        kind: Temperature,
        aliases: &["c", "°c", "celsius", "degree celsius", "degrees celsius"],
        factor: 1.0,
        offset: 0.0,
    },
    Unit {
        name: "degree Fahrenheit",
        symbol: "°F",
        kind: Temperature,
        aliases: &["f", "°f", "fahrenheit", "degree fahrenheit", "degrees fahrenheit"],
        factor: 5.0 / 9.0,
        offset: -32.0,
    },
    Unit {
        name: "kelvin",
        symbol: "K",
        kind: Temperature,
        aliases: &["k", "kelvin", "kelvins"],
        factor: 1.0,
        offset: -273.15,
    },
    // volume
    u("milliliter", "ml", Volume, &["ml", "milliliter", "milliliters", "millilitre", "millilitres"], 0.001),
    u("liter", "l", Volume, &["l", "liter", "liters", "litre", "litres"], 1.0),
    u("gallon", "gal", Volume, &["gal", "gallon", "gallons"], 3.785411784),
    u("quart", "qt", Volume, &["qt", "quart", "quarts"], 0.946352946),
    u("pint", "pt", Volume, &["pt", "pint", "pints"], 0.473176473),
    u("cup", "cup", Volume, &["cup", "cups"], 0.2365882365),
    u("fluid ounce", "fl oz", Volume, &["fl oz", "floz", "fluid ounce", "fluid ounces"], 0.0295735295625),
    // area
    u("square meter", "m²", Area, &["m2", "m²", "sqm", "square meter", "square meters", "square metre", "square metres"], 1.0),
    u("square kilometer", "km²", Area, &["km2", "km²", "sqkm", "square kilometer", "square kilometers", "square kilometre", "square kilometres"], 1e6),
    u("hectare", "ha", Area, &["ha", "hectare", "hectares"], 1e4),
    u("acre", "ac", Area, &["acre", "acres", "ac"], 4046.8564224),
    u("square foot", "ft²", Area, &["ft2", "ft²", "sqft", "square foot", "square feet"], 0.09290304),
    // speed
    u("meter per second", "m/s", Speed, &["m/s", "mps", "meter per second", "meters per second"], 1.0),
    u("kilometer per hour", "km/h", Speed, &["km/h", "kmh", "kph", "kmph", "kilometer per hour", "kilometers per hour"], 1.0 / 3.6),
    u("mile per hour", "mph", Speed, &["mph", "mi/h", "mile per hour", "miles per hour"], 0.44704),
    u("knot", "kn", Speed, &["kn", "kt", "knot", "knots"], 1852.0 / 3600.0),
    // data (b means byte)
    u("byte", "B", Data, &["b", "byte", "bytes"], 1.0),
    u("kilobyte", "kB", Data, &["kb", "kilobyte", "kilobytes"], 1e3),
    u("megabyte", "MB", Data, &["mb", "megabyte", "megabytes"], 1e6),
    u("gigabyte", "GB", Data, &["gb", "gigabyte", "gigabytes"], 1e9),
    u("terabyte", "TB", Data, &["tb", "terabyte", "terabytes"], 1e12),
    u("kibibyte", "KiB", Data, &["kib", "kibibyte", "kibibytes"], 1024.0),
    u("mebibyte", "MiB", Data, &["mib", "mebibyte", "mebibytes"], 1048576.0),
    u("gibibyte", "GiB", Data, &["gib", "gibibyte", "gibibytes"], 1073741824.0),
    u("tebibyte", "TiB", Data, &["tib", "tebibyte", "tebibytes"], 1099511627776.0),
    // durations
    u("second", "s", Duration, &["s", "sec", "secs", "second", "seconds"], 1.0),
    u("minute", "min", Duration, &["min", "mins", "minute", "minutes"], 60.0),
    u("hour", "h", Duration, &["h", "hr", "hrs", "hour", "hours"], 3600.0),
    u("day", "d", Duration, &["d", "day", "days"], 86400.0),
    u("week", "wk", Duration, &["wk", "week", "weeks"], 604800.0),
];

/// EUR plus the codes in the ECB daily reference-rate file. BGN left it when
/// Bulgaria took the euro (1 January 2026).
pub const CURRENCIES: &[&str] = &[
    "EUR", "USD", "JPY", "CZK", "DKK", "GBP", "HUF", "PLN", "RON", "SEK", "CHF", "ISK",
    "NOK", "TRY", "AUD", "BRL", "CAD", "CNY", "HKD", "IDR", "ILS", "INR", "KRW", "MXN", "MYR",
    "NZD", "PHP", "SGD", "THB", "ZAR",
];

#[derive(Debug)]
pub enum Ask {
    Unit { amount: f64, from: &'static Unit, to: &'static Unit },
    /// Uppercase ISO codes; `to: None` means the home currency.
    Money { amount: f64, from: String, to: Option<String> },
}

fn find_unit(s: &str) -> Option<&'static Unit> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    UNITS.iter().find(|u| u.aliases.contains(&s))
}

fn symbol_code(c: char) -> Option<&'static str> {
    match c {
        '$' => Some("USD"),
        '€' => Some("EUR"),
        '£' => Some("GBP"),
        '¥' => Some("JPY"),
        _ => None,
    }
}

fn currency_code(s: &str) -> Option<String> {
    let s = s.trim();
    let mut chars = s.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if let Some(code) = symbol_code(c) {
            return Some(code.into());
        }
    }
    let up = s.to_uppercase();
    CURRENCIES.contains(&up.as_str()).then_some(up)
}

/// Splits `20 usd`, `5km`, `$20`, `20$` into the amount, the remaining unit text
/// and a currency implied by a symbol. `group` is the locale's thousands separator.
fn split_quantity(s: &str, group: char) -> Option<(f64, String, Option<&'static str>)> {
    let s = s.trim();
    let first = s.chars().next()?;
    let last = s.chars().last()?;
    let (amount, rest, sym) = if let Some(code) = symbol_code(first) {
        (parse_number(&s[first.len_utf8()..], group)?, String::new(), Some(code))
    } else if let Some(code) = symbol_code(last) {
        (parse_number(&s[..s.len() - last.len_utf8()], group)?, String::new(), Some(code))
    } else {
        let end = s
            .char_indices()
            .find(|&(i, c)| {
                !(c.is_ascii_digit() || c == ',' || c == '.' || (i == 0 && (c == '-' || c == '+')))
            })
            .map_or(s.len(), |(i, _)| i);
        (parse_number(&s[..end], group)?, s[end..].trim().to_string(), None)
    };
    amount.is_finite().then_some((amount, rest, sym))
}

fn ask_between(left: &str, right: &str, group: char) -> Option<Ask> {
    let (amount, unit_text, sym) = split_quantity(left, group)?;
    if sym.is_none() {
        if let (Some(from), Some(to)) = (find_unit(&unit_text), find_unit(right)) {
            return Some(Ask::Unit { amount, from, to });
        }
    }
    let from = match sym {
        Some(code) => code.to_string(),
        None => currency_code(&unit_text)?,
    };
    Some(Ask::Money { amount, from, to: Some(currency_code(right)?) })
}

/// Recognises `<n> <unit> in|to <unit>`, the same with currencies, and a bare
/// `<n> <currency>` (converted to the home currency). `<n> in <unit>` is not an
/// answer: `in` is the separator there, so there is no source unit. `group` is
/// the locale's thousands separator (see [`parse_number`]).
pub fn parse_conversion(text: &str, group: char) -> Option<Ask> {
    let lower = text.trim().to_lowercase();
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    for (i, t) in tokens.iter().enumerate() {
        if *t == "in" || *t == "to" {
            let (left, right) = (tokens[..i].join(" "), tokens[i + 1..].join(" "));
            if let Some(ask) = ask_between(&left, &right, group) {
                return Some(ask);
            }
        }
    }
    let (amount, unit_text, sym) = split_quantity(&lower, group)?;
    let from = match sym {
        Some(code) => code.to_string(),
        None => currency_code(&unit_text)?,
    };
    Some(Ask::Money { amount, from, to: None })
}

/// None for different kinds or a non-finite result.
pub fn convert(amount: f64, from: &Unit, to: &Unit) -> Option<f64> {
    if from.kind != to.kind {
        return None;
    }
    let base = (amount + from.offset) * from.factor;
    let out = base / to.factor - to.offset;
    out.is_finite().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ans(s: &str) -> Option<f64> {
        match parse_conversion(s, ',') {
            Some(Ask::Unit { amount, from, to }) => convert(amount, from, to),
            _ => None,
        }
    }

    fn m(s: &str) -> Option<(f64, String, Option<String>)> {
        match parse_conversion(s, ',') {
            Some(Ask::Money { amount, from, to }) => Some((amount, from, to)),
            _ => None,
        }
    }

    #[test]
    fn units_convert() {
        assert!((ans("5 km in miles").unwrap() - 3.10685596).abs() < 1e-6);
        assert!((ans("72 f in c").unwrap() - 22.2222222).abs() < 1e-6);
        assert_eq!(ans("2 gb in mb"), Some(2000.0));
        assert_eq!(ans("1 gib to mib"), Some(1024.0));
        assert!((ans("5,5 km in m").unwrap() - 5500.0).abs() < 1e-9);
        assert!((ans("10 in in cm").unwrap() - 25.4).abs() < 1e-9);
        assert_eq!(ans("5 kg in km"), None);
    }

    #[test]
    fn unit_forms_and_edges() {
        assert!((ans("5km in m").unwrap() - 5000.0).abs() < 1e-9);
        assert!((ans("100 C to F").unwrap() - 212.0).abs() < 1e-9);
        assert!((ans("0 c in k").unwrap() - 273.15).abs() < 1e-9);
        assert!((ans("-300 c in k").unwrap() - -26.85).abs() < 1e-9);
        assert!((ans("1 gal to floz").unwrap() - 128.0).abs() < 1e-6);
        assert!((ans("36 kmh in m/s").unwrap() - 10.0).abs() < 1e-9);
        assert!((ans("1 ha in m2").unwrap() - 10000.0).abs() < 1e-9);
        assert!((ans("1 week in days").unwrap() - 7.0).abs() < 1e-9);
        assert_eq!(ans("5 in cm"), None); // `in` is the separator: no source unit
        assert_eq!(ans("5 km in km"), Some(5.0));
        assert_eq!(ans(&format!("{} km in mm", "9".repeat(320))), None);
    }

    #[test]
    fn money_forms() {
        assert_eq!(m("20 usd in eur"), Some((20.0, "USD".into(), Some("EUR".into()))));
        assert_eq!(m("€15 in gbp"), Some((15.0, "EUR".into(), Some("GBP".into()))));
        assert_eq!(m("$20"), Some((20.0, "USD".into(), None)));
        assert_eq!(m("20 usd"), Some((20.0, "USD".into(), None)));
        assert_eq!(m("20 xyz in eur"), None);
        assert_eq!(m("20 bgn in eur"), None, "the euro since 2026: no ECB rate");
        assert_eq!(m("20$"), Some((20.0, "USD".into(), None)));
        assert_eq!(m("20usd"), Some((20.0, "USD".into(), None)));
        assert_eq!(m("£5"), Some((5.0, "GBP".into(), None)));
        assert_eq!(m("¥100 to eur"), Some((100.0, "JPY".into(), Some("EUR".into()))));
        assert_eq!(m("5,5 EUR to Usd"), Some((5.5, "EUR".into(), Some("USD".into()))));
        assert_eq!(m("20 usd in km"), None);
    }

    #[test]
    fn not_conversions() {
        for s in ["5 in", "in 45 days", "km", "time tokyo", "5", "", "5 in cm", "5 to"] {
            assert!(parse_conversion(s, ',').is_none(), "{s}");
        }
    }

    #[test]
    fn table_is_consistent() {
        for (i, a) in UNITS.iter().enumerate() {
            for b in &UNITS[i + 1..] {
                for al in a.aliases {
                    assert!(!b.aliases.contains(al), "{al} duplicated");
                }
            }
        }
        for c in CURRENCIES {
            assert!(find_unit(&c.to_lowercase()).is_none(), "{c} clashes with a unit");
        }
    }
}
