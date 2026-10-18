//! Session-wide peer policy: upload slots and choking, the global connection
//! limit, rate limit exemptions, the once-a-second peer tick (rates, rechoke,
//! optimistic unchoke) and transfer statistics.

use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tokio::{sync::Notify, time::MissedTickBehavior};
use tracing::{debug_span, trace};

use crate::{
    Session,
    conn_limiter::ConnLimiter,
    limits::LimitsConfig,
    mixed_mode::{
        self, DirectionActivity, MIN_DOWNLOAD_BPS, MIN_UPLOAD_BPS, MixedModeAlgorithm,
        MixedModeLimits,
    },
    torrent_state::{
        TorrentStateLive,
        live::choker::{
            self, ChokerConfig, ChokingAlgorithm, OPTIMISTIC_UNCHOKE_INTERVAL, OptimisticDecision,
            RECHOKE_INTERVAL, RechokeAction, SeedChokingAlgorithm,
        },
    },
};

const NONE_U32: u32 = u32::MAX;

fn opt_u32(v: u32) -> Option<u32> {
    (v != NONE_U32).then_some(v)
}

/// Session-wide peer policy. Every field can also be changed at runtime with
/// the `Session::set_*` methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerPolicyOptions {
    /// Upload slots (unchoked peers) across all torrents, `None` = unlimited
    /// (qBittorrent `max_uploads`). Used by [`ChokingAlgorithm::FixedSlots`].
    pub max_uploads: Option<u32>,
    /// Upload slots of a newly added torrent, `None` = unlimited
    /// (qBittorrent `max_uploads_per_torrent`). Change existing torrents with
    /// `ManagedTorrent::set_max_uploads`.
    pub max_uploads_per_torrent: Option<u32>,
    pub choking_algorithm: ChokingAlgorithm,
    pub seed_choking_algorithm: SeedChokingAlgorithm,
    /// Peer connections across all torrents, `None` = unlimited
    /// (qBittorrent `max_connec`).
    pub max_connections: Option<u32>,
    /// Apply the session rate limits to uTP peers (qBittorrent `limit_utp_rate`).
    pub rate_limit_utp: bool,
    /// Apply the session rate limits to peers on the local network
    /// (qBittorrent `limit_lan_peers`).
    pub rate_limit_lan: bool,
    /// Bandwidth balancing between TCP and uTP peers (qBittorrent
    /// `utp_tcp_mixed_mode`).
    #[serde(default)]
    pub mixed_mode: MixedModeAlgorithm,
}

impl Default for PeerPolicyOptions {
    fn default() -> Self {
        Self {
            max_uploads: None,
            max_uploads_per_torrent: None,
            choking_algorithm: ChokingAlgorithm::FixedSlots,
            seed_choking_algorithm: SeedChokingAlgorithm::FastestUpload,
            max_connections: None,
            rate_limit_utp: true,
            rate_limit_lan: true,
            mixed_mode: MixedModeAlgorithm::PreferTcp,
        }
    }
}

pub(crate) struct SessionPeerPolicy {
    max_uploads: AtomicU32,
    max_uploads_per_torrent: AtomicU32,
    choking_algorithm: AtomicU8,
    seed_choking_algorithm: AtomicU8,
    rate_limit_utp: AtomicBool,
    rate_limit_lan: AtomicBool,
    /// uTP/TCP mixed mode throttles (set by the peer tick).
    pub mixed: MixedModeLimits,
    /// The global connection limit.
    pub connections: ConnLimiter,
    // Upload slots allowed by the last rechoke (rate-based mode computes them).
    allowed_slots: AtomicU32,
    pub rechoke_notify: Arc<Notify>,
}

