//! Tracker list helpers (`trackerentry.cpp`): text format parsing, merging
//! and the "additional trackers" rules.

use crate::model::TrackerTierEntry;

/// `parseTrackerEntries`: one URL per line, trimmed; every empty line
/// increments the tier. Equality is by URL (first occurrence wins).
pub fn parse_tracker_entries(text: &str) -> Vec<TrackerTierEntry> {
    let mut out: Vec<TrackerTierEntry> = Vec::new();
    let mut tier: i64 = 0;
    for line in text.split('\n') {
        let url = line.trim();
        if url.is_empty() {
            tier = tier.saturating_add(1);
            continue;
        }
        if !out.iter().any(|t| t.url == url) {
            out.push(TrackerTierEntry { url: url.to_string(), tier });
        }
    }
    out
}

/// Text form of a tracker list (inverse of [`parse_tracker_entries`]).
pub fn to_text(trackers: &[TrackerTierEntry]) -> String {
    let mut sorted: Vec<&TrackerTierEntry> = trackers.iter().collect();
    sorted.sort_by_key(|t| t.tier);
    let mut out = String::new();
    let mut last_tier = sorted.first().map(|t| t.tier).unwrap_or(0);
    for (i, t) in sorted.iter().enumerate() {
        if i > 0 {
            out.push('\n');
            if t.tier != last_tier {
                out.push('\n');
            }
        }
        last_tier = t.tier;
        out.push_str(&t.url);
    }
    out
}

/// Appends `extra` after the existing tiers (tier = max existing + 1 +
/// entry tier), skipping URLs that are already present. Returns whether
/// anything was added.
pub fn append_after_last_tier(existing: &mut Vec<TrackerTierEntry>, extra: &[TrackerTierEntry]) -> bool {
    let base = existing.iter().map(|t| t.tier).max().map(|t| t + 1).unwrap_or(0);
    let mut added = false;
    for t in extra {
        if !existing.iter().any(|e| e.url == t.url) {
            existing.push(TrackerTierEntry { url: t.url.clone(), tier: base.saturating_add(t.tier.max(0)) });
            added = true;
        }
    }
    added
}

/// `addTrackers`: dedupe by URL, keep sorted by tier (stable).
pub fn add_trackers(existing: &mut Vec<TrackerTierEntry>, new: &[TrackerTierEntry]) -> bool {
    let mut added = false;
    for t in new {
        let url = t.url.trim();
        if url.is_empty() || existing.iter().any(|e| e.url == url) {
            continue;
        }
        existing.push(TrackerTierEntry { url: url.to_string(), tier: t.tier.clamp(0, 255) });
        added = true;
    }
    if added {
        existing.sort_by_key(|t| t.tier);
    }
    added
}

/// `replaceTrackers`: dedupe, sorted by tier.
pub fn normalized(list: &[TrackerTierEntry]) -> Vec<TrackerTierEntry> {
    let mut out: Vec<TrackerTierEntry> = Vec::new();
    add_trackers(&mut out, list);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(url: &str, tier: i64) -> TrackerTierEntry {
        TrackerTierEntry { url: url.into(), tier }
    }

    #[test]
    fn parses_tiers() {
        let list = parse_tracker_entries("a\n b \n\nc\n\n\nd\na\n");
        assert_eq!(list, vec![e("a", 0), e("b", 0), e("c", 1), e("d", 3)]);
        assert_eq!(parse_tracker_entries("\nx"), vec![e("x", 1)]);
        assert_eq!(to_text(&[e("a", 0), e("b", 0), e("c", 1)]), "a\nb\n\nc");
    }

    #[test]
    fn appends_after_last_tier() {
        let mut list = vec![e("a", 0), e("b", 2)];
        assert!(append_after_last_tier(&mut list, &[e("c", 0), e("a", 0), e("d", 1)]));
        assert_eq!(list, vec![e("a", 0), e("b", 2), e("c", 3), e("d", 4)]);
        let mut empty = Vec::new();
        append_after_last_tier(&mut empty, &[e("x", 0)]);
        assert_eq!(empty, vec![e("x", 0)]);
    }

    #[test]
    fn add_dedupes_and_sorts() {
        let mut list = vec![e("a", 1)];
        assert!(add_trackers(&mut list, &[e("b", 0), e("a", 0), e(" ", 0)]));
        assert_eq!(list, vec![e("b", 0), e("a", 1)]);
        assert!(!add_trackers(&mut list, &[e("a", 5)]));
    }
}
