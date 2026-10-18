use std::{
    sync::{Arc, atomic::Ordering},
    time::Instant,
};

use librqbit_core::speed_estimator::SpeedEstimator;
use tokio::sync::Notify;
use snapshot::SessionStatsSnapshot;

use crate::{Session, torrent_state::peers::stats::AggregatePeerStatsAtomic};

pub mod snapshot;

gen_stats!(SessionCountersAtomic SessionCountersSnapshot, [
    fetched_bytes u64,
    uploaded_bytes u64,
    blocked_incoming u64,
    blocked_outgoing u64,
    protocol_received_bytes u64,
    protocol_sent_bytes u64,
    ip_overhead_received_bytes u64,
    ip_overhead_sent_bytes u64,
    wasted_failed_bytes u64,
    wasted_redundant_bytes u64
], []);

pub struct SessionStats {
    pub counters: SessionCountersAtomic,
    pub peers: Arc<AggregatePeerStatsAtomic>,
    pub down_speed_estimator: SpeedEstimator,
    pub up_speed_estimator: SpeedEstimator,
    pub startup_time: Instant,
    /// Wakes the session peer tick while it is parked (no live peers and no
    /// data moving): a peer went live or a web seed delivered data.
    pub(crate) tick_wake: Arc<Notify>,
}

impl SessionStats {
    pub fn new() -> Self {
        SessionStats {
            counters: Default::default(),
            peers: Default::default(),
            down_speed_estimator: SpeedEstimator::new(5),
            up_speed_estimator: SpeedEstimator::new(5),
            startup_time: Instant::now(),
            tick_wake: Default::default(),
        }
    }
}

impl SessionStats {
    /// Wakes the session peer tick if it is parked (a no-op otherwise, apart
    /// from one spare tick later).
    pub(crate) fn wake_peer_tick(&self) {
        self.tick_wake.notify_one();
    }

    /// Whether both session speed estimators read zero.
    pub(crate) fn speed_is_zero(&self) -> bool {
        self.down_speed_estimator.bps() == 0 && self.up_speed_estimator.bps() == 0
    }
}

impl Default for SessionStats {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// One 1 s sample of the session speed estimators. Called by the
    /// session peer tick, so the session has a single 1 s timer.
    pub(crate) fn sample_session_speed(&self, now: Instant) {
        let fetched = self.stats.counters.fetched_bytes.load(Ordering::Relaxed);
        let uploaded = self.stats.counters.uploaded_bytes.load(Ordering::Relaxed);
        self.stats
            .down_speed_estimator
            .add_snapshot(fetched, None, now);
        self.stats.up_speed_estimator.add_snapshot(uploaded, None, now);
    }

    /// Starts fresh speed windows after the peer tick was parked.
    pub(crate) fn rebase_session_speed(&self, now: Instant) {
        self.stats.down_speed_estimator.reset();
        self.stats.up_speed_estimator.reset();
        self.sample_session_speed(now);
    }

    pub fn stats_snapshot(&self) -> SessionStatsSnapshot {
        SessionStatsSnapshot::from((&*self.stats, self.connector.stats().snapshot()))
    }
}