impl SessionPeerPolicy {
    pub fn new(opts: PeerPolicyOptions) -> Self {
        Self {
            max_uploads: AtomicU32::new(opts.max_uploads.unwrap_or(NONE_U32)),
            max_uploads_per_torrent: AtomicU32::new(
                opts.max_uploads_per_torrent.unwrap_or(NONE_U32),
            ),
            choking_algorithm: AtomicU8::new(opts.choking_algorithm.to_u8()),
            seed_choking_algorithm: AtomicU8::new(opts.seed_choking_algorithm.to_u8()),
            rate_limit_utp: AtomicBool::new(opts.rate_limit_utp),
            rate_limit_lan: AtomicBool::new(opts.rate_limit_lan),
            mixed: MixedModeLimits::new(opts.mixed_mode),
            connections: ConnLimiter::new(
                opts.max_connections
                    .map(|v| usize::try_from(v).unwrap_or(usize::MAX)),
            ),
            allowed_slots: AtomicU32::new(match opts.choking_algorithm {
                ChokingAlgorithm::FixedSlots => opts.max_uploads.unwrap_or(NONE_U32),
                ChokingAlgorithm::RateBased => 1,
            }),
            rechoke_notify: Default::default(),
        }
    }

    pub fn options(&self) -> PeerPolicyOptions {
        PeerPolicyOptions {
            max_uploads: opt_u32(self.max_uploads.load(Ordering::Relaxed)),
            max_uploads_per_torrent: opt_u32(self.max_uploads_per_torrent.load(Ordering::Relaxed)),
            choking_algorithm: ChokingAlgorithm::from_u8(
                self.choking_algorithm.load(Ordering::Relaxed),
            ),
            seed_choking_algorithm: SeedChokingAlgorithm::from_u8(
                self.seed_choking_algorithm.load(Ordering::Relaxed),
            ),
            max_connections: self
                .connections
                .limit()
                .map(|v| u32::try_from(v).unwrap_or(u32::MAX)),
            rate_limit_utp: self.rate_limit_utp.load(Ordering::Relaxed),
            rate_limit_lan: self.rate_limit_lan.load(Ordering::Relaxed),
            mixed_mode: self.mixed.algorithm(),
        }
    }

    pub fn default_max_uploads_per_torrent(&self) -> Option<u32> {
        opt_u32(self.max_uploads_per_torrent.load(Ordering::Relaxed))
    }

    fn choker_config(&self) -> ChokerConfig {
        let opts = self.options();
        ChokerConfig {
            global_slots: opts
                .max_uploads
                .map(|v| usize::try_from(v).unwrap_or(usize::MAX)),
            algorithm: opts.choking_algorithm,
            seed_algorithm: opts.seed_choking_algorithm,
        }
    }

    pub(crate) fn allowed_slots(&self) -> Option<usize> {
        opt_u32(self.allowed_slots.load(Ordering::Relaxed))
            .map(|v| usize::try_from(v).unwrap_or(usize::MAX))
    }

    fn set_allowed_slots(&self, slots: Option<usize>) {
        let v = slots.map_or(NONE_U32, |s| u32::try_from(s).unwrap_or(NONE_U32 - 1));
        self.allowed_slots.store(v, Ordering::Relaxed);
    }

    /// Whether one more peer may be unchoked right away.
    pub fn has_free_upload_slot(&self, unchoked: usize) -> bool {
        self.allowed_slots().is_none_or(|s| unchoked < s)
    }

    /// Whether the session rate limits apply to a peer.
    pub fn session_limits_apply(&self, utp: bool, lan: bool) -> bool {
        (!utp || self.rate_limit_utp.load(Ordering::Relaxed))
            && (!lan || self.rate_limit_lan.load(Ordering::Relaxed))
    }

    pub(crate) fn request_rechoke(&self) {
        self.rechoke_notify.notify_one();
    }
}

/// Whether an address is on the local network (loopback, private, link-local
/// or unique-local).
pub(crate) fn is_lan_addr(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(a) => a.is_loopback() || a.is_private() || a.is_link_local(),
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return v4.is_loopback() || v4.is_private() || v4.is_link_local();
            }
            a.is_loopback() || a.is_unique_local() || a.is_unicast_link_local()
        }
    }
}

/// Estimated network header bytes for sending/receiving `bytes` of protocol
/// data in full-size packets (libtorrent's estimate: 1500 byte MTU, IP + TCP
/// headers, or IP + UDP + uTP headers).
pub(crate) fn estimate_ip_overhead(bytes: u64, ipv6: bool, utp: bool) -> u64 {
    if bytes == 0 {
        return 0;
    }
    let ip = if ipv6 { 40 } else { 20 };
    let header: u64 = if utp { ip + 8 + 20 } else { ip + 20 };
    let payload_per_packet = 1500 - header;
    bytes.div_ceil(payload_per_packet) * header
}

