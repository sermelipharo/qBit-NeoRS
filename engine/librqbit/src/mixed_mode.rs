//! uTP/TCP mixed mode bandwidth balancing (libtorrent `mixed_mode_algorithm`).
//!
//! When a session has both TCP and uTP peers, the two protocols compete for
//! the same link. libtorrent offers two policies:
//!
//! * [`MixedModeAlgorithm::PreferTcp`]: TCP comes first. libtorrent relies on
//!   uTP's LEDBAT congestion control to back off; this engine's uTP uses CUBIC,
//!   which does not yield, so the same effect is produced with a throttle:
//!   while TCP peers are transferring in a direction and the session rate
//!   limit applies to uTP, uTP peers only get the part of the session limit
//!   TCP peers leave unused (minus a margin TCP can grow into), never below a
//!   small floor. Without a session limit there is no shared budget to divide
//!   and nothing is throttled.
//! * [`MixedModeAlgorithm::PeerProportional`]: libtorrent's algorithm as is:
//!   while uTP peers are transferring in a direction, TCP peers are throttled
//!   to the TCP share of the peers (`rate * tcp_peers / all_peers`) of the
//!   current throughput, never below 5 kB/s up / 30 kB/s down. uTP is not
//!   throttled.
//!
//! The throttles are recomputed by the once-a-second session peer tick from
//! the per-peer rates and apply to peers outside the local network.

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

use crate::limits::Limits;

/// How bandwidth is shared between TCP and uTP peers (qBittorrent
/// `utp_tcp_mixed_mode`: 0 = prefer TCP, 1 = peer proportional).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MixedModeAlgorithm {
    #[default]
    PreferTcp,
    PeerProportional,
}

impl MixedModeAlgorithm {
    pub(crate) fn to_u8(self) -> u8 {
        match self {
            Self::PreferTcp => 0,
            Self::PeerProportional => 1,
        }
    }

    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::PeerProportional,
            _ => Self::PreferTcp,
        }
    }
}

/// Never throttle a protocol below these rates (libtorrent's lower limits).
pub(crate) const MIN_UPLOAD_BPS: u64 = 5_000;
pub(crate) const MIN_DOWNLOAD_BPS: u64 = 30_000;

/// Transfer activity of one direction over the last tick, peers outside the
/// local network only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DirectionActivity {
    /// Peers with a non-zero rate in this direction.
    pub tcp_peers: u32,
    pub utp_peers: u32,
    /// Their summed rates, bytes/s.
    pub tcp_bps: u64,
    pub utp_bps: u64,
}

impl DirectionActivity {
    pub fn add(&mut self, utp: bool, bps: u64) {
        if bps == 0 {
            return;
        }
        if utp {
            self.utp_peers += 1;
            self.utp_bps += bps;
        } else {
            self.tcp_peers += 1;
            self.tcp_bps += bps;
        }
    }
}

/// Per-protocol throttles of one direction (`None` = not throttled).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ProtocolThrottle {
    pub tcp: Option<NonZeroU32>,
    pub utp: Option<NonZeroU32>,
}

fn bps(v: u64) -> Option<NonZeroU32> {
    NonZeroU32::new(u32::try_from(v).unwrap_or(u32::MAX))
}

/// The throttles of one direction.
///
/// `session_limit` is the session rate limit of that direction and
/// `utp_session_limited` whether it applies to uTP peers
/// (`rate_limit_utp`). `floor` is the lowest throttle ([`MIN_UPLOAD_BPS`] /
/// [`MIN_DOWNLOAD_BPS`]).
pub(crate) fn compute_throttle(
    algorithm: MixedModeAlgorithm,
    a: DirectionActivity,
    session_limit: Option<NonZeroU32>,
    utp_session_limited: bool,
    floor: u64,
) -> ProtocolThrottle {
    match algorithm {
        MixedModeAlgorithm::PreferTcp => {
            let Some(limit) = session_limit.filter(|_| utp_session_limited) else {
                return ProtocolThrottle::default();
            };
            if a.tcp_peers == 0 || a.utp_peers == 0 {
                return ProtocolThrottle::default();
            }
            let limit = u64::from(limit.get());
            // What TCP leaves of the budget, minus a quarter of it so TCP
            // can still speed up.
            let spare = limit.saturating_sub(a.tcp_bps);
            let utp = (spare - spare / 4).max(floor.min(limit));
            ProtocolThrottle {
                tcp: None,
                utp: bps(utp),
            }
        }
        MixedModeAlgorithm::PeerProportional => {
            if a.utp_peers == 0 {
                return ProtocolThrottle::default();
            }
            let tcp_peers = u64::from(a.tcp_peers.max(1));
            let total_peers = tcp_peers + u64::from(a.utp_peers);
            let rate = a.tcp_bps + a.utp_bps;
            let tcp = (rate.saturating_mul(tcp_peers) / total_peers).max(floor);
            ProtocolThrottle {
                tcp: bps(tcp),
                utp: None,
            }
        }
    }
}

