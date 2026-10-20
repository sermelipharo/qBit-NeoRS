//! How often a torrent looks itself up on the DHT.
//!
//! Every live torrent runs one `get_peers` lookup loop per address family
//! (see [`dht::LookupPacer`]). A torrent that is seeding, or downloading
//! with plenty of peers, does not need fresh DHT peers every minute: like
//! libtorrent (`dht_announce_interval`, 15 min) it looks itself up every
//! 15 minutes. A download short of peers keeps a 1 minute interval, and a
//! download that has had enough peers for a while backs off step by step.
//! The loops are woken early when a torrent that had enough peers loses
//! some, or starts downloading again.
//!
//! | state                                   | interval                  |
//! |-----------------------------------------|---------------------------|
//! | not live yet (magnet, starting)         | 1 min                     |
//! | complete (seeding)                      | 15 min                    |
//! | downloading, short of peers             | 1 min (2 min after 10 min)|
//! | downloading, enough peers for < 1 min   | 1 min                     |
//! | ... for < 3 min                         | 2 min                     |
//! | ... for < 8 min                         | 5 min                     |
//! | ... longer                              | 15 min                    |
//!
//! Every interval is jittered by ±10% so torrents started together do not
//! stay in step.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use dht::LookupPacer;
use parking_lot::Mutex;

use super::live::TorrentStateLive;
use super::trackers::TorrentTrackers;

/// Interval while peers are needed.
pub(crate) const SHORT_INTERVAL: Duration = Duration::from_secs(60);
/// Interval of a torrent that does not need peers.
pub(crate) const LONG_INTERVAL: Duration = dht::DHT_ANNOUNCE_INTERVAL;
/// Downloading with enough peers: (enough for at least, interval).
const BACKOFF: [(Duration, Duration); 4] = [
    (Duration::ZERO, SHORT_INTERVAL),
    (Duration::from_secs(60), Duration::from_secs(2 * 60)),
    (Duration::from_secs(3 * 60), Duration::from_secs(5 * 60)),
    (Duration::from_secs(8 * 60), LONG_INTERVAL),
];
/// A download short of peers this long is in a small or dead swarm: the
/// DHT has been asked often enough, slow down a little.
const STALLED_AFTER: Duration = Duration::from_secs(10 * 60);
const STALLED_INTERVAL: Duration = Duration::from_secs(2 * 60);
/// Connections plus candidates a download wants before it has "enough"
/// peers: its connection limit, at most this many.
const WANTED_CAP: usize = 100;
/// ±10%.
const JITTER_PERMILLE: u32 = 100;

/// What a torrent needs from the DHT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PeerDemand {
    /// Not live: resolving a magnet or starting.
    Unknown,
    /// All selected pieces are here.
    Complete,
    /// Downloading with fewer connections and candidates than wanted.
    Short,
    /// Downloading with enough connections and candidates.
    Sufficient,
}

impl PeerDemand {
    /// `active`: connected and connecting peers; `candidates`: known peers
    /// waiting for a connection; `limit`: the torrent's connection limit.
    pub(crate) fn of(
        complete: bool,
        active: usize,
        candidates: usize,
        limit: Option<usize>,
    ) -> Self {
        if complete {
            return Self::Complete;
        }
        let wanted = limit.unwrap_or(WANTED_CAP).clamp(1, WANTED_CAP);
        if active.saturating_add(candidates) >= wanted {
            Self::Sufficient
        } else {
            Self::Short
        }
    }

    pub(crate) fn of_live(live: &TorrentStateLive) -> Self {
        let (active, queued) = live.active_and_queued_peers();
        Self::of(live.is_finished(), active, queued, live.peer_limit())
    }
}

/// The interval before jitter. `held_for`: how long the torrent has been
/// in `demand`.
pub(crate) fn base_interval(demand: PeerDemand, held_for: Duration) -> Duration {
    match demand {
        PeerDemand::Unknown => SHORT_INTERVAL,
        PeerDemand::Complete => LONG_INTERVAL,
        PeerDemand::Short if held_for >= STALLED_AFTER => STALLED_INTERVAL,
        PeerDemand::Short => SHORT_INTERVAL,
        PeerDemand::Sufficient => BACKOFF
            .iter()
            .rev()
            .find(|(after, _)| held_for >= *after)
            .map_or(SHORT_INTERVAL, |(_, d)| *d),
    }
}