/// Session-wide transfer statistics, cumulative since the session started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct SessionTransferStats {
    /// Piece data received from peers.
    pub payload_downloaded: u64,
    /// Piece data sent to peers.
    pub payload_uploaded: u64,
    /// All BitTorrent protocol bytes received (payload plus handshakes, message
    /// framing and other messages).
    pub protocol_downloaded: u64,
    /// All BitTorrent protocol bytes sent.
    pub protocol_uploaded: u64,
    /// Estimated TCP/IP (or UDP/uTP) header bytes for the protocol traffic.
    pub ip_overhead_downloaded: u64,
    pub ip_overhead_uploaded: u64,
    /// `wasted_failed_bytes + wasted_redundant_bytes`.
    pub wasted_bytes: u64,
    /// Pieces that failed the hash check.
    pub wasted_failed_bytes: u64,
    /// Blocks received that were not needed (late, duplicate or stolen).
    pub wasted_redundant_bytes: u64,
    /// Current payload rates, bytes/s.
    pub payload_download_rate: u64,
    pub payload_upload_rate: u64,
    /// Connected (handshaken) peers across all torrents.
    pub connected_peers: u32,
    /// Peers we are uploading to (unchoked).
    pub unchoked_peers: u32,
    /// Nodes in the DHT routing tables (0 without DHT).
    pub dht_nodes: usize,
}

impl Session {
    /// The current peer policy.
    pub fn peer_policy(&self) -> PeerPolicyOptions {
        self.peer_policy.options()
    }

    /// Apply a whole peer policy at once.
    pub fn set_peer_policy(&self, opts: PeerPolicyOptions) {
        self.set_max_uploads(opts.max_uploads);
        self.set_default_max_uploads_per_torrent(opts.max_uploads_per_torrent);
        self.set_choking_algorithm(opts.choking_algorithm);
        self.set_seed_choking_algorithm(opts.seed_choking_algorithm);
        self.set_max_connections(opts.max_connections);
        self.set_rate_limit_utp(opts.rate_limit_utp);
        self.set_rate_limit_lan(opts.rate_limit_lan);
        self.set_mixed_mode_algorithm(opts.mixed_mode);
    }

    /// Session-wide upload slots (`None` = unlimited). Takes effect at once.
    pub fn set_max_uploads(&self, max_uploads: Option<u32>) {
        let p = &self.peer_policy;
        let v = max_uploads.unwrap_or(NONE_U32);
        if p.max_uploads.swap(v, Ordering::Relaxed) != v {
            if p.options().choking_algorithm == ChokingAlgorithm::FixedSlots {
                p.allowed_slots.store(v, Ordering::Relaxed);
            }
            p.request_rechoke();
        }
    }

    /// Upload slots given to torrents added from now on.
    pub fn set_default_max_uploads_per_torrent(&self, max_uploads: Option<u32>) {
        self.peer_policy
            .max_uploads_per_torrent
            .store(max_uploads.unwrap_or(NONE_U32), Ordering::Relaxed);
    }

    pub fn set_choking_algorithm(&self, algorithm: ChokingAlgorithm) {
        let p = &self.peer_policy;
        if p.choking_algorithm
            .swap(algorithm.to_u8(), Ordering::Relaxed)
            != algorithm.to_u8()
        {
            if algorithm == ChokingAlgorithm::FixedSlots {
                p.allowed_slots
                    .store(p.max_uploads.load(Ordering::Relaxed), Ordering::Relaxed);
            }
            p.request_rechoke();
        }
    }

    pub fn set_seed_choking_algorithm(&self, algorithm: SeedChokingAlgorithm) {
        let p = &self.peer_policy;
        if p.seed_choking_algorithm
            .swap(algorithm.to_u8(), Ordering::Relaxed)
            != algorithm.to_u8()
        {
            p.request_rechoke();
        }
    }