/// Whether a throttle changed enough to replace the limiter (a new limiter
/// starts with a full burst, so it is not replaced for small changes).
fn significant_change(old: Option<NonZeroU32>, new: Option<NonZeroU32>) -> bool {
    match (old, new) {
        (None, None) => false,
        (Some(o), Some(n)) => {
            let (o, n) = (u64::from(o.get()), u64::from(n.get()));
            o.abs_diff(n) * 8 > o
        }
        _ => true,
    }
}

/// The session's mixed mode state: the algorithm and the per-protocol
/// limiters the peer tick sets.
pub(crate) struct MixedModeLimits {
    algorithm: AtomicU8,
    tcp: Limits,
    utp: Limits,
    // Current throttles (bytes/s, 0 = none): tcp up, tcp down, utp up, utp down.
    current: [AtomicU32; 4],
}

impl MixedModeLimits {
    pub fn new(algorithm: MixedModeAlgorithm) -> Self {
        Self {
            algorithm: AtomicU8::new(algorithm.to_u8()),
            tcp: Limits::new(Default::default()),
            utp: Limits::new(Default::default()),
            current: Default::default(),
        }
    }

    pub fn algorithm(&self) -> MixedModeAlgorithm {
        MixedModeAlgorithm::from_u8(self.algorithm.load(Ordering::Relaxed))
    }

    /// Returns true if the algorithm changed (the throttles are cleared).
    pub fn set_algorithm(&self, algorithm: MixedModeAlgorithm) -> bool {
        let changed =
            self.algorithm.swap(algorithm.to_u8(), Ordering::Relaxed) != algorithm.to_u8();
        if changed {
            self.apply(ProtocolThrottle::default(), ProtocolThrottle::default());
        }
        changed
    }

    /// The limiter for a peer's protocol.
    pub fn limits(&self, utp: bool) -> &Limits {
        if utp { &self.utp } else { &self.tcp }
    }

    /// Current throttles as (upload, download).
    #[cfg(test)]
    pub fn throttles(&self) -> (ProtocolThrottle, ProtocolThrottle) {
        let get = |i: usize| NonZeroU32::new(self.current[i].load(Ordering::Relaxed));
        (
            ProtocolThrottle {
                tcp: get(0),
                utp: get(2),
            },
            ProtocolThrottle {
                tcp: get(1),
                utp: get(3),
            },
        )
    }

