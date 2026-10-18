//! Alternative speed limits scheduler (`bandwidthscheduler.cpp`).

use crate::prefs::SchedulerDays;

/// Whether `days` includes the ISO weekday (Mon = 1 ... Sun = 7).
pub fn day_matches(days: SchedulerDays, iso_weekday: u32) -> bool {
    match days {
        SchedulerDays::EveryDay => true,
        SchedulerDays::Weekday => (1..=5).contains(&iso_weekday),
        SchedulerDays::Weekend => iso_weekday == 6 || iso_weekday == 7,
        d => (d.as_i32() - 2) as u32 == iso_weekday,
    }
}

/// `BandwidthScheduler::isTimeForAlternative`. Times are seconds since
/// midnight; the window is inclusive and may wrap around midnight.
pub fn is_time_for_alternative(now_secs: u32, iso_weekday: u32, from_secs: u32, to_secs: u32, days: SchedulerDays) -> bool {
    let (mut start, mut end) = (from_secs, to_secs);
    let mut alt = false;
    if start > end {
        std::mem::swap(&mut start, &mut end);
        alt = true;
    }
    if start <= now_secs && now_secs <= end && day_matches(days, iso_weekday) {
        alt = !alt;
    }
    alt
}

/// Same, from the preference fields and the local wall clock.
pub fn is_time_for_alternative_now(from_h: i64, from_m: i64, to_h: i64, to_m: i64, days: SchedulerDays) -> bool {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    let secs = now.num_seconds_from_midnight();
    let wd = now.weekday().number_from_monday();
    let clamp = |h: i64, m: i64| (h.clamp(0, 23) * 3600 + m.clamp(0, 59) * 60) as u32;
    is_time_for_alternative(secs, wd, clamp(from_h, from_m), clamp(to_h, to_m), days)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u32 = 3600;

    #[test]
    fn plain_window() {
        let d = SchedulerDays::EveryDay;
        assert!(!is_time_for_alternative(7 * H, 1, 8 * H, 20 * H, d));
        assert!(is_time_for_alternative(8 * H, 1, 8 * H, 20 * H, d));
        assert!(is_time_for_alternative(20 * H, 1, 8 * H, 20 * H, d));
        assert!(!is_time_for_alternative(20 * H + 1, 1, 8 * H, 20 * H, d));
    }

    #[test]
    fn window_crossing_midnight() {
        let d = SchedulerDays::EveryDay;
        assert!(is_time_for_alternative(23 * H, 3, 22 * H, 6 * H, d));
        assert!(is_time_for_alternative(H, 3, 22 * H, 6 * H, d));
        assert!(!is_time_for_alternative(12 * H, 3, 22 * H, 6 * H, d));
        // Boundaries of the swapped window are "inside" the normal span.
        assert!(!is_time_for_alternative(7 * H, 3, 22 * H, 6 * H, d));
    }

    #[test]
    fn day_selection() {
        assert!(day_matches(SchedulerDays::Weekday, 5));
        assert!(!day_matches(SchedulerDays::Weekday, 6));
        assert!(day_matches(SchedulerDays::Weekend, 7));
        assert!(day_matches(SchedulerDays::Monday, 1));
        assert!(day_matches(SchedulerDays::Sunday, 7));
        assert!(!day_matches(SchedulerDays::Sunday, 1));
        // On a non-matching day a plain window is off ...
        assert!(!is_time_for_alternative(12 * H, 6, 8 * H, 20 * H, SchedulerDays::Weekday));
        // ... but a wrapped window stays on all day (qBittorrent quirk).
        assert!(is_time_for_alternative(12 * H, 6, 22 * H, 6 * H, SchedulerDays::Weekday));
    }
}