    /// Global peer connection limit across all torrents (`None` =
    /// unlimited). Lowering it below the current number of connections
    /// refuses new ones until enough have closed.
    pub fn set_max_connections(&self, max_connections: Option<u32>) {
        self.peer_policy
            .connections
            .set_limit(max_connections.map(|v| usize::try_from(v).unwrap_or(usize::MAX)));
    }

    /// Whether the session rate limits apply to uTP peers.
    pub fn set_rate_limit_utp(&self, apply: bool) {
        self.peer_policy
            .rate_limit_utp
            .store(apply, Ordering::Relaxed);
    }

    /// Whether the session rate limits apply to peers on the local network.
    pub fn set_rate_limit_lan(&self, apply: bool) {
        self.peer_policy
            .rate_limit_lan
            .store(apply, Ordering::Relaxed);
    }

    /// How bandwidth is shared between TCP and uTP peers. Takes effect at the
    /// next peer tick (within a second).
    pub fn set_mixed_mode_algorithm(&self, algorithm: MixedModeAlgorithm) {
        self.peer_policy.mixed.set_algorithm(algorithm);
    }

    /// Recompute the uTP/TCP mixed mode throttles from the last tick's
    /// activity.
    fn update_mixed_mode(&self, up: DirectionActivity, down: DirectionActivity) {
        let p = &self.peer_policy;
        let alg = p.mixed.algorithm();
        let utp_limited = p.rate_limit_utp.load(Ordering::Relaxed);
        let limits = self.ratelimits.get_config();
        let up =
            mixed_mode::compute_throttle(alg, up, limits.upload_bps, utp_limited, MIN_UPLOAD_BPS);
        let down = mixed_mode::compute_throttle(
            alg,
            down,
            limits.download_bps,
            utp_limited,
            MIN_DOWNLOAD_BPS,
        );
        p.mixed.apply(up, down);
    }

    /// Session-wide rate limits (bytes/s, `None` = unlimited). Takes effect at once.
    pub fn set_ratelimits(&self, limits: LimitsConfig) {
        self.ratelimits.set_upload_bps(limits.upload_bps);
        self.ratelimits.set_download_bps(limits.download_bps);
    }

    pub fn get_ratelimits(&self) -> LimitsConfig {
        self.ratelimits.get_config()
    }

    /// Transfer statistics of the whole session.
    pub fn transfer_stats(&self) -> SessionTransferStats {
        let c = &self.stats.counters;
        let load = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
        let failed = load(&c.wasted_failed_bytes);
        let redundant = load(&c.wasted_redundant_bytes);
        let peers = self.stats.peers.snapshot();
        SessionTransferStats {
            payload_downloaded: load(&c.fetched_bytes),
            payload_uploaded: load(&c.uploaded_bytes),
            protocol_downloaded: load(&c.protocol_received_bytes),
            protocol_uploaded: load(&c.protocol_sent_bytes),
            ip_overhead_downloaded: load(&c.ip_overhead_received_bytes),
            ip_overhead_uploaded: load(&c.ip_overhead_sent_bytes),
            wasted_bytes: failed + redundant,
            wasted_failed_bytes: failed,
            wasted_redundant_bytes: redundant,
            payload_download_rate: self.stats.down_speed_estimator.bps(),
            payload_upload_rate: self.stats.up_speed_estimator.bps(),
            connected_peers: peers.live,
            unchoked_peers: peers.unchoked,
            dht_nodes: self
                .get_dht()
                .map(|d| {
                    let s = d.stats();
                    s.routing_table_size + s.routing_table_size_v6
                })
                .unwrap_or(0),
        }
    }

    fn live_torrents(&self) -> Vec<Arc<TorrentStateLive>> {
        self.with_torrents(|torrents| torrents.filter_map(|(_, t)| t.live()).collect())
    }