    /// Install new throttles, replacing a limiter only on a significant change.
    pub fn apply(&self, up: ProtocolThrottle, down: ProtocolThrottle) {
        let slots: [(usize, Option<NonZeroU32>, &Limits, bool); 4] = [
            (0, up.tcp, &self.tcp, true),
            (1, down.tcp, &self.tcp, false),
            (2, up.utp, &self.utp, true),
            (3, down.utp, &self.utp, false),
        ];
        for (i, new, limits, upload) in slots {
            let old = NonZeroU32::new(self.current[i].load(Ordering::Relaxed));
            if !significant_change(old, new) {
                continue;
            }
            self.current[i].store(new.map_or(0, NonZeroU32::get), Ordering::Relaxed);
            if upload {
                limits.set_upload_bps(new);
            } else {
                limits.set_download_bps(new);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(tcp_peers: u32, tcp_bps: u64, utp_peers: u32, utp_bps: u64) -> DirectionActivity {
        DirectionActivity {
            tcp_peers,
            utp_peers,
            tcp_bps,
            utp_bps,
        }
    }

    fn nz(v: u32) -> Option<NonZeroU32> {
        NonZeroU32::new(v)
    }

    #[test]
    fn activity_counts_only_transferring_peers() {
        let mut a = DirectionActivity::default();
        a.add(false, 0);
        a.add(true, 0);
        a.add(false, 100);
        a.add(true, 50);
        a.add(true, 25);
        assert_eq!(a, act(1, 100, 2, 75));
    }

    #[test]
    fn prefer_tcp_throttles_utp_to_the_budget_tcp_leaves() {
        let alg = MixedModeAlgorithm::PreferTcp;
        let limit = nz(1_000_000);
        // TCP uses 600 kB/s of 1 MB/s: uTP gets 3/4 of the 400 kB/s left.
        let t = compute_throttle(
            alg,
            act(3, 600_000, 2, 400_000),
            limit,
            true,
            MIN_DOWNLOAD_BPS,
        );
        assert_eq!(
            t,
            ProtocolThrottle {
                tcp: None,
                utp: nz(300_000)
            }
        );
        // TCP saturates the limit: uTP keeps the floor.
        let t = compute_throttle(alg, act(3, 1_200_000, 2, 0), limit, true, MIN_DOWNLOAD_BPS);
        assert_eq!(t.utp, nz(30_000));
        // The floor never exceeds the limit itself.
        let t = compute_throttle(
            alg,
            act(1, 20_000, 1, 0),
            nz(10_000),
            true,
            MIN_DOWNLOAD_BPS,
        );
        assert_eq!(t.utp, nz(10_000));
    }

    #[test]
    fn prefer_tcp_needs_both_protocols_and_a_shared_limit() {
        let alg = MixedModeAlgorithm::PreferTcp;
        let none = ProtocolThrottle::default();
        let limit = nz(1_000_000);
        // No TCP peer transferring: uTP free.
        assert_eq!(
            compute_throttle(alg, act(0, 0, 4, 900_000), limit, true, 1),
            none
        );
        // No uTP peer: nothing to throttle.
        assert_eq!(
            compute_throttle(alg, act(4, 900_000, 0, 0), limit, true, 1),
            none
        );
        // No session limit, or uTP exempt from it.
        assert_eq!(compute_throttle(alg, act(1, 1, 1, 1), None, true, 1), none);
        assert_eq!(
            compute_throttle(alg, act(1, 1, 1, 1), limit, false, 1),
            none
        );
    }

    #[test]
    fn peer_proportional_throttles_tcp_to_its_share_of_peers() {
        let alg = MixedModeAlgorithm::PeerProportional;
        // 1 TCP + 3 uTP peers, 800 kB/s in total: TCP gets a quarter.
        let t = compute_throttle(alg, act(1, 500_000, 3, 300_000), None, true, MIN_UPLOAD_BPS);
        assert_eq!(
            t,
            ProtocolThrottle {
                tcp: nz(200_000),
                utp: None
            }
        );
        // The session limit plays no part, the floor does.
        let t = compute_throttle(alg, act(1, 1_000, 9, 9_000), nz(5), true, MIN_UPLOAD_BPS);
        assert_eq!(t.tcp, nz(5_000));
        // No active TCP peer counts as one (libtorrent).
        let t = compute_throttle(alg, act(0, 0, 1, 100_000), None, true, 1);
        assert_eq!(t.tcp, nz(50_000));
        // No uTP peer: TCP free.
        assert_eq!(
            compute_throttle(alg, act(5, 100_000, 0, 0), None, true, 1),
            ProtocolThrottle::default()
        );
    }

    #[test]
    fn limiters_change_only_on_significant_changes() {
        let m = MixedModeLimits::new(MixedModeAlgorithm::PeerProportional);
        assert_eq!(m.algorithm(), MixedModeAlgorithm::PeerProportional);
        let up = ProtocolThrottle {
            tcp: nz(80_000),
            utp: None,
        };
        m.apply(up, ProtocolThrottle::default());
        assert_eq!(m.limits(false).get_upload_bps(), nz(80_000));
        assert_eq!(m.limits(true).get_upload_bps(), None);
        // Within 1/8: kept.
        m.apply(
            ProtocolThrottle {
                tcp: nz(85_000),
                utp: None,
            },
            Default::default(),
        );
        assert_eq!(m.limits(false).get_upload_bps(), nz(80_000));
        // Beyond: replaced.
        m.apply(
            ProtocolThrottle {
                tcp: nz(100_000),
                utp: None,
            },
            Default::default(),
        );
        assert_eq!(m.limits(false).get_upload_bps(), nz(100_000));
        assert_eq!(m.throttles().0.tcp, nz(100_000));
        // Switching the algorithm clears every throttle.
        assert!(m.set_algorithm(MixedModeAlgorithm::PreferTcp));
        assert!(!m.set_algorithm(MixedModeAlgorithm::PreferTcp));
        assert_eq!(m.limits(false).get_upload_bps(), None);
        assert_eq!(m.throttles(), Default::default());
    }
}
