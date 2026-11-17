//! Time in other places and date maths.

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::Timestamp;

/// A named place with its IANA zone and extra spellings (all matched case-insensitively).
pub struct Place {
    pub name: &'static str,
    pub zone: &'static str,
    pub aliases: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    Days,
    Weeks,
    Months,
    Years,
}

#[derive(Debug)]
pub enum ClockAsk {
    TimeIn(&'static Place),
    LocalTime,
    Shift { amount: i64, unit: Span },
    DaysUntil(Date),
}

impl std::fmt::Debug for Place {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Place({})", self.name)
    }
}

/// Parses `time`, `time <place>`, `in 3 weeks`, `2 days ago` and `days until <date>`.
/// Keywords are whole words. `today` decides the year of a date typed without one.
pub fn parse_clock(text: &str, today: Date) -> Option<ClockAsk> {
    let lower = text.trim().to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    match words.as_slice() {
        ["time"] => Some(ClockAsk::LocalTime),
        ["time", rest @ ..] => find_place(&rest.join(" ")).map(ClockAsk::TimeIn),
        ["in", n, unit] => {
            let amount: i64 = n.parse().ok()?;
            Some(ClockAsk::Shift { amount, unit: parse_unit(unit)? })
        }
        [n, unit, "ago"] => {
            let amount: i64 = n.parse().ok()?;
            Some(ClockAsk::Shift { amount: amount.checked_neg()?, unit: parse_unit(unit)? })
        }
        ["days", "until", rest @ ..] => parse_date(rest, today).map(ClockAsk::DaysUntil),
        _ => None,
    }
}

fn parse_unit(w: &str) -> Option<Span> {
    match w {
        "day" | "days" => Some(Span::Days),
        "week" | "weeks" => Some(Span::Weeks),
        "month" | "months" => Some(Span::Months),
        "year" | "years" => Some(Span::Years),
        _ => None,
    }
}

fn find_place(name: &str) -> Option<&'static Place> {
    PLACES.iter().find(|p| {
        p.name.to_lowercase() == name || p.aliases.iter().any(|a| a.to_lowercase() == name)
    })
}

fn month_number(w: &str) -> Option<i8> {
    const MONTHS: [&str; 12] = [
        "january", "february", "march", "april", "may", "june", "july", "august", "september",
        "october", "november", "december",
    ];
    if w.chars().count() < 3 {
        return None;
    }
    if let Some(i) = MONTHS.iter().position(|m| *m == w || (w.len() == 3 && m.starts_with(w))) {
        return Some(i as i8 + 1);
    }
    // German spellings that differ from English.
    match w {
        "mär" | "märz" | "maerz" | "mrz" => Some(3),
        "mai" => Some(5),
        "okt" | "oktober" => Some(10),
        "dez" | "dezember" => Some(12),
        "sept" => Some(9),
        _ => None,
    }
}

/// Date without a year: this year, or next year when it already passed.
fn resolve(day: i8, month: i8, year: Option<i16>, today: Date) -> Option<Date> {
    match year {
        Some(y) => Date::new(y, month, day).ok(),
        None => {
            let d = Date::new(today.year(), month, day).ok()?;
            if d < today {
                Date::new(today.year() + 1, month, day).ok()
            } else {
                Some(d)
            }
        }
    }
}