    fn rechoke_all(&self, lives: &[Arc<TorrentStateLive>], round: Duration, now: Instant) {
        let torrents: Vec<_> = lives.iter().map(|l| l.choke_info()).collect();
        let mut collected = Vec::new();
        let mut owners = Vec::new();
        for (i, l) in lives.iter().enumerate() {
            let before = collected.len();
            l.collect_choke_candidates(i, true, now, &mut collected);
            owners.extend(std::iter::repeat_n(i, collected.len() - before));
        }
        let cands: Vec<_> = collected.iter().map(|(c, _)| c.clone()).collect();
        let (actions, slots) = choker::rechoke_round(
            &self.peer_policy.choker_config(),
            &torrents,
            &cands,
            round,
            now,
        );
        self.peer_policy.set_allowed_slots(slots);
        trace!(candidates = cands.len(), ?slots, "rechoke");
        // Choke first so that slots are free before new unchokes.
        for choke_pass in [true, false] {
            for ((a, (_, addr)), t) in actions.iter().zip(&collected).zip(&owners) {
                let live = &lives[*t];
                match (a, choke_pass) {
                    (RechokeAction::Choke, true) => live.apply_choke(*addr, true, false, now),
                    (RechokeAction::Unchoke, false) => live.apply_choke(*addr, false, false, now),
                    (RechokeAction::UnchokeOptimistic, false) => {
                        live.apply_choke(*addr, false, true, now)
                    }
                    _ => {}
                }
            }
        }
    }

    fn optimistic_unchoke_all(&self, lives: &[Arc<TorrentStateLive>], now: Instant) {
        let torrents: Vec<_> = lives.iter().map(|l| l.choke_info()).collect();
        let mut collected = Vec::new();
        let mut owners = Vec::new();
        for (i, l) in lives.iter().enumerate() {
            let before = collected.len();
            l.collect_choke_candidates(i, false, now, &mut collected);
            owners.extend(std::iter::repeat_n(i, collected.len() - before));
        }
        let cands: Vec<_> = collected.iter().map(|(c, _)| c.clone()).collect();
        let decisions =
            choker::rotate_optimistic(self.peer_policy.allowed_slots(), &torrents, &cands);
        for ((d, (_, addr)), t) in decisions.iter().zip(&collected).zip(&owners) {
            match d {
                OptimisticDecision::Choke => lives[*t].apply_choke(*addr, true, false, now),
                OptimisticDecision::Unchoke => lives[*t].apply_choke(*addr, false, true, now),
                OptimisticDecision::Keep | OptimisticDecision::Leave => {}
            }
        }
    }

    /// Spawns the once-a-second session peer tick: session and idle torrent
    /// speed samples, per-peer rates, the rechoke every 10 s (or right after a
    /// policy change) and the optimistic unchoke rotation every 30 s.
    ///
    /// With no live peer and no data moving anywhere (every speed reads zero)
    /// the tick parks without a timer until a peer goes live, a web seed
    /// delivers data or the policy changes, so an idle session does not wake
    /// up once a second.
    pub(crate) fn start_peer_tick(self: &Arc<Self>) {
        let notify = self.peer_policy.rechoke_notify.clone();
        let wake = self.stats.tick_wake.clone();
        let weak = Arc::downgrade(self);
        self.spawn(
            debug_span!(parent: self.rs(), "peer_tick"),
            "peer_tick",
            async move {
                let mut interval = tokio::time::interval(Duration::from_secs(1));
                interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
                // Ticks may come a little early; don't skip a round because of it.
                let slack = Duration::from_millis(500);
                let mut last_tick = Instant::now();
                let mut last_rechoke: Option<Instant> = None;
                let mut last_optimistic: Option<Instant> = None;
                let mut parked = false;
                loop {
                    let forced = if parked {
                        tokio::select! {
                            _ = wake.notified() => {
                                parked = false;
                                let Some(session) = weak.upgrade() else {
                                    return Ok(());
                                };
                                let now = Instant::now();
                                session.rebase_session_speed(now);
                                for l in session.live_torrents() {
                                    l.estimator_rebase(now);
                                }
                                last_tick = now;
                                interval.reset();
                                continue;
                            }
                            _ = notify.notified() => true,
                        }
                    } else {
                        tokio::select! {
                            _ = interval.tick() => false,
                            _ = notify.notified() => true,
                        }
                    };
                    let Some(session) = weak.upgrade() else {
                        return Ok(());
                    };
                    let now = Instant::now();
                    let lives = session.live_torrents();
                    if !forced {
                        session.sample_session_speed(now);
                        let elapsed = now.saturating_duration_since(last_tick);
                        last_tick = now;
                        let mut up = DirectionActivity::default();
                        let mut down = DirectionActivity::default();
                        for l in &lives {
                            l.update_peer_rates(elapsed, &mut up, &mut down);
                            l.estimator_idle_tick(now);
                        }
                        session.update_mixed_mode(up, down);
                    }
                    let due = |last: Option<Instant>, every: Duration| {
                        last.is_none_or(|t| now.saturating_duration_since(t) + slack >= every)
                    };
                    if forced || due(last_rechoke, RECHOKE_INTERVAL) {
                        let round = last_rechoke
                            .map(|t| now.saturating_duration_since(t))
                            .unwrap_or(RECHOKE_INTERVAL);
                        session.rechoke_all(&lives, round, now);
                        last_rechoke = Some(now);
                    }
                    if due(last_optimistic, OPTIMISTIC_UNCHOKE_INTERVAL) {
                        session.optimistic_unchoke_all(&lives, now);
                        last_optimistic = Some(now);
                    }
                    if !forced {
                        // Not parked here: a parked tick only runs forced rounds.
                        parked = session.peer_tick_can_park(&lives);
                    }
                }
            },
        );
    }

