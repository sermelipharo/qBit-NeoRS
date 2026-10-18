//! Incremental transfer-list sync for the GUI (record-level diffs).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::model::{Category, MainDataUpdate, ServerState, TorrentInfo, TrackerTorrents};

/// A full snapshot of the data shown in the main window.
#[derive(Clone, Debug, Default)]
pub struct MainDataSnapshot {
    pub torrents: Vec<TorrentInfo>,
    pub categories: Vec<Category>,
    pub tags: Vec<String>,
    /// Tracker URL -> hashes.
    pub trackers: BTreeMap<String, Vec<String>>,
    /// Order-independent fingerprint of all (torrent, tracker) pairs.
    pub trackers_fp: u64,
    /// `trackers` was not built because its fingerprint equals the one the
    /// caller already knows.
    pub trackers_unchanged: bool,
    pub server_state: ServerState,
}

/// Remembers what was last sent to the GUI.
#[derive(Default)]
pub struct GuiSyncState {
    rid: i64,
    sent: bool,
    torrents: HashMap<String, TorrentInfo>,
    categories: HashMap<String, Category>,
    tags: HashSet<String>,
    trackers: BTreeMap<String, Vec<String>>,
    trackers_fp: Option<u64>,
}

impl GuiSyncState {
    /// Fingerprint of the tracker map last sent (`None` before the first sync).
    pub fn known_trackers_fp(&self) -> Option<u64> {
        self.trackers_fp
    }

    pub fn reset(&mut self) {
        *self = Self { rid: self.rid, ..Default::default() };
    }

    pub fn diff(&mut self, snap: MainDataSnapshot) -> MainDataUpdate {
        self.rid += 1;
        let full = !self.sent;
        self.sent = true;
        let mut out = MainDataUpdate { rid: self.rid, full_update: full, ..Default::default() };

        let mut seen = HashSet::with_capacity(snap.torrents.len());
        for t in snap.torrents {
            seen.insert(t.hash.clone());
            if full || self.torrents.get(&t.hash) != Some(&t) {
                self.torrents.insert(t.hash.clone(), t.clone());
                out.torrents.push(t);
            }
        }
        self.torrents.retain(|h, _| {
            let keep = seen.contains(h);
            if !keep {
                out.torrents_removed.push(h.clone());
            }
            keep
        });

        let mut seen_cats = HashSet::new();
        for c in snap.categories {
            seen_cats.insert(c.name.clone());
            if full || self.categories.get(&c.name) != Some(&c) {
                self.categories.insert(c.name.clone(), c.clone());
                out.categories.push(c);
            }
        }
        self.categories.retain(|n, _| {
            let keep = seen_cats.contains(n);
            if !keep {
                out.categories_removed.push(n.clone());
            }
            keep
        });

        let new_tags: HashSet<String> = snap.tags.into_iter().collect();
        for t in &new_tags {
            if full || !self.tags.contains(t) {
                out.tags.push(t.clone());
            }
        }
        for t in &self.tags {
            if !new_tags.contains(t) {
                out.tags_removed.push(t.clone());
            }
        }
        self.tags = new_tags;

        if snap.trackers_unchanged && !full {
            // Same fingerprint as the last sync: nothing to send or compare.
        } else {
            for (url, hashes) in &snap.trackers {
                if full || self.trackers.get(url) != Some(hashes) {
                    out.trackers.push(TrackerTorrents { url: url.clone(), hashes: hashes.clone() });
                }
            }
            for url in self.trackers.keys() {
                if !snap.trackers.contains_key(url) {
                    out.trackers_removed.push(url.clone());
                }
            }
            self.trackers = snap.trackers;
        }
        self.trackers_fp = Some(snap.trackers_fp);

        out.server_state = snap.server_state;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(hash: &str, progress: f64) -> TorrentInfo {
        TorrentInfo { hash: hash.into(), progress, ..Default::default() }
    }

    #[test]
    fn sends_only_changes_after_full_update() {
        let mut s = GuiSyncState::default();
        let snap = |ts: Vec<TorrentInfo>| MainDataSnapshot { torrents: ts, ..Default::default() };
        let u = s.diff(snap(vec![t("a", 0.1), t("b", 0.2)]));
        assert!(u.full_update);
        assert_eq!(u.torrents.len(), 2);

        let u = s.diff(snap(vec![t("a", 0.1), t("b", 0.3)]));
        assert!(!u.full_update);
        assert_eq!(u.torrents.len(), 1);
        assert_eq!(u.torrents[0].hash, "b");

        let u = s.diff(snap(vec![t("b", 0.3)]));
        assert_eq!(u.torrents_removed, vec!["a".to_string()]);
        assert!(u.torrents.is_empty());

        s.reset();
        let u = s.diff(snap(vec![t("b", 0.3)]));
        assert!(u.full_update);
        assert_eq!(u.torrents.len(), 1);
    }

    #[test]
    fn unchanged_tracker_fingerprint_skips_the_map() {
        let mut s = GuiSyncState::default();
        let map = |h: &str| BTreeMap::from([("udp://t".to_string(), vec![h.to_string()])]);
        let snap = |fp, unchanged, trackers| MainDataSnapshot { trackers, trackers_fp: fp, trackers_unchanged: unchanged, ..Default::default() };
        let u = s.diff(snap(1, false, map("a")));
        assert_eq!(u.trackers.len(), 1);
        assert_eq!(s.known_trackers_fp(), Some(1));
        // Unchanged: an empty map must not be read as "all trackers removed".
        let u = s.diff(snap(1, true, BTreeMap::new()));
        assert!(u.trackers.is_empty() && u.trackers_removed.is_empty());
        let u = s.diff(snap(2, false, map("b")));
        assert_eq!(u.trackers.len(), 1);
        // After a reset the next update is full whatever the caller claims.
        s.reset();
        assert_eq!(s.known_trackers_fp(), None);
        let u = s.diff(snap(2, true, map("b")));
        assert_eq!(u.trackers.len(), 1);
    }
}