fn parse_date(words: &[&str], today: Date) -> Option<Date> {
    let num = |s: &str| s.parse::<i8>().ok();
    match words {
        [one] => {
            // 24.12. / 24.12 / 24.12.2027
            let parts: Vec<&str> = one.trim_end_matches('.').split('.').collect();
            match parts.as_slice() {
                [d, m] => resolve(num(d)?, num(m)?, None, today),
                [d, m, y] if y.len() == 4 => resolve(num(d)?, num(m)?, Some(y.parse().ok()?), today),
                _ => None,
            }
        }
        [a, b] => {
            let a = a.trim_end_matches('.');
            if let (Some(d), Some(m)) = (num(a), month_number(b)) {
                resolve(d, m, None, today)
            } else if let (Some(m), Some(d)) = (month_number(a), num(b)) {
                resolve(d, m, None, today)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Adds `amount` units to a date; month and year steps clamp to the end of the month.
/// None when the amount or the result is outside the supported range.
pub fn shift(d: Date, amount: i64, unit: Span) -> Option<Date> {
    let zero = jiff::Span::new();
    let span = match unit {
        Span::Days => zero.try_days(amount),
        Span::Weeks => zero.try_weeks(amount),
        Span::Months => zero.try_months(amount),
        Span::Years => zero.try_years(amount),
    }
    .ok()?;
    d.checked_add(span).ok()
}

/// The place's local date and time, and how many minutes it is ahead of the PC's zone.
#[allow(dead_code)] // the bar uses `time_in_from` with the zone it read at start
pub fn time_in(place: &Place, now: Timestamp) -> Option<(DateTime, i64)> {
    static LOCAL: std::sync::OnceLock<TimeZone> = std::sync::OnceLock::new();
    time_in_from(place, now, LOCAL.get_or_init(TimeZone::system))
}

/// Like [`time_in`] with an explicit "local" zone, so tests do not depend on the PC.
pub fn time_in_from(place: &Place, now: Timestamp, local: &TimeZone) -> Option<(DateTime, i64)> {
    let zone = TimeZone::get(place.zone).ok()?;
    let ahead = i64::from(zone.to_offset(now).seconds() - local.to_offset(now).seconds()) / 60;
    Some((zone.to_datetime(now), ahead))
}

pub static PLACES: &[Place] = &[
    Place { name: "London", zone: "Europe/London", aliases: &["uk", "united kingdom", "england", "britain", "great britain"] },
    Place { name: "Dublin", zone: "Europe/Dublin", aliases: &["ireland"] },
    Place { name: "Paris", zone: "Europe/Paris", aliases: &["france"] },
    Place { name: "Berlin", zone: "Europe/Berlin", aliases: &["germany", "deutschland"] },
    Place { name: "Vienna", zone: "Europe/Vienna", aliases: &["wien", "austria", "oesterreich", "österreich"] },
    Place { name: "Munich", zone: "Europe/Berlin", aliases: &["münchen", "muenchen"] },
    Place { name: "Cologne", zone: "Europe/Berlin", aliases: &["köln", "koeln"] },
    Place { name: "Hamburg", zone: "Europe/Berlin", aliases: &[] },
    Place { name: "Frankfurt", zone: "Europe/Berlin", aliases: &[] },
    Place { name: "Zurich", zone: "Europe/Zurich", aliases: &["zürich", "switzerland", "schweiz"] },
    Place { name: "Geneva", zone: "Europe/Zurich", aliases: &["genf"] },
    Place { name: "Rome", zone: "Europe/Rome", aliases: &["rom", "italy", "italien"] },
    Place { name: "Milan", zone: "Europe/Rome", aliases: &["mailand"] },
    Place { name: "Madrid", zone: "Europe/Madrid", aliases: &["spain", "spanien"] },
    Place { name: "Barcelona", zone: "Europe/Madrid", aliases: &[] },
    Place { name: "Lisbon", zone: "Europe/Lisbon", aliases: &["lissabon", "portugal"] },
    Place { name: "Amsterdam", zone: "Europe/Amsterdam", aliases: &["netherlands", "holland"] },
    Place { name: "Brussels", zone: "Europe/Brussels", aliases: &["brüssel", "belgium", "belgien"] },
    Place { name: "Luxembourg", zone: "Europe/Luxembourg", aliases: &[] },
    Place { name: "Copenhagen", zone: "Europe/Copenhagen", aliases: &["denmark"] },
    Place { name: "Stockholm", zone: "Europe/Stockholm", aliases: &["sweden"] },
    Place { name: "Oslo", zone: "Europe/Oslo", aliases: &["norway"] },
    Place { name: "Helsinki", zone: "Europe/Helsinki", aliases: &["finland"] },
    Place { name: "Reykjavik", zone: "Atlantic/Reykjavik", aliases: &["iceland"] },
    Place { name: "Warsaw", zone: "Europe/Warsaw", aliases: &["warschau", "poland", "polen"] },
    Place { name: "Prague", zone: "Europe/Prague", aliases: &["prag", "czechia"] },
    Place { name: "Budapest", zone: "Europe/Budapest", aliases: &["hungary", "ungarn"] },
    Place { name: "Bratislava", zone: "Europe/Bratislava", aliases: &["slovakia"] },
    Place { name: "Ljubljana", zone: "Europe/Ljubljana", aliases: &["slovenia"] },
    Place { name: "Zagreb", zone: "Europe/Zagreb", aliases: &["croatia"] },
    Place { name: "Belgrade", zone: "Europe/Belgrade", aliases: &["serbia"] },
    Place { name: "Sofia", zone: "Europe/Sofia", aliases: &["bulgaria"] },
    Place { name: "Bucharest", zone: "Europe/Bucharest", aliases: &["romania"] },
    Place { name: "Athens", zone: "Europe/Athens", aliases: &["athen", "greece"] },
    Place { name: "Istanbul", zone: "Europe/Istanbul", aliases: &["turkey", "türkei"] },
    Place { name: "Kyiv", zone: "Europe/Kyiv", aliases: &["kiev", "ukraine"] },
    Place { name: "Moscow", zone: "Europe/Moscow", aliases: &["moskau", "russia"] },
    Place { name: "Riga", zone: "Europe/Riga", aliases: &["latvia"] },
    Place { name: "Tallinn", zone: "Europe/Tallinn", aliases: &["estonia"] },
    Place { name: "Vilnius", zone: "Europe/Vilnius", aliases: &["lithuania"] },
    Place { name: "Minsk", zone: "Europe/Minsk", aliases: &["belarus"] },
    Place { name: "Malta", zone: "Europe/Malta", aliases: &[] },
    Place { name: "Cyprus", zone: "Asia/Nicosia", aliases: &["nicosia"] },
    Place { name: "Beijing", zone: "Asia/Shanghai", aliases: &["peking", "china"] },
    Place { name: "Shanghai", zone: "Asia/Shanghai", aliases: &[] },
    Place { name: "Hong Kong", zone: "Asia/Hong_Kong", aliases: &["hongkong"] },
    Place { name: "Macau", zone: "Asia/Macau", aliases: &[] },
    Place { name: "Taipei", zone: "Asia/Taipei", aliases: &["taiwan"] },
    Place { name: "Tokyo", zone: "Asia/Tokyo", aliases: &["japan"] },
    Place { name: "Osaka", zone: "Asia/Tokyo", aliases: &[] },
    Place { name: "Seoul", zone: "Asia/Seoul", aliases: &["south korea", "korea"] },
    Place { name: "Pyongyang", zone: "Asia/Pyongyang", aliases: &["north korea"] },
    Place { name: "Singapore", zone: "Asia/Singapore", aliases: &[] },
    Place { name: "Bangkok", zone: "Asia/Bangkok", aliases: &["thailand"] },
    Place { name: "Hanoi", zone: "Asia/Bangkok", aliases: &["vietnam"] },
    Place { name: "Ho Chi Minh City", zone: "Asia/Ho_Chi_Minh", aliases: &["saigon"] },
    Place { name: "Phnom Penh", zone: "Asia/Phnom_Penh", aliases: &["cambodia"] },
    Place { name: "Vientiane", zone: "Asia/Vientiane", aliases: &["laos"] },
    Place { name: "Yangon", zone: "Asia/Yangon", aliases: &["myanmar", "rangoon"] },
    Place { name: "Kuala Lumpur", zone: "Asia/Kuala_Lumpur", aliases: &["malaysia"] },
    Place { name: "Jakarta", zone: "Asia/Jakarta", aliases: &["indonesia"] },
    Place { name: "Manila", zone: "Asia/Manila", aliases: &["philippines"] },
    Place { name: "Bali", zone: "Asia/Makassar", aliases: &[] },
    Place { name: "Delhi", zone: "Asia/Kolkata", aliases: &["new delhi", "india", "indien"] },
    Place { name: "Mumbai", zone: "Asia/Kolkata", aliases: &["bombay"] },
    Place { name: "Kolkata", zone: "Asia/Kolkata", aliases: &["calcutta"] },
    Place { name: "Bangalore", zone: "Asia/Kolkata", aliases: &["bengaluru"] },
    Place { name: "Chennai", zone: "Asia/Kolkata", aliases: &[] },
    Place { name: "Kathmandu", zone: "Asia/Kathmandu", aliases: &["nepal"] },
    Place { name: "Dhaka", zone: "Asia/Dhaka", aliases: &["bangladesh"] },
    Place { name: "Colombo", zone: "Asia/Colombo", aliases: &["sri lanka"] },
    Place { name: "Karachi", zone: "Asia/Karachi", aliases: &["pakistan"] },
    Place { name: "Islamabad", zone: "Asia/Karachi", aliases: &[] },
    Place { name: "Kabul", zone: "Asia/Kabul", aliases: &["afghanistan"] },
    Place { name: "Tehran", zone: "Asia/Tehran", aliases: &["iran"] },
    Place { name: "Baghdad", zone: "Asia/Baghdad", aliases: &["iraq"] },
    Place { name: "Riyadh", zone: "Asia/Riyadh", aliases: &["saudi arabia"] },
    Place { name: "Dubai", zone: "Asia/Dubai", aliases: &["uae", "united arab emirates"] },
    Place { name: "Abu Dhabi", zone: "Asia/Dubai", aliases: &[] },
    Place { name: "Doha", zone: "Asia/Qatar", aliases: &["qatar"] },
    Place { name: "Kuwait City", zone: "Asia/Kuwait", aliases: &["kuwait"] },
    Place { name: "Muscat", zone: "Asia/Muscat", aliases: &["oman"] },
    Place { name: "Jerusalem", zone: "Asia/Jerusalem", aliases: &["israel"] },
    Place { name: "Tel Aviv", zone: "Asia/Jerusalem", aliases: &[] },
    Place { name: "Beirut", zone: "Asia/Beirut", aliases: &["lebanon"] },
    Place { name: "Amman", zone: "Asia/Amman", aliases: &["jordan"] },
    Place { name: "Damascus", zone: "Asia/Damascus", aliases: &["syria"] },
    Place { name: "Tbilisi", zone: "Asia/Tbilisi", aliases: &["georgia"] },
    Place { name: "Yerevan", zone: "Asia/Yerevan", aliases: &["armenia"] },
    Place { name: "Baku", zone: "Asia/Baku", aliases: &["azerbaijan"] },
    Place { name: "Tashkent", zone: "Asia/Tashkent", aliases: &["uzbekistan"] },
    Place { name: "Almaty", zone: "Asia/Almaty", aliases: &["kazakhstan"] },
    Place { name: "Ulaanbaatar", zone: "Asia/Ulaanbaatar", aliases: &["mongolia"] },
    Place { name: "Vladivostok", zone: "Asia/Vladivostok", aliases: &[] },
    Place { name: "Novosibirsk", zone: "Asia/Novosibirsk", aliases: &[] },
    Place { name: "Cairo", zone: "Africa/Cairo", aliases: &["egypt", "ägypten"] },
    Place { name: "Lagos", zone: "Africa/Lagos", aliases: &["nigeria"] },
    Place { name: "Accra", zone: "Africa/Accra", aliases: &["ghana"] },
    Place { name: "Abidjan", zone: "Africa/Abidjan", aliases: &["ivory coast"] },
    Place { name: "Dakar", zone: "Africa/Dakar", aliases: &["senegal"] },
    Place { name: "Casablanca", zone: "Africa/Casablanca", aliases: &["morocco", "marokko"] },
    Place { name: "Algiers", zone: "Africa/Algiers", aliases: &["algeria"] },
    Place { name: "Tunis", zone: "Africa/Tunis", aliases: &["tunisia"] },
    Place { name: "Tripoli", zone: "Africa/Tripoli", aliases: &["libya"] },
    Place { name: "Khartoum", zone: "Africa/Khartoum", aliases: &["sudan"] },
    Place { name: "Addis Ababa", zone: "Africa/Addis_Ababa", aliases: &["ethiopia"] },
    Place { name: "Nairobi", zone: "Africa/Nairobi", aliases: &["kenya"] },
    Place { name: "Kampala", zone: "Africa/Kampala", aliases: &["uganda"] },
    Place { name: "Dar es Salaam", zone: "Africa/Dar_es_Salaam", aliases: &["tanzania"] },
    Place { name: "Kigali", zone: "Africa/Kigali", aliases: &["rwanda"] },
    Place { name: "Kinshasa", zone: "Africa/Kinshasa", aliases: &["congo"] },
    Place { name: "Luanda", zone: "Africa/Luanda", aliases: &["angola"] },
    Place { name: "Harare", zone: "Africa/Harare", aliases: &["zimbabwe"] },
    Place { name: "Lusaka", zone: "Africa/Lusaka", aliases: &["zambia"] },
    Place { name: "Maputo", zone: "Africa/Maputo", aliases: &["mozambique"] },
    Place { name: "Johannesburg", zone: "Africa/Johannesburg", aliases: &["south africa", "südafrika"] },
    Place { name: "Cape Town", zone: "Africa/Johannesburg", aliases: &["kapstadt"] },
    Place { name: "Windhoek", zone: "Africa/Windhoek", aliases: &["namibia"] },
    Place { name: "Antananarivo", zone: "Indian/Antananarivo", aliases: &["madagascar"] },
    Place { name: "New York", zone: "America/New_York", aliases: &["nyc", "new york city"] },
    Place { name: "Washington", zone: "America/New_York", aliases: &["washington dc"] },
    Place { name: "Boston", zone: "America/New_York", aliases: &[] },
    Place { name: "Miami", zone: "America/New_York", aliases: &[] },
    Place { name: "Atlanta", zone: "America/New_York", aliases: &[] },
    Place { name: "Toronto", zone: "America/Toronto", aliases: &[] },
    Place { name: "Montreal", zone: "America/Toronto", aliases: &[] },
    Place { name: "Chicago", zone: "America/Chicago", aliases: &[] },
    Place { name: "Houston", zone: "America/Chicago", aliases: &[] },
    Place { name: "Dallas", zone: "America/Chicago", aliases: &[] },
    Place { name: "Mexico City", zone: "America/Mexico_City", aliases: &["mexico", "mexiko"] },
    Place { name: "Denver", zone: "America/Denver", aliases: &[] },
    Place { name: "Phoenix", zone: "America/Phoenix", aliases: &[] },
    Place { name: "Calgary", zone: "America/Edmonton", aliases: &["edmonton"] },
    Place { name: "Los Angeles", zone: "America/Los_Angeles", aliases: &["la"] },
    Place { name: "San Francisco", zone: "America/Los_Angeles", aliases: &["sf"] },
    Place { name: "Seattle", zone: "America/Los_Angeles", aliases: &[] },
    Place { name: "Las Vegas", zone: "America/Los_Angeles", aliases: &[] },
    Place { name: "Vancouver", zone: "America/Vancouver", aliases: &[] },
    Place { name: "Anchorage", zone: "America/Anchorage", aliases: &["alaska"] },
    Place { name: "Honolulu", zone: "Pacific/Honolulu", aliases: &["hawaii"] },
    Place { name: "Havana", zone: "America/Havana", aliases: &["cuba"] },
    Place { name: "Panama City", zone: "America/Panama", aliases: &["panama"] },
    Place { name: "San Jose", zone: "America/Costa_Rica", aliases: &["costa rica"] },
    Place { name: "Bogota", zone: "America/Bogota", aliases: &["colombia", "kolumbien"] },
    Place { name: "Lima", zone: "America/Lima", aliases: &["peru"] },
    Place { name: "Quito", zone: "America/Guayaquil", aliases: &["ecuador"] },
    Place { name: "Caracas", zone: "America/Caracas", aliases: &["venezuela"] },
    Place { name: "La Paz", zone: "America/La_Paz", aliases: &["bolivia"] },
    Place { name: "Santiago", zone: "America/Santiago", aliases: &["chile"] },
    Place { name: "Buenos Aires", zone: "America/Argentina/Buenos_Aires", aliases: &["argentina", "argentinien"] },
    Place { name: "Montevideo", zone: "America/Montevideo", aliases: &["uruguay"] },
    Place { name: "Asuncion", zone: "America/Asuncion", aliases: &["paraguay"] },
    Place { name: "Sao Paulo", zone: "America/Sao_Paulo", aliases: &["são paulo", "brazil", "brasilien"] },
    Place { name: "Rio de Janeiro", zone: "America/Sao_Paulo", aliases: &["rio"] },
    Place { name: "Sydney", zone: "Australia/Sydney", aliases: &["australia", "australien"] },
    Place { name: "Melbourne", zone: "Australia/Melbourne", aliases: &[] },
    Place { name: "Brisbane", zone: "Australia/Brisbane", aliases: &[] },
    Place { name: "Perth", zone: "Australia/Perth", aliases: &[] },
    Place { name: "Adelaide", zone: "Australia/Adelaide", aliases: &[] },
    Place { name: "Darwin", zone: "Australia/Darwin", aliases: &[] },
    Place { name: "Canberra", zone: "Australia/Sydney", aliases: &[] },
    Place { name: "Auckland", zone: "Pacific/Auckland", aliases: &["new zealand", "neuseeland"] },
    Place { name: "Wellington", zone: "Pacific/Auckland", aliases: &[] },
    Place { name: "Fiji", zone: "Pacific/Fiji", aliases: &["suva"] },
    Place { name: "Port Moresby", zone: "Pacific/Port_Moresby", aliases: &["papua new guinea"] },
    Place { name: "UTC", zone: "UTC", aliases: &[] },
    Place { name: "GMT", zone: "Etc/GMT", aliases: &[] },
    Place { name: "EST", zone: "America/New_York", aliases: &[] },
    Place { name: "PST", zone: "America/Los_Angeles", aliases: &[] },
    Place { name: "CET", zone: "Europe/Berlin", aliases: &[] },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn place(s: &str) -> &'static Place {
        match parse_clock(s, jiff::civil::date(2026, 7, 1)) {
            Some(ClockAsk::TimeIn(p)) => p,
            _ => panic!("{s}"),
        }
    }

    #[test]
    fn time_in_tokyo_with_dst_elsewhere() {
        let now: Timestamp = "2026-07-01T12:00:00Z".parse().unwrap();
        let p = place;
        assert_eq!(time_in(p("time tokyo"), now).unwrap().0.time(), jiff::civil::time(21, 0, 0, 0));
        assert_eq!(time_in(p("time london"), now).unwrap().0.time(), jiff::civil::time(13, 0, 0, 0)); // BST
        assert_eq!(p("time wien").name, "Vienna");
    }

    #[test]
    fn offset_against_injected_local_zone() {
        let now: Timestamp = "2026-07-01T12:00:00Z".parse().unwrap();
        let local = TimeZone::get("Europe/Vienna").unwrap();
        let (dt, ahead) = time_in_from(place("time tokyo"), now, &local).unwrap();
        assert_eq!(dt.hour(), 21);
        assert_eq!(ahead, 7 * 60);
        assert_eq!(time_in_from(place("time new york"), now, &local).unwrap().1, -6 * 60);
        assert_eq!(time_in_from(place("time Los Angeles"), now, &local).unwrap().1, -9 * 60);
    }

    #[test]
    fn place_names_and_aliases() {
        assert_eq!(place("time München").name, "Munich");
        assert_eq!(place("time MUENCHEN").name, "Munich");
        assert_eq!(place("time Köln").name, "Cologne");
        assert_eq!(place("time rom").name, "Rome");
        assert_eq!(place("time mailand").name, "Milan");
        assert_eq!(place("time peking").name, "Beijing");
        assert_eq!(place("time utc").name, "UTC");
        assert_eq!(place("time  New   York ").name, "New York");
    }

    #[test]
    fn every_place_has_zone_data() {
        for p in PLACES {
            assert!(TimeZone::get(p.zone).is_ok(), "{} / {}", p.name, p.zone);
        }
        assert!(PLACES.len() >= 150);
    }

    #[test]
    fn local_time() {
        assert!(matches!(parse_clock("time", jiff::civil::date(2026, 7, 1)), Some(ClockAsk::LocalTime)));
    }

    #[test]
    fn relative_dates() {
        let today = jiff::civil::date(2026, 11, 17);
        assert!(matches!(parse_clock("in 45 days", today), Some(ClockAsk::Shift { amount: 45, unit: Span::Days })));
        assert!(matches!(parse_clock("3 weeks ago", today), Some(ClockAsk::Shift { amount: -3, unit: Span::Weeks })));
        assert!(matches!(parse_clock("in 1 year", today), Some(ClockAsk::Shift { amount: 1, unit: Span::Years })));
        assert!(matches!(parse_clock("2 months ago", today), Some(ClockAsk::Shift { amount: -2, unit: Span::Months })));
        assert_eq!(shift(jiff::civil::date(2027, 1, 31), 1, Span::Months), Some(jiff::civil::date(2027, 2, 28)));
        assert_eq!(shift(jiff::civil::date(2026, 11, 17), 45, Span::Days), Some(jiff::civil::date(2027, 1, 1)));
        assert_eq!(shift(jiff::civil::date(2026, 11, 17), -3, Span::Weeks), Some(jiff::civil::date(2026, 10, 27)));
        assert!(matches!(parse_clock("days until 24 dec", today), Some(ClockAsk::DaysUntil(d)) if d == jiff::civil::date(2026, 12, 24)));
        assert!(matches!(parse_clock("days until 1 jan", today), Some(ClockAsk::DaysUntil(d)) if d == jiff::civil::date(2027, 1, 1)));
    }

    #[test]
    fn huge_amounts_never_panic() {
        let today = jiff::civil::date(2026, 11, 17);
        for s in ["in 100000 years", "in 99999999999 days", "2000000 weeks ago", "in 9223372036854775807 months"] {
            if let Some(ClockAsk::Shift { amount, unit }) = parse_clock(s, today) {
                assert!(shift(today, amount, unit).is_none(), "{s}");
            }
        }
    }

    #[test]
    fn date_forms() {
        let today = jiff::civil::date(2026, 11, 17);
        let d = |s| match parse_clock(s, today) {
            Some(ClockAsk::DaysUntil(d)) => d,
            _ => panic!("{s}"),
        };
        assert_eq!(d("days until 24.12."), jiff::civil::date(2026, 12, 24));
        assert_eq!(d("days until 24.12.2027"), jiff::civil::date(2027, 12, 24));
        assert_eq!(d("days until dec 24"), jiff::civil::date(2026, 12, 24));
        assert_eq!(d("days until 17 nov"), today);
        assert_eq!(d("days until 16 nov"), jiff::civil::date(2027, 11, 16));
        assert!(parse_clock("days until 31 feb", today).is_none());
        assert!(parse_clock("days until soon", today).is_none());
    }

    #[test]
    fn not_clock_questions() {
        for s in ["timer", "time xyzzy", "in", "in 45", "incoming", "5 days", "times", "time in tokyo"] {
            assert!(parse_clock(s, jiff::civil::date(2026, 11, 17)).is_none(), "{s}");
        }
    }
}
