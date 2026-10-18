//! Human-readable formatting helpers (qBittorrent `Utils::Misc`).

/// Unit names in qBittorrent order (`Utils::Misc::SizeUnit`).
const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

/// Formats `n` with `precision` decimals, rounding down like
/// `Utils::String::fromDouble` (so 99.99 never shows as 100.0).
pub fn from_double(n: f64, precision: usize) -> String {
    let prec = 10f64.powi(precision as i32);
    format!("{:.*}", precision, (n * prec).floor() / prec)
}

/// Splits `bytes` into a value and a unit index (B, KiB, ... EiB).
fn split_to_friendly_unit(bytes: i64, threshold: f64) -> Option<(f64, usize)> {
    if bytes < 0 {
        return None;
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= threshold && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    Some((value, unit))
}

/// Decimal places qBittorrent shows per unit (`friendlyUnitPrecision`).
fn friendly_unit_precision(unit: usize) -> usize {
    match unit {
        0 => 0,
        1 | 2 => 1,
        3 => 2,
        _ => 3,
    }
}

/// `Utils::Misc::friendlyUnit`: "1.5 MiB", "300 B", "2.00 GiB/s".
/// Number and unit are separated by a no-break space, like qBittorrent.
/// Negative sizes are "Unknown".
pub fn friendly_unit(bytes: i64, is_speed: bool) -> String {
    let Some((value, unit)) = split_to_friendly_unit(bytes, 1024.0) else {
        return "Unknown".to_string();
    };
    let mut s = from_double(value, friendly_unit_precision(unit));
    s.push('\u{a0}');
    s.push_str(UNITS[unit]);
    if is_speed {
        s.push_str("/s");
    }
    s
}

/// `Utils::Misc::userFriendlyDuration` with second resolution: "0", "45s",
/// "12m", "3h 5m", "2d 10h", "1y 4d"; negative values or values at/above
/// `max_cap` (when `max_cap >= 0`) are "∞".
pub fn user_friendly_duration(seconds: i64, max_cap: i64) -> String {
    if seconds < 0 || (max_cap >= 0 && seconds >= max_cap) {
        return "∞".to_string();
    }
    if seconds == 0 {
        return "0".to_string();
    }
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let mut minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let mut hours = minutes / 60;
    if hours < 24 {
        minutes -= hours * 60;
        return format!("{hours}h {minutes}m");
    }
    let mut days = hours / 24;
    if days < 365 {
        hours -= days * 24;
        return format!("{days}d {hours}h");
    }
    let years = days / 365;
    days -= years * 365;
    format!("{years}y {days}d")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_units() {
        assert_eq!(friendly_unit(0, false), "0\u{a0}B");
        assert_eq!(friendly_unit(1023, false), "1023\u{a0}B");
        assert_eq!(friendly_unit(1024, false), "1.0\u{a0}KiB");
        assert_eq!(friendly_unit(1536 * 1024, false), "1.5\u{a0}MiB");
        // Rounds down like qBittorrent.
        assert_eq!(friendly_unit(1024 * 1024 - 1, false), "1023.9\u{a0}KiB");
        assert_eq!(friendly_unit(3 * 1024 * 1024 * 1024, true), "3.00\u{a0}GiB/s");
        assert_eq!(friendly_unit(5 * (1i64 << 40), false), "5.000\u{a0}TiB");
        assert_eq!(friendly_unit(-1, false), "Unknown");
    }

    #[test]
    fn durations() {
        assert_eq!(user_friendly_duration(0, -1), "0");
        assert_eq!(user_friendly_duration(59, -1), "59s");
        assert_eq!(user_friendly_duration(61, -1), "1m");
        assert_eq!(user_friendly_duration(3600 + 5 * 60, -1), "1h 5m");
        assert_eq!(user_friendly_duration(2 * 86400 + 10 * 3600, -1), "2d 10h");
        assert_eq!(user_friendly_duration(366 * 86400, -1), "1y 1d");
        assert_eq!(user_friendly_duration(-5, -1), "∞");
        assert_eq!(user_friendly_duration(100, 100), "∞");
    }
}
