//! Date handling for feeds and RSS storage: the RFC 822/2822 parser
//! qBittorrent ported from KDElibs (`parseDate` in `rss_parser.cpp`), a
//! `Qt::ISODate`-compatible ISO 8601 parser for Atom/Dublin Core dates, and
//! the `Qt::RFC2822Date` text format used in `articles/*.json`,
//! `download_rules.json` and the WebAPI. All values are Unix milliseconds.

use std::sync::LazyLock;

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike, Utc};
use regex::Regex;

/// Outcome of [`parse_rfc822`], mirroring the three results of the original:
/// a date, "use the fallback date" or an invalid `QDateTime`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rfc822 {
    Valid(i64),
    Fallback,
    Invalid,
}

static PRIMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:([A-Z][a-z]+),\s*)?([0-9]{1,2})(\s+|-)([^-\s]+)(\s+|-)([0-9]{2,4})\s+([0-9][0-9]):([0-9][0-9])(?::([0-9][0-9]))?\s+(\S+)$",
    )
    .expect("valid regex")
});
static ASCTIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([A-Z][a-z]+)\s+(\S+)\s+([0-9][0-9])\s+([0-9][0-9]):([0-9][0-9]):([0-9][0-9])\s+([0-9]{4})$")
        .expect("valid regex")
});