/// `d` scaled by a factor in [0.9, 1.1] picked by `random` (any value).
pub(crate) fn jittered(d: Duration, random: u32) -> Duration {
    let span = 2 * JITTER_PERMILLE + 1;
    d * (1000 - JITTER_PERMILLE + random % span) / 1000
}

/// A torrent's DHT pacing state; lives in [`TorrentTrackers`] (one per
/// torrent, kept across pause/unpause and shared by all its lookups).
pub(crate) struct DhtPacing {
    pacer: Arc<LookupPacer>,
    /// The demand seen last and since when.
    state: Arc<Mutex<(PeerDemand, Instant)>>,
    /// The last demand was `Sufficient` (fast path for lost peers).
    sufficient: Arc<AtomicBool>,
}

impl DhtPacing {
    pub(crate) fn new(trackers: Weak<TorrentTrackers>) -> Self {
        let state = Arc::new(Mutex::new((PeerDemand::Unknown, Instant::now())));
        let sufficient = Arc::new(AtomicBool::new(false));
        let pacer = {
            let (state, sufficient) = (state.clone(), sufficient.clone());
            LookupPacer::new(move || {
                let demand = trackers
                    .upgrade()
                    .and_then(|t| t.live_torrent())
                    .map_or(PeerDemand::Unknown, |l| PeerDemand::of_live(&l));
                let held_for = observe(&state, &sufficient, demand, Instant::now());
                jittered(base_interval(demand, held_for), rand::random())
            })
        };
        Self {
            pacer,
            state,
            sufficient,
        }
    }

    pub(crate) fn pacer(&self) -> Arc<LookupPacer> {
        self.pacer.clone()
    }

    /// Re-evaluate the interval of the running lookups now.
    pub(crate) fn wake(&self) {
        self.pacer.wake();
    }

    /// Look up and announce at once (manual reannounce).
    pub(crate) fn force(&self) {
        self.pacer.force();
    }

    /// A peer of a live torrent went away. Wakes the lookups if the torrent
    /// had enough peers and no longer has.
    pub(crate) fn on_peer_lost(&self, live: &TorrentStateLive) {
        self.on_peer_lost_with(|| PeerDemand::of_live(live));
    }

    fn on_peer_lost_with(&self, demand: impl FnOnce() -> PeerDemand) {
        if !self.sufficient.load(Ordering::Relaxed) {
            return;
        }
        let demand = demand();
        if demand == PeerDemand::Short {
            observe(&self.state, &self.sufficient, demand, Instant::now());
            self.wake();
        }
    }
}

