//! `qBittorrent.formatDateTime`: Qt date/time format strings
//! (`QLocale::toString(QDateTime, format)`) rendered with chrono in local
//! time, and the short locale format used when no format is given.

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};

/// Local time of a Unix timestamp; `None` when out of range.
fn local(secs: i64) -> Option<DateTime<Local>> {
    Local.timestamp_opt(secs, 0).single()
}

/// `QLocale::system().toString(dt, QLocale::ShortFormat)`. The app shows
/// dates in the short ISO-like style qBit NeoRS uses elsewhere.
pub fn format_short(secs: i64) -> String {
    format_qt("yyyy-MM-dd HH:mm", secs)
}

/// Renders a Qt format string. Supported: `d dd ddd dddd M MM MMM MMMM yy
/// yyyy h hh H HH m mm s ss z zzz AP ap A a t`, and `'quoted text'`
/// (`''` is a literal quote). Unknown letters are copied as is.
pub fn format_qt(format: &str, secs: i64) -> String {
    let Some(dt) = local(secs) else { return String::new() };
    let chars: Vec<char> = format.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let has_ampm = format.contains("AP") || format.contains("ap") || format.contains('A') || format.contains('a');
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            if chars.get(i + 1) == Some(&'\'') {
                out.push('\'');
                i += 2;
                continue;
            }
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        out.push('\'');
                        i += 2;
                        continue;
                    }
                    break;
                }
                out.push(chars[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        let mut n = 1;
        while i + n < chars.len() && chars[i + n] == c {
            n += 1;
        }
        let used = match c {
            'd' => {
                let k = n.min(4);
                out.push_str(&match k {
                    1 => dt.day().to_string(),
                    2 => format!("{:02}", dt.day()),
                    3 => dt.format("%a").to_string(),
                    _ => dt.format("%A").to_string(),
                });
                k
            }
            'M' => {
                let k = n.min(4);
                out.push_str(&match k {
                    1 => dt.month().to_string(),
                    2 => format!("{:02}", dt.month()),
                    3 => dt.format("%b").to_string(),
                    _ => dt.format("%B").to_string(),
                });
                k
            }
            'y' if n >= 4 => {
                out.push_str(&format!("{:04}", dt.year()));
                4
            }
            'y' if n >= 2 => {
                out.push_str(&format!("{:02}", dt.year().rem_euclid(100)));
                2
            }
            'h' | 'H' => {
                let k = n.min(2);
                let hour = if c == 'h' && has_ampm {
                    let h = dt.hour() % 12;
                    if h == 0 { 12 } else { h }
                } else {
                    dt.hour()
                };
                out.push_str(&if k == 2 { format!("{hour:02}") } else { hour.to_string() });
                k
            }
            'm' => {
                let k = n.min(2);
                out.push_str(&if k == 2 { format!("{:02}", dt.minute()) } else { dt.minute().to_string() });
                k
            }
            's' => {
                let k = n.min(2);
                out.push_str(&if k == 2 { format!("{:02}", dt.second()) } else { dt.second().to_string() });
                k
            }
            'z' => {
                // Whole seconds only: milliseconds are always zero.
                out.push_str(if n >= 3 { "000" } else { "0" });
                if n >= 3 { 3 } else { 1 }
            }
            'A' | 'a' => {
                let pm = dt.hour() >= 12;
                let text = if pm { "PM" } else { "AM" };
                out.push_str(&if c == 'a' { text.to_lowercase() } else { text.to_string() });
                if chars.get(i + 1).is_some_and(|&p| p == 'P' || p == 'p') { 2 } else { 1 }
            }
            't' => {
                out.push_str(&dt.format("%Z").to_string());
                1
            }
            _ => {
                out.push(c);
                1
            }
        };
        i += used;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_qt_tokens() {
        let t = Local.with_ymd_and_hms(2026, 3, 7, 14, 5, 9).single().unwrap().timestamp();
        assert_eq!(format_qt("yyyy-MM-dd HH:mm:ss", t), "2026-03-07 14:05:09");
        assert_eq!(format_qt("d.M.yy h:m:s", t), "7.3.26 14:5:9");
        assert_eq!(format_qt("hh:mm AP", t), "02:05 PM");
        assert_eq!(format_qt("h ap", t), "2 pm");
        assert_eq!(format_qt("'Day' d 'o''clock' ''", t), "Day 7 o'clock '");
        assert_eq!(format_qt("ddd MMM", t), "Sat Mar");
        assert_eq!(format_qt("dddd MMMM", t), "Saturday March");
        assert_eq!(format_short(t), "2026-03-07 14:05");
        assert_eq!(format_qt("ss.zzz", t), "09.000");
    }
}