const SHORT_MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Parses an RSS `pubDate` (RFC 822/2822 and the obsolete forms accepted by
/// KDElibs). Input is trimmed first.
pub(crate) fn parse_rfc822(input: &str) -> Rfc822 {
    let s = input.trim();
    if s.is_empty() || s.len() > 256 {
        return Rfc822::Fallback;
    }

    // (day, month, year, hour, minute, second, zone)
    let (day, month, year, hour, minute, second, zone): (&str, &str, &str, &str, &str, &str, Option<&str>);
    if let Some(c) = PRIMARY.captures(s) {
        let h1 = c.get(3).map(|m| m.as_str()) == Some("-");
        let h2 = c.get(5).map(|m| m.as_str()) == Some("-");
        if h1 != h2 {
            return Rfc822::Fallback;
        }
        day = c.get(2).map_or("", |m| m.as_str());
        month = c.get(4).map_or("", |m| m.as_str());
        year = c.get(6).map_or("", |m| m.as_str());
        hour = c.get(7).map_or("", |m| m.as_str());
        minute = c.get(8).map_or("", |m| m.as_str());
        second = c.get(9).map_or("", |m| m.as_str());
        zone = Some(c.get(10).map_or("", |m| m.as_str()));
    } else if let Some(c) = ASCTIME.captures(s) {
        month = c.get(2).map_or("", |m| m.as_str());
        day = c.get(3).map_or("", |m| m.as_str());
        hour = c.get(4).map_or("", |m| m.as_str());
        minute = c.get(5).map_or("", |m| m.as_str());
        second = c.get(6).map_or("", |m| m.as_str());
        year = c.get(7).map_or("", |m| m.as_str());
        zone = None;
    } else {
        return Rfc822::Fallback;
    }

    let (Ok(day), Ok(mut year_num), Ok(hour), Ok(minute)) =
        (day.parse::<i32>(), year.parse::<i32>(), hour.parse::<i32>(), minute.parse::<i32>())
    else {
        return Rfc822::Fallback;
    };
    let mut second_num = 0;
    if !second.is_empty() {
        match second.parse::<i32>() {
            Ok(v) => second_num = v,
            Err(_) => return Rfc822::Fallback,
        }
    }
    let leap_second = second_num == 60;
    if leap_second {
        second_num = 59;
    }
    let month_index = SHORT_MONTHS.iter().position(|m| *m == month).unwrap_or(12) as u32;
    if year.len() < 4 {
        year_num += if year.len() == 2 && year_num < 50 { 2000 } else { 1900 };
    }

    let mut offset: i64 = 0;
    if let Some(zone) = zone {
        let zb = zone.as_bytes();
        if zb.len() == 5 && (zb[0] == b'+' || zb[0] == b'-') && zb[1..].iter().all(u8::is_ascii_digit) {
            let hh = i64::from(zb[1] - b'0') * 10 + i64::from(zb[2] - b'0');
            let mm = i64::from(zb[3] - b'0') * 10 + i64::from(zb[4] - b'0');
            if mm > 59 {
                return Rfc822::Invalid;
            }
            offset = hh * 3600 + mm * 60;
            if zb[0] == b'-' {
                offset = -offset;
            }
        } else {
            // Qt's toLatin1(): characters outside Latin-1 become '?'.
            let latin: Vec<u8> = zone.chars().map(|c| if (c as u32) < 256 { c as u32 as u8 } else { b'?' }).collect();
            let military = latin.len() == 1 && latin[0].is_ascii_alphabetic() && !latin[0].eq_ignore_ascii_case(&b'J');
            if !military && latin != b"UT" && latin != b"GMT" {
                offset = match latin.as_slice() {
                    b"EDT" => -4 * 3600,
                    b"EST" | b"CDT" => -5 * 3600,
                    b"CST" | b"MDT" => -6 * 3600,
                    b"MST" | b"PDT" => -7 * 3600,
                    b"PST" => -8 * 3600,
                    _ => 0,
                };
                if offset == 0 && !latin.iter().all(u8::is_ascii_alphabetic) {
                    return Rfc822::Invalid;
                }
            }
        }
    }

    let Some(date) = NaiveDate::from_ymd_opt(year_num, month_index + 1, day.max(0) as u32).filter(|_| day > 0) else {
        return Rfc822::Fallback;
    };
    // An invalid time of day turns into midnight (Qt 6 QDateTime semantics).
    let time = NaiveTime::from_hms_opt(hour as u32, minute as u32, second_num as u32)
        .filter(|_| hour >= 0 && minute >= 0)
        .unwrap_or(NaiveTime::MIN);
    let utc = NaiveDateTime::new(date, time).and_utc().timestamp_millis() - offset * 1000;

    if leap_second {
        let check = i64::from(hour) * 3600 + i64::from(minute) * 60 + 60 - offset + 86400 * 5;
        if check.rem_euclid(86400) != 0 {
            return Rfc822::Fallback;
        }
    }
    Rfc822::Valid(utc)
}

/// `QDateTime::fromString(s, Qt::ISODate)`: `yyyy-MM-dd`, optionally followed
/// by `T` or a space and `hh:mm[:ss[.fff]]` plus `Z`, `±hh`, `±hhmm` or
/// `±hh:mm`. Missing zone means local time; a date alone is local midnight.
pub(crate) fn parse_iso8601(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 10 || b.len() > 64 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = b.get(r)?;
        if part.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(part).ok()?.parse().ok()
        } else {
            None
        }
    };
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(num(0..4)? as i32, num(5..7)?, num(8..10)?)?;
    if b.len() == 10 {
        return local_to_utc(NaiveDateTime::new(date, NaiveTime::MIN));
    }
    if b[10] != b'T' && b[10] != b' ' {
        return None;
    }
    let rest = &s[11..];
    // Split off the zone designator.
    let (time_part, zone) = match rest.find(['Z', '+', '-']) {
        Some(i) => (&rest[..i], Some(&rest[i..])),
        None => (rest, None),
    };
    let tb = time_part.as_bytes();
    if tb.len() < 5 || tb[2] != b':' {
        return None;
    }
    let tnum = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = tb.get(r)?;
        if part.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(part).ok()?.parse().ok()
        } else {
            None
        }
    };
    let hour = tnum(0..2)?;
    let minute = tnum(3..5)?;
    let mut second = 0;
    let mut millis = 0;
    if tb.len() > 5 {
        if tb[5] != b':' || tb.len() < 8 {
            return None;
        }
        second = tnum(6..8)?;
        if tb.len() > 8 {
            if tb[8] != b'.' && tb[8] != b',' {
                return None;
            }
            let frac = &tb[9..];
            if frac.is_empty() || !frac.iter().all(u8::is_ascii_digit) {
                return None;
            }
            let mut ms = 0u32;
            for i in 0..3 {
                ms = ms * 10 + frac.get(i).map_or(0, |d| u32::from(d - b'0'));
            }
            millis = ms;
        }
    }
    let naive = if hour == 24 && minute == 0 && second == 0 && millis == 0 {
        NaiveDateTime::new(date.succ_opt()?, NaiveTime::MIN)
    } else {
        NaiveDateTime::new(date, NaiveTime::from_hms_milli_opt(hour, minute, second, millis)?)
    };
    match zone {
        None => local_to_utc(naive),
        Some("Z") => Some(naive.and_utc().timestamp_millis()),
        Some(z) => {
            let zb = z.as_bytes();
            let sign: i64 = if zb[0] == b'-' { -1 } else { 1 };
            let digits: Vec<u8> = zb[1..].iter().copied().filter(|c| *c != b':').collect();
            let colon_ok = match zb.len() {
                3 | 5 => !zb[1..].contains(&b':'),
                6 => zb[3] == b':',
                _ => false,
            };
            if !colon_ok || !digits.iter().all(u8::is_ascii_digit) || !(digits.len() == 2 || digits.len() == 4) {
                return None;
            }
            let hh = i64::from(digits[0] - b'0') * 10 + i64::from(digits[1] - b'0');
            let mm = if digits.len() == 4 { i64::from(digits[2] - b'0') * 10 + i64::from(digits[3] - b'0') } else { 0 };
            if hh > 23 || mm > 59 {
                return None;
            }
            Some(naive.and_utc().timestamp_millis() - sign * (hh * 3600 + mm * 60) * 1000)
        }
    }
}