/// Record `demand` at `now`; returns how long it has held.
fn observe(
    state: &Mutex<(PeerDemand, Instant)>,
    sufficient: &AtomicBool,
    demand: PeerDemand,
    now: Instant,
) -> Duration {
    let mut g = state.lock();
    if g.0 != demand {
        *g = (demand, now);
    }
    sufficient.store(demand == PeerDemand::Sufficient, Ordering::Relaxed);
    now.saturating_duration_since(g.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn demand_from_peer_counts() {
        use PeerDemand::*;
        assert_eq!(PeerDemand::of(true, 0, 0, Some(100)), Complete);
        assert_eq!(PeerDemand::of(false, 10, 20, Some(100)), Short);
        assert_eq!(PeerDemand::of(false, 40, 60, Some(100)), Sufficient);
        // A low connection limit is satisfied sooner.
        assert_eq!(PeerDemand::of(false, 5, 0, Some(5)), Sufficient);
        // No limit (or a huge one): capped.
        assert_eq!(PeerDemand::of(false, 30, 70, None), Sufficient);
        assert_eq!(PeerDemand::of(false, 30, 70, Some(1000)), Sufficient);
        assert_eq!(PeerDemand::of(false, 30, 69, None), Short);
        // A zero limit never makes "one peer" a requirement it can't meet.
        assert_eq!(PeerDemand::of(false, 0, 1, Some(0)), Sufficient);
    }

    #[test]
    fn policy_table() {
        use PeerDemand::*;
        assert_eq!(base_interval(Unknown, Duration::ZERO), MIN);
        assert_eq!(base_interval(Unknown, 60 * MIN), MIN);
        assert_eq!(base_interval(Complete, Duration::ZERO), 15 * MIN);
        assert_eq!(base_interval(Complete, 60 * MIN), 15 * MIN);
        assert_eq!(base_interval(Short, Duration::ZERO), MIN);
        assert_eq!(base_interval(Short, 9 * MIN), MIN);
        assert_eq!(base_interval(Short, 10 * MIN), 2 * MIN);
        assert_eq!(base_interval(Sufficient, Duration::ZERO), MIN);
        assert_eq!(base_interval(Sufficient, 59 * MIN / 60), MIN);
        assert_eq!(base_interval(Sufficient, MIN), 2 * MIN);
        assert_eq!(base_interval(Sufficient, 3 * MIN), 5 * MIN);
        assert_eq!(base_interval(Sufficient, 8 * MIN), 15 * MIN);
        assert_eq!(base_interval(Sufficient, 600 * MIN), 15 * MIN);
    }

    #[test]
    fn backoff_follows_the_rounds() {
        // Rounds run at the interval the ladder gives: 1, 2, 5, then 15 min.
        let mut t = Duration::ZERO;
        let mut seen = Vec::new();
        for _ in 0..5 {
            let d = base_interval(PeerDemand::Sufficient, t);
            seen.push(d.as_secs() / 60);
            t += d;
        }
        assert_eq!(seen, [1, 2, 5, 15, 15]);
    }

    #[test]
    fn jitter_is_within_ten_percent() {
        let d = 15 * MIN;
        assert_eq!(jittered(d, 0), d * 9 / 10);
        assert_eq!(jittered(d, 100), d);
        assert_eq!(jittered(d, 200), d * 11 / 10);
        assert_eq!(jittered(d, 201), d * 9 / 10);
        for r in [7, 12_345, u32::MAX, u32::MAX / 3] {
            let j = jittered(d, r);
            assert!(j >= d * 9 / 10 && j <= d * 11 / 10, "{r} {j:?}");
        }
    }

    #[test]
    fn observe_tracks_how_long_a_demand_held() {
        let t0 = Instant::now();
        let state = Mutex::new((PeerDemand::Unknown, t0));
        let flag = AtomicBool::new(false);
        assert_eq!(
            observe(&state, &flag, PeerDemand::Sufficient, t0 + MIN),
            Duration::ZERO
        );
        assert!(flag.load(Ordering::Relaxed));
        assert_eq!(
            observe(&state, &flag, PeerDemand::Sufficient, t0 + 4 * MIN),
            3 * MIN
        );
        // Losing peers resets the ladder.
        assert_eq!(
            observe(&state, &flag, PeerDemand::Short, t0 + 5 * MIN),
            Duration::ZERO
        );
        assert!(!flag.load(Ordering::Relaxed));
        assert_eq!(
            observe(&state, &flag, PeerDemand::Sufficient, t0 + 6 * MIN),
            Duration::ZERO
        );
    }

    #[test]
    fn lost_peers_wake_the_lookups() {
        let pacing = DhtPacing::new(Weak::new());
        let p = pacing.pacer();
        let mut rx = p.subscribe();
        rx.borrow_and_update();
        // Not live: the demand is unknown, the interval short.
        let i = p.interval();
        assert!(i >= MIN * 9 / 10 && i <= MIN * 11 / 10, "{i:?}");
        // Already short of peers: losing one more changes nothing.
        pacing.on_peer_lost_with(|| PeerDemand::Short);
        assert!(!rx.has_changed().unwrap());
        // Had enough and still has: no wake.
        observe(&pacing.state, &pacing.sufficient, PeerDemand::Sufficient, Instant::now());
        pacing.on_peer_lost_with(|| PeerDemand::Sufficient);
        assert!(!rx.has_changed().unwrap());
        // Had enough, now short: wake once and leave the fast path.
        pacing.on_peer_lost_with(|| PeerDemand::Short);
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();
        assert!(!pacing.sufficient.load(Ordering::Relaxed));
        pacing.on_peer_lost_with(|| unreachable!("fast path"));
        assert!(!rx.has_changed().unwrap());
    }
}
