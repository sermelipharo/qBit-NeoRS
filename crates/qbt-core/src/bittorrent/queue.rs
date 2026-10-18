//! Torrent queueing (§4.5): the auto-manager decision as a pure function over
//! torrent snapshots, and queue position moves.

use std::collections::HashSet;

use crate::model::QueueMove;

#[derive(Clone, Copy, Debug)]
pub struct QueueLimits {
    pub enabled: bool,
    /// -1 = unlimited.
    pub max_downloads: i64,
    pub max_uploads: i64,
    pub max_active: i64,
    /// "Do not count slow torrents in these limits".
    pub dont_count_slow: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct QueueItem {
    /// Wants to run under the auto-manager (not stopped, not forced, no
    /// error, not moving).
    pub auto_managed: bool,
    /// Forced and able to run: counts against the total limit only.
    pub forced_active: bool,
    pub finished: bool,
    /// Queue position (downloaders); seeds use `seed_rank`.
    pub position: i64,
    /// Lower = seeds first (e.g. seeding time).
    pub seed_rank: i64,
    /// Slow for longer than the inactivity timer while running.
    pub inactive: bool,
}

/// Returns, per item, whether it must be held in the queue.
pub fn decide(items: &[QueueItem], limits: &QueueLimits) -> Vec<bool> {
    let mut queued = vec![false; items.len()];
    if !limits.enabled {
        return queued;
    }
    let unlimited = |v: i64| if v < 0 { i64::MAX } else { v };
    let forced = items.iter().filter(|i| i.forced_active).count() as i64;
    let mut hard = unlimited(limits.max_active).saturating_sub(forced).max(0);
    let mut downloaders: Vec<usize> = (0..items.len()).filter(|&i| items[i].auto_managed && !items[i].finished).collect();
    downloaders.sort_by_key(|&i| (items[i].position < 0, items[i].position));
    let mut seeds: Vec<usize> = (0..items.len()).filter(|&i| items[i].auto_managed && items[i].finished).collect();
    seeds.sort_by_key(|&i| items[i].seed_rank);

    let mut run = |list: &[usize], mut type_limit: i64, queued: &mut Vec<bool>| {
        for &i in list {
            if limits.dont_count_slow && items[i].inactive && hard > 0 {
                // Slow torrents keep running without taking a slot.
                continue;
            }
            if type_limit > 0 && hard > 0 {
                type_limit -= 1;
                hard -= 1;
            } else {
                queued[i] = true;
            }
        }
    };
    run(&downloaders, unlimited(limits.max_downloads), &mut queued);
    run(&seeds, unlimited(limits.max_uploads), &mut queued);
    queued
}

/// Applies a queue move to `order` (index = queue position) for the
/// selected ids, preserving their relative order like qBittorrent.
pub fn move_in_queue(order: &mut [String], selected: &HashSet<String>, mv: QueueMove) {
    let mut positions: Vec<usize> = order.iter().enumerate().filter(|(_, id)| selected.contains(*id)).map(|(i, _)| i).collect();
    positions.sort_unstable();
    match mv {
        QueueMove::Up => {
            let ids: Vec<String> = positions.iter().map(|&p| order[p].clone()).collect();
            for id in ids {
                if let Some(p) = order.iter().position(|x| *x == id)
                    && p > 0
                {
                    order.swap(p, p - 1);
                }
            }
        }
        QueueMove::Down => {
            let ids: Vec<String> = positions.iter().rev().map(|&p| order[p].clone()).collect();
            for id in ids {
                if let Some(p) = order.iter().position(|x| *x == id)
                    && p + 1 < order.len()
                {
                    order.swap(p, p + 1);
                }
            }
        }
        QueueMove::Top => {
            let ids: Vec<String> = positions.iter().rev().map(|&p| order[p].clone()).collect();
            for id in ids {
                if let Some(p) = order.iter().position(|x| *x == id) {
                    order[..=p].rotate_right(1);
                }
            }
        }
        QueueMove::Bottom => {
            let ids: Vec<String> = positions.iter().map(|&p| order[p].clone()).collect();
            for id in ids {
                if let Some(p) = order.iter().position(|x| *x == id) {
                    order[p..].rotate_left(1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dl(position: i64) -> QueueItem {
        QueueItem { auto_managed: true, forced_active: false, finished: false, position, seed_rank: 0, inactive: false }
    }

    fn seed(rank: i64) -> QueueItem {
        QueueItem { finished: true, position: -1, seed_rank: rank, ..dl(0) }
    }

    fn limits(d: i64, u: i64, t: i64) -> QueueLimits {
        QueueLimits { enabled: true, max_downloads: d, max_uploads: u, max_active: t, dont_count_slow: false }
    }

    #[test]
    fn disabled_queues_nothing() {
        let items = vec![dl(0), dl(1)];
        let l = QueueLimits { enabled: false, ..limits(0, 0, 0) };
        assert_eq!(decide(&items, &l), vec![false, false]);
    }

    #[test]
    fn downloads_in_queue_order() {
        let items = vec![dl(2), dl(0), dl(1), seed(0), seed(1)];
        assert_eq!(decide(&items, &limits(2, 1, 5)), vec![true, false, false, false, true]);
        // Total limit applies across downloads and seeds.
        assert_eq!(decide(&items, &limits(3, 3, 3)), vec![false, false, false, true, true]);
        // Unlimited.
        assert_eq!(decide(&items, &limits(-1, -1, -1)), vec![false; 5]);
    }

    #[test]
    fn forced_torrents_take_total_slots_only() {
        let mut forced = dl(0);
        forced.auto_managed = false;
        forced.forced_active = true;
        let items = vec![forced, dl(1), dl(2)];
        assert_eq!(decide(&items, &limits(2, 2, 2)), vec![false, false, true]);
    }

    #[test]
    fn slow_torrents_do_not_count() {
        let mut slow = dl(0);
        slow.inactive = true;
        let items = vec![slow, dl(1), dl(2)];
        let mut l = limits(1, 1, 5);
        assert_eq!(decide(&items, &l), vec![false, true, true]);
        l.dont_count_slow = true;
        assert_eq!(decide(&items, &l), vec![false, false, true]);
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn sel(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn queue_moves_preserve_relative_order() {
        let mut o = ids(&["a", "b", "c", "d", "e"]);
        move_in_queue(&mut o, &sel(&["c", "d"]), QueueMove::Up);
        assert_eq!(o, ids(&["a", "c", "d", "b", "e"]));
        move_in_queue(&mut o, &sel(&["c", "d"]), QueueMove::Down);
        assert_eq!(o, ids(&["a", "b", "c", "d", "e"]));
        move_in_queue(&mut o, &sel(&["c", "e"]), QueueMove::Top);
        assert_eq!(o, ids(&["c", "e", "a", "b", "d"]));
        move_in_queue(&mut o, &sel(&["c", "a"]), QueueMove::Bottom);
        assert_eq!(o, ids(&["e", "b", "d", "c", "a"]));
        move_in_queue(&mut o, &sel(&["e"]), QueueMove::Up);
        assert_eq!(o[0], "e");
        move_in_queue(&mut o, &sel(&["a"]), QueueMove::Down);
        assert_eq!(o[4], "a");
    }
}