fn local_to_utc(naive: NaiveDateTime) -> Option<i64> {
    Local.from_local_datetime(&naive).earliest().map(|d| d.timestamp_millis())
}

/// `QDateTime::toString(Qt::RFC2822Date)` of a UTC date:
/// `03 Oct 2026 04:05:06 +0000`.
pub(crate) fn format_rfc2822(ms: i64) -> String {
    let Some(d) = Utc.timestamp_millis_opt(ms).single() else {
        return String::new();
    };
    format!(
        "{:02} {} {:04} {:02}:{:02}:{:02} +0000",
        d.day(),
        SHORT_MONTHS[d.month0() as usize],
        d.year(),
        d.hour(),
        d.minute(),
        d.second()
    )
}

/// Parses dates written by [`format_rfc2822`] or by qBittorrent
/// (`Qt::RFC2822Date`, with or without weekday). `None` if invalid/empty.
pub(crate) fn parse_rfc2822(s: &str) -> Option<i64> {
    match parse_rfc822(s) {
        Rfc822::Valid(ms) => Some(ms),
        _ => None,
    }
}

pub(crate) fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::Rfc822 as D;
    use super::*;

    /// Expected values were produced by running qBittorrent's `parseDate`
    /// (compiled against Qt 6) on the same inputs.
    #[test]
    fn rfc822_matches_qbittorrent() {
        let cases: &[(&str, D)] = &[
            ("Sat, 03 Oct 2026 04:05:06 +0000", D::Valid(1791000306000)),
            ("Sat, 03 Oct 2026 04:05:06 GMT", D::Valid(1791000306000)),
            ("Sat, 3 Oct 2026 04:05:06 UT", D::Valid(1791000306000)),
            ("03 Oct 2026 04:05:06 +0200", D::Valid(1790993106000)),
            ("03 Oct 2026 04:05 -0130", D::Valid(1791005700000)),
            ("Saturday, 03 Oct 2026 04:05:06 EST", D::Valid(1791018306000)),
            ("03-Oct-26 04:05:06 EDT", D::Valid(1791014706000)),
            ("03-Oct-99 04:05:06 PDT", D::Valid(938948706000)),
            ("03 Oct 126 04:05:06 PST", D::Valid(1791029106000)),
            ("03-Oct 2026 04:05:06 +0000", D::Fallback),
            ("03 oct 2026 04:05:06 +0000", D::Fallback),
            ("03 Foo 2026 04:05:06 +0000", D::Fallback),
            ("Sat Oct 03 04:05:06 2026", D::Valid(1791000306000)),
            ("Sat Oct  3 04:05:06 2026", D::Fallback),
            ("31 Dec 2016 23:59:60 +0000", D::Valid(1483228799000)),
            ("31 Dec 2016 22:59:60 -0100", D::Valid(1483228799000)),
            ("30 Dec 2016 23:59:60 +0000", D::Valid(1483142399000)),
            ("03 Oct 2026 04:05:06 +0099", D::Invalid),
            ("03 Oct 2026 04:05:06 Z", D::Valid(1791000306000)),
            ("03 Oct 2026 04:05:06 J", D::Valid(1791000306000)),
            ("03 Oct 2026 04:05:06 CEST", D::Valid(1791000306000)),
            ("03 Oct 2026 04:05:06 UTC+1", D::Invalid),
            ("03 Oct 2026 25:05:06 +0000", D::Valid(1790985600000)),
            ("31 Feb 2026 04:05:06 +0000", D::Fallback),
            ("  Sat, 03 Oct 2026 04:05:06 +0000  ", D::Valid(1791000306000)),
            ("2026-10-03T04:05:06Z", D::Fallback),
            ("", D::Fallback),
            ("garbage", D::Fallback),
            ("Sat,03 Oct 2026 04:05:06 +0000", D::Valid(1791000306000)),
            ("03 Oct 2026 04:05:06 +0000 extra", D::Fallback),
            ("03 Oct 2026 04:05:61 +0000", D::Valid(1790985600000)),
            ("03 Oct 2026 04:60:06 +0000", D::Valid(1790985600000)),
            ("3 Oct 2026 04:05:06 CDT", D::Valid(1791018306000)),
            ("3 Oct 2026 04:05:06 MDT", D::Valid(1791021906000)),
            ("3 Oct 2026 04:05:06 MST", D::Valid(1791025506000)),
            ("3 Oct 2026 04:05:06 CST", D::Valid(1791021906000)),
            ("Sat, 03 Oct 2026 04:05:06 -0000", D::Valid(1791000306000)),
            ("Mon, 01 Jan 0001 00:00:00 +0000", D::Valid(-62135596800000)),
        ];
        for (input, expected) in cases {
            assert_eq!(parse_rfc822(input), *expected, "input {input:?}");
        }
    }

    #[test]
    fn iso8601_matches_qt() {
        assert_eq!(parse_iso8601("2026-10-03T04:05:06Z"), Some(1791000306000));
        assert_eq!(parse_iso8601("2026-10-03T04:05:06+02:00"), Some(1790993106000));
        assert_eq!(parse_iso8601("2026-10-03T04:05:06.123+0200"), Some(1790993106123));
        assert_eq!(parse_iso8601("2026-10-03 04:05:06Z"), Some(1791000306000));
        assert_eq!(parse_iso8601("2026-10-03T04:05Z"), Some(1791000300000));
        assert_eq!(parse_iso8601("2026-10-03T04:05:06+02"), Some(1790993106000));
        assert_eq!(parse_iso8601("2026-10-03T04:05:06.5Z"), Some(1791000306500));
        assert_eq!(parse_iso8601("2026-10-03T04:05:06,5Z"), Some(1791000306500));
        assert_eq!(parse_iso8601("2026-10-03T24:00:00Z"), Some(1791072000000));
        assert_eq!(parse_iso8601("20261003T040506Z"), None);
        assert_eq!(parse_iso8601("2026-10-03T04:05:06 +02:00"), None);
        assert_eq!(parse_iso8601(" 2026-10-03T04:05:06Z"), None);
        assert_eq!(parse_iso8601("2026-13-03T04:05:06Z"), None);
        assert_eq!(parse_iso8601("not a date at all"), None);
        // Local-time forms only need to be valid.
        assert!(parse_iso8601("2026-10-03").is_some());
        assert!(parse_iso8601("2026-10-03T04:05:06").is_some());
    }

    #[test]
    fn rfc2822_round_trip() {
        assert_eq!(format_rfc2822(1791000306000), "03 Oct 2026 04:05:06 +0000");
        assert_eq!(parse_rfc2822("03 Oct 2026 04:05:06 +0000"), Some(1791000306000));
        assert_eq!(parse_rfc2822("Sat, 03 Oct 2026 04:05:06 +0200"), Some(1790993106000));
        assert_eq!(parse_rfc2822(""), None);
        assert_eq!(parse_rfc2822("nope"), None);
    }

    #[test]
    fn hostile_dates_do_not_panic() {
        for s in [
            "99 Oct 9999 99:99:99 +9999",
            "00 Jan 0000 00:00:00 +0000",
            "Mon, 31 Dec 9999 23:59:60 -2359",
            "Mon, 1 Jan 50 00:00:60 +2359",
            "\u{0}\u{1}",
            "Ünïcödé, 03 Oct 2026 04:05:06 ÄÖ",
            "9999-99-99T99:99:99.9999999999Z",
            "2026-02-30T00:00:00Z",
            "2026-10-03T04:05:06+99:99",
            "2026-10-03T04:05:06-",
            "2026-10-03T",
        ] {
            let _ = parse_rfc822(s);
            let _ = parse_iso8601(s);
        }
        assert_eq!(format_rfc2822(i64::MAX), "");
    }
}