    /// No live peer, and the session and every live torrent read zero speed
    /// with their own samplers parked: the peer tick has nothing to do.
    fn peer_tick_can_park(&self, lives: &[Arc<TorrentStateLive>]) -> bool {
        self.stats.peers.live.load(Ordering::Relaxed) == 0
            && self.stats.speed_is_zero()
            && lives.iter().all(|l| l.estimator_settled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lan_addresses() {
        let lan = |s: &str| is_lan_addr(&s.parse().unwrap());
        assert!(lan("127.0.0.1:1"));
        assert!(lan("192.168.1.10:1"));
        assert!(lan("10.0.0.1:1"));
        assert!(lan("169.254.0.1:1"));
        assert!(lan("[::1]:1"));
        assert!(lan("[fd00::1]:1"));
        assert!(lan("[fe80::1]:1"));
        assert!(lan("[::ffff:192.168.0.1]:1"));
        assert!(!lan("8.8.8.8:1"));
        assert!(!lan("[2001:4860::8888]:1"));
    }

    #[test]
    fn ip_overhead_estimate() {
        assert_eq!(estimate_ip_overhead(0, false, false), 0);
        assert_eq!(estimate_ip_overhead(1, false, false), 40);
        assert_eq!(estimate_ip_overhead(1460, false, false), 40);
        assert_eq!(estimate_ip_overhead(1461, false, false), 80);
        assert_eq!(estimate_ip_overhead(16384, true, false), 12 * 60);
        assert_eq!(estimate_ip_overhead(1452, false, true), 48);
    }

    #[test]
    fn policy_options_round_trip_and_exemptions() {
        let opts = PeerPolicyOptions {
            max_uploads: Some(20),
            max_uploads_per_torrent: Some(4),
            choking_algorithm: ChokingAlgorithm::RateBased,
            seed_choking_algorithm: SeedChokingAlgorithm::AntiLeech,
            max_connections: Some(500),
            rate_limit_utp: false,
            rate_limit_lan: true,
            mixed_mode: MixedModeAlgorithm::PeerProportional,
        };
        let p = SessionPeerPolicy::new(opts);
        assert_eq!(p.options(), opts);
        assert_eq!(p.default_max_uploads_per_torrent(), Some(4));
        assert!(p.session_limits_apply(false, false));
        assert!(!p.session_limits_apply(true, false));
        assert!(p.session_limits_apply(false, true));
        // Rate-based starts with one slot until the first rechoke.
        assert!(p.has_free_upload_slot(0));
        assert!(!p.has_free_upload_slot(1));

        let p = SessionPeerPolicy::new(PeerPolicyOptions::default());
        assert_eq!(p.options(), PeerPolicyOptions::default());
        assert!(p.has_free_upload_slot(100_000));
    }
}
