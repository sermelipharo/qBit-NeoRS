//! Upload slot selection (choking), modelled on libtorrent's choker.
//!
//! Every [`RECHOKE_INTERVAL`] the session ranks all interested peers of all
//! live torrents and unchokes the best ones up to the upload slot limits;
//! every [`OPTIMISTIC_UNCHOKE_INTERVAL`] it rotates the optimistic unchoke
//! slots among the peers that have waited the longest.
//!
//! Ranking (libtorrent `unchoke_sort`): peers that sent us the most in the
//! last round first (tit-for-tat). Ties, which is everything when seeding,
//! are broken by the seed choking algorithm:
//! * round-robin: keep the current set, but rotate out a peer once it got
//!   `piece_length * SEEDING_PIECE_QUOTA` bytes and was unchoked for more than
//!   a minute, then prefer faster uploads, then the longest wait;
//! * fastest upload: the peers we upload to fastest, then the longest wait;
//! * anti-leech: peers that just started or are about to finish first.
//!
//! The upload slot count is either fixed (session `max_uploads`) or
//! rate-based (libtorrent `rate_based_choker`: one slot per peer whose upload
//! rate beats a threshold that starts at 1 KiB/s and grows by 2 KiB/s per
//! slot, plus one). Per-torrent `max_uploads` limits are applied on top.

use std::{
    cmp::Ordering,
    sync::atomic::Ordering as AtomicOrdering,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::stream_connect::ConnectionKind;
use crate::type_aliases::PeerHandle;

use super::{TorrentStateLive, availability::count_ones};

/// How often regular unchoke slots are recalculated.
pub const RECHOKE_INTERVAL: Duration = Duration::from_secs(10);
/// How often optimistic unchoke slots are rotated.
pub const OPTIMISTIC_UNCHOKE_INTERVAL: Duration = Duration::from_secs(30);
/// Round-robin: pieces a peer may get before its slot rotates (libtorrent
/// `seeding_piece_quota`).
/// A peer unchoked within this time is not unchoked again preemptively
/// (it waits for a rechoke round): otherwise a peer toggling interested /
/// not interested would make us queue an unchoke / choke pair per 10 bytes
/// it sends, without bound while it does not read.
pub const PREEMPTIVE_UNCHOKE_MIN_INTERVAL: Duration = RECHOKE_INTERVAL;

pub(crate) fn preemptive_unchoke_allowed(last_unchoke: Option<Instant>, now: Instant) -> bool {
    last_unchoke.is_none_or(|t| now.saturating_duration_since(t) >= PREEMPTIVE_UNCHOKE_MIN_INTERVAL)
}

pub const SEEDING_PIECE_QUOTA: u64 = 20;
const RATE_CHOKER_INITIAL_THRESHOLD: u64 = 1024;
const RATE_CHOKER_THRESHOLD_STEP: u64 = 2048;
const ROUND_ROBIN_MIN_UNCHOKE_TIME: Duration = Duration::from_secs(60);

/// How the number of upload slots is determined
/// (qBittorrent "Upload slots behavior").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChokingAlgorithm {
    /// A fixed number of slots: the session `max_uploads`.
    #[default]
    FixedSlots,
    /// Open slots while the upload rate keeps growing.
    RateBased,
}

/// How peers are ranked when they don't upload to us, i.e. when seeding
/// (qBittorrent "Upload choking algorithm").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeedChokingAlgorithm {
    RoundRobin,
    #[default]
    FastestUpload,
    AntiLeech,
}

impl ChokingAlgorithm {
    pub(crate) fn to_u8(self) -> u8 {
        match self {
            Self::FixedSlots => 0,
            Self::RateBased => 1,
        }
    }

    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::RateBased,
            _ => Self::FixedSlots,
        }
    }
}

impl SeedChokingAlgorithm {
    pub(crate) fn to_u8(self) -> u8 {
        match self {
            Self::RoundRobin => 0,
            Self::FastestUpload => 1,
            Self::AntiLeech => 2,
        }
    }

    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::RoundRobin,
            2 => Self::AntiLeech,
            _ => Self::FastestUpload,
        }
    }
}

/// One interested peer, as seen by the choker.
#[derive(Debug, Clone)]
pub(crate) struct ChokeCandidate {
    /// Index into the torrents slice.
    pub torrent: usize,
    pub choked: bool,
    pub optimistic: bool,
    pub downloaded_last_round: u64,
    pub uploaded_last_round: u64,
    pub uploaded_since_unchoke: u64,
    pub last_unchoke: Option<Instant>,
    pub last_optimistic: Option<Instant>,
    /// libtorrent's anti-leech score (0..=1000).
    pub anti_leech_score: u32,
}

/// Per-torrent inputs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TorrentChokeInfo {
    pub max_uploads: Option<usize>,
    pub piece_length: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ChokerConfig {
    /// Session-wide upload slots (`None` = unlimited); used by fixed slots.
    pub global_slots: Option<usize>,
    pub algorithm: ChokingAlgorithm,
    pub seed_algorithm: SeedChokingAlgorithm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChokeDecision {
    /// Regular unchoke (an optimistic peer promoted to a regular slot loses
    /// the optimistic flag).
    Unchoke,
    Choke,
    /// Leave an optimistic unchoke alone until the next rotation.
    KeepOptimistic,
}

/// libtorrent's anti-leech score: high for peers that have little or almost
/// everything, low for peers half-way.
pub(crate) fn anti_leech_score(
    total_size: u64,
    piece_length: u64,
    have_pieces: u64,
    uploaded_to_peer: u64,
) -> u32 {
    if total_size == 0 {
        return 0;
    }
    let have = uploaded_to_peer.max(piece_length.saturating_mul(have_pieces));
    let have = have.min(total_size) as i128;
    let total = total_size as i128;
    let score = ((have - total / 2) * 2000 / total).unsigned_abs();
    #[allow(clippy::cast_possible_truncation)]
    let score = score.min(1000) as u32;
    score
}

fn rate_based_slots(cands: &[ChokeCandidate], round: Duration) -> usize {
    let ms = u64::try_from(round.as_millis()).unwrap_or(u64::MAX).max(1);
    let mut rates: Vec<u64> = cands
        .iter()
        .map(|c| c.uploaded_last_round.saturating_mul(1000) / ms)
        .collect();
    rates.sort_unstable_by(|a, b| b.cmp(a));
    let mut slots = 0;
    let mut threshold = RATE_CHOKER_INITIAL_THRESHOLD;
    for rate in rates {
        if rate < threshold {
            break;
        }
        slots += 1;
        threshold += RATE_CHOKER_THRESHOLD_STEP;
    }
    slots + 1
}

fn compare(
    a: &ChokeCandidate,
    b: &ChokeCandidate,
    cfg: &ChokerConfig,
    torrents: &[TorrentChokeInfo],
    now: Instant,
) -> Ordering {
    // Tit-for-tat: whoever gave us the most goes first.
    let ord = b.downloaded_last_round.cmp(&a.downloaded_last_round);
    if ord != Ordering::Equal {
        return ord;
    }
    let waited_longest = || a.last_unchoke.cmp(&b.last_unchoke);
    match cfg.seed_algorithm {
        SeedChokingAlgorithm::RoundRobin => {
            let quota_complete = |c: &ChokeCandidate| {
                let quota = torrents
                    .get(c.torrent)
                    .map(|t| t.piece_length)
                    .unwrap_or_default()
                    .saturating_mul(SEEDING_PIECE_QUOTA);
                !c.choked
                    && c.uploaded_since_unchoke > quota
                    && c.last_unchoke.is_some_and(|t| {
                        now.saturating_duration_since(t) > ROUND_ROBIN_MIN_UNCHOKE_TIME
                    })
            };
            let up = |c: &ChokeCandidate| if c.choked { 0 } else { c.uploaded_last_round };
            quota_complete(a)
                .cmp(&quota_complete(b))
                .then_with(|| up(b).cmp(&up(a)))
                .then_with(waited_longest)
        }
        SeedChokingAlgorithm::FastestUpload => b
            .uploaded_last_round
            .cmp(&a.uploaded_last_round)
            .then_with(waited_longest),
        SeedChokingAlgorithm::AntiLeech => b
            .anti_leech_score
            .cmp(&a.anti_leech_score)
            .then_with(waited_longest),
    }
}

/// The regular rechoke. Returns one decision per candidate (same order) and
/// the number of upload slots that were allowed (`None` = unlimited).
pub(crate) fn rechoke(
    cfg: &ChokerConfig,
    torrents: &[TorrentChokeInfo],
    cands: &[ChokeCandidate],
    round: Duration,
    now: Instant,
) -> (Vec<ChokeDecision>, Option<usize>) {
    let slots = match cfg.algorithm {
        ChokingAlgorithm::FixedSlots => cfg.global_slots,
        ChokingAlgorithm::RateBased => Some(rate_based_slots(cands, round)),
    };

    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| compare(&cands[a], &cands[b], cfg, torrents, now));

    let current_optimistic = cands.iter().filter(|c| c.optimistic).count();
    let reserved_optimistic = match slots {
        Some(s) if current_optimistic == 0 => (s / 5).max(1),
        _ => current_optimistic,
    };
    let mut unchoke_set_size = match slots {
        Some(s) => s.saturating_sub(reserved_optimistic),
        None => usize::MAX,
    };

    // Optimistic unchokes occupy their torrent's slots.
    let mut per_torrent = vec![0usize; torrents.len()];
    for c in cands.iter().filter(|c| c.optimistic) {
        if let Some(n) = per_torrent.get_mut(c.torrent) {
            *n += 1;
        }
    }

    let mut decisions = vec![ChokeDecision::Choke; cands.len()];
    for idx in order {
        let c = &cands[idx];
        let max = torrents.get(c.torrent).and_then(|t| t.max_uploads);
        let count = per_torrent.get(c.torrent).copied().unwrap_or_default();
        let torrent_has_room = c.optimistic || max.is_none_or(|m| count < m);
        if unchoke_set_size > 0 && torrent_has_room {
            unchoke_set_size -= 1;
            if !c.optimistic
                && let Some(n) = per_torrent.get_mut(c.torrent)
            {
                *n += 1;
            }
            decisions[idx] = ChokeDecision::Unchoke;
        } else if c.optimistic {
            decisions[idx] = ChokeDecision::KeepOptimistic;
        }
    }
    (decisions, slots)
}

/// What to do with a candidate at an optimistic unchoke rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OptimisticDecision {
    /// Nothing changes for this peer.
    Leave,
    /// Unchoke it as the new optimistic unchoke.
    Unchoke,
    /// It keeps its optimistic slot for another round.
    Keep,
    /// Its optimistic slot ends: choke it.
    Choke,
}

/// The optimistic unchoke rotation. `allowed_slots` is the number of upload
/// slots of the last rechoke (`None` = unlimited).
pub(crate) fn rotate_optimistic(
    allowed_slots: Option<usize>,
    torrents: &[TorrentChokeInfo],
    cands: &[ChokeCandidate],
) -> Vec<OptimisticDecision> {
    let mut decisions = vec![OptimisticDecision::Leave; cands.len()];
    let num_opt = match allowed_slots {
        Some(0) => 0,
        Some(s) => (s / 5).max(1),
        None => 1,
    };

    // Regular (non-optimistic) unchokes per torrent.
    let mut per_torrent = vec![0usize; torrents.len()];
    for c in cands.iter().filter(|c| !c.choked && !c.optimistic) {
        if let Some(n) = per_torrent.get_mut(c.torrent) {
            *n += 1;
        }
    }
    let has_room = |c: &ChokeCandidate, per_torrent: &[usize]| {
        let max = torrents.get(c.torrent).and_then(|t| t.max_uploads);
        let count = per_torrent.get(c.torrent).copied().unwrap_or_default();
        max.is_none_or(|m| count < m)
    };

    let mut eligible: Vec<usize> = (0..cands.len())
        .filter(|&i| cands[i].choked || cands[i].optimistic)
        .filter(|&i| has_room(&cands[i], &per_torrent))
        .collect();
    // The ones that waited the longest since their last optimistic unchoke.
    // Then the ones that were last unchoked the longest ago.
    eligible.sort_by(|&a, &b| {
        (cands[a].last_optimistic, cands[a].last_unchoke)
            .cmp(&(cands[b].last_optimistic, cands[b].last_unchoke))
    });

    let mut chosen = 0;
    for idx in eligible {
        if chosen >= num_opt {
            break;
        }
        let c = &cands[idx];
        if !has_room(c, &per_torrent) {
            continue;
        }
        if let Some(n) = per_torrent.get_mut(c.torrent) {
            *n += 1;
        }
        chosen += 1;
        decisions[idx] = if c.optimistic {
            OptimisticDecision::Keep
        } else {
            OptimisticDecision::Unchoke
        };
    }
    for (i, c) in cands.iter().enumerate() {
        if c.optimistic && decisions[i] == OptimisticDecision::Leave {
            decisions[i] = OptimisticDecision::Choke;
        }
    }
    decisions
}

/// What a rechoke round does to a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RechokeAction {
    Unchoke,
    UnchokeOptimistic,
    Choke,
    Leave,
}

/// The regular rechoke, plus an immediate optimistic unchoke when the
/// round reserved optimistic slots that no peer holds (otherwise those slots
/// would stay unused until the next rotation).
pub(crate) fn rechoke_round(
    cfg: &ChokerConfig,
    torrents: &[TorrentChokeInfo],
    cands: &[ChokeCandidate],
    round: Duration,
    now: Instant,
) -> (Vec<RechokeAction>, Option<usize>) {
    let (decisions, slots) = rechoke(cfg, torrents, cands, round, now);
    let mut actions: Vec<RechokeAction> = decisions
        .iter()
        .map(|d| match d {
            ChokeDecision::Unchoke => RechokeAction::Unchoke,
            ChokeDecision::Choke => RechokeAction::Choke,
            ChokeDecision::KeepOptimistic => RechokeAction::Leave,
        })
        .collect();
    if slots != Some(0) && !decisions.contains(&ChokeDecision::KeepOptimistic) {
        let after: Vec<ChokeCandidate> = cands
            .iter()
            .zip(&decisions)
            .map(|(c, d)| ChokeCandidate {
                choked: *d != ChokeDecision::Unchoke,
                optimistic: false,
                ..c.clone()
            })
            .collect();
        for (a, o) in actions
            .iter_mut()
            .zip(rotate_optimistic(slots, torrents, &after))
        {
            if o == OptimisticDecision::Unchoke {
                *a = RechokeAction::UnchokeOptimistic;
            }
        }
    }
    (actions, slots)
}

// === Live torrent side: collecting candidates and applying decisions ===

impl TorrentStateLive {
    pub(crate) fn choke_info(&self) -> TorrentChokeInfo {
        TorrentChokeInfo {
            max_uploads: self
                .shared
                .runtime
                .max_uploads()
                .map(|v| usize::try_from(v).unwrap_or(usize::MAX)),
            piece_length: u64::from(self.lengths.default_piece_length()),
        }
    }

    /// Collects the interested peers as choke candidates (with their address)
    /// and chokes unchoked peers that are not interested. With
    /// `new_round`, the per-round byte counters start over.
    pub(crate) fn collect_choke_candidates(
        &self,
        torrent: usize,
        new_round: bool,
        now: Instant,
        out: &mut Vec<(ChokeCandidate, PeerHandle)>,
    ) {
        let upload_allowed = !self.shared.options.disable_upload();
        let total_size = self.lengths.total_length();
        let piece_length = u64::from(self.lengths.default_piece_length());
        let total_pieces = self.lengths.total_pieces() as usize;
        for mut e in self.peers.states.iter_mut() {
            let addr = *e.key();
            let peer = e.value_mut();
            let fetched = peer
                .stats
                .counters
                .fetched_bytes
                .load(AtomicOrdering::Relaxed);
            let uploaded = peer
                .stats
                .counters
                .uploaded_bytes
                .load(AtomicOrdering::Relaxed);
            let Some(live) = peer.get_live_mut() else {
                continue;
            };
            let downloaded_last_round = fetched.saturating_sub(live.round_fetched_base);
            let uploaded_last_round = uploaded.saturating_sub(live.round_uploaded_base);
            if new_round {
                live.round_fetched_base = fetched;
                live.round_uploaded_base = uploaded;
            }
            if !live.peer_interested || !upload_allowed {
                if !live.am_choking {
                    self.peers.set_choking(peer, true, false, now);
                }
                continue;
            }
            let have_pieces = count_ones(live.bitfield.as_raw_slice(), total_pieces);
            out.push((
                ChokeCandidate {
                    torrent,
                    choked: live.am_choking,
                    optimistic: live.optimistic && !live.am_choking,
                    downloaded_last_round,
                    uploaded_last_round,
                    uploaded_since_unchoke: uploaded.saturating_sub(live.uploaded_at_unchoke),
                    last_unchoke: live.last_unchoke,
                    last_optimistic: live.last_optimistic,
                    anti_leech_score: anti_leech_score(
                        total_size,
                        piece_length,
                        u64::from(have_pieces),
                        uploaded,
                    ),
                },
                addr,
            ));
        }
    }

    pub(crate) fn apply_choke(
        &self,
        addr: PeerHandle,
        choke: bool,
        optimistic: bool,
        now: Instant,
    ) {
        self.peers.with_peer_mut(addr, "apply_choke", |p| {
            if !choke && !p.get_live().is_some_and(|l| l.peer_interested) {
                return;
            }
            self.peers.set_choking(p, choke, optimistic, now);
        });
    }

    /// A peer became interested: unchoke it right away if there is a free
    /// upload slot (libtorrent's "preemptive unchoke"); otherwise it waits for
    /// the next rechoke.
    pub(crate) fn maybe_unchoke_interested(&self, addr: PeerHandle) {
        if self.shared.options.disable_upload() {
            return;
        }
        let Some(session) = self.shared.session.upgrade() else {
            return;
        };
        let session_unchoked = session.stats.peers.unchoked.load(AtomicOrdering::Relaxed);
        if !session
            .peer_policy
            .has_free_upload_slot(session_unchoked as usize)
        {
            return;
        }
        if let Some(max) = self.shared.runtime.max_uploads()
            && self.peers.stats.unchoked.load(AtomicOrdering::Relaxed) >= max
        {
            return;
        }
        let now = Instant::now();
        self.peers.with_peer_mut(addr, "preemptive_unchoke", |p| {
            if p.get_live().is_some_and(|l| {
                l.peer_interested
                    && l.am_choking
                    && preemptive_unchoke_allowed(l.last_unchoke, now)
            }) {
                self.peers.set_choking(p, false, false, now);
            }
        });
    }

    /// The peer is no longer interested: free its upload slot.
    pub(crate) fn choke_not_interested(&self, addr: PeerHandle) {
        let freed = self
            .peers
            .with_peer_mut(addr, "choke_not_interested", |p| {
                p.get_live()
                    .is_some_and(|l| !l.peer_interested && !l.am_choking)
                    && self.peers.set_choking(p, true, false, Instant::now())
            })
            .unwrap_or(false);
        // Give the freed slot to a waiting peer now rather than at the next
        // round (with unlimited slots nobody waits).
        if freed
            && let Some(session) = self.shared.session.upgrade()
            && (session.peer_policy.allowed_slots().is_some()
                || self.shared.runtime.max_uploads().is_some())
        {
            session.peer_policy.request_rechoke();
        }
    }

    /// Update the per-peer rate estimates (called once a second) and add the
    /// transfer activity of peers outside the local network to `up` / `down`
    /// (for the uTP/TCP mixed mode throttles).
    pub(crate) fn update_peer_rates(
        &self,
        elapsed: Duration,
        up: &mut crate::mixed_mode::DirectionActivity,
        down: &mut crate::mixed_mode::DirectionActivity,
    ) {
        let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        for mut e in self.peers.states.iter_mut() {
            let lan = crate::peer_policy::is_lan_addr(e.key());
            let peer = e.value_mut();
            let fetched = peer
                .stats
                .counters
                .fetched_bytes
                .load(AtomicOrdering::Relaxed);
            let uploaded = peer
                .stats
                .counters
                .uploaded_bytes
                .load(AtomicOrdering::Relaxed);
            if let Some(live) = peer.get_live_mut() {
                live.rates.update(fetched, uploaded, elapsed_ms);
                if !lan {
                    let utp = matches!(live.connection_kind, ConnectionKind::Utp);
                    up.add(utp, live.rates.up_bps);
                    down.add(utp, live.rates.down_bps);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(torrent: usize) -> ChokeCandidate {
        ChokeCandidate {
            torrent,
            choked: true,
            optimistic: false,
            downloaded_last_round: 0,
            uploaded_last_round: 0,
            uploaded_since_unchoke: 0,
            last_unchoke: None,
            last_optimistic: None,
            anti_leech_score: 0,
        }
    }

    fn torrents(n: usize, max_uploads: Option<usize>) -> Vec<TorrentChokeInfo> {
        vec![
            TorrentChokeInfo {
                max_uploads,
                piece_length: 16384,
            };
            n
        ]
    }

    fn cfg(slots: Option<usize>, seed: SeedChokingAlgorithm) -> ChokerConfig {
        ChokerConfig {
            global_slots: slots,
            algorithm: ChokingAlgorithm::FixedSlots,
            seed_algorithm: seed,
        }
    }

    fn unchoked(d: &[ChokeDecision]) -> Vec<usize> {
        d.iter()
            .enumerate()
            .filter(|(_, d)| **d == ChokeDecision::Unchoke)
            .map(|(i, _)| i)
            .collect()
    }

    const ROUND: Duration = RECHOKE_INTERVAL;

    #[test]
    fn interest_toggling_gets_one_preemptive_unchoke_per_interval() {
        let now = Instant::now();
        assert!(super::preemptive_unchoke_allowed(None, now));
        assert!(!super::preemptive_unchoke_allowed(Some(now), now));
        assert!(!super::preemptive_unchoke_allowed(Some(now), now + Duration::from_secs(9)));
        assert!(super::preemptive_unchoke_allowed(
            Some(now),
            now + super::PREEMPTIVE_UNCHOKE_MIN_INTERVAL
        ));
    }

    #[test]
    fn tit_for_tat_then_fastest_upload() {
        let mut c: Vec<_> = (0..6).map(|_| cand(0)).collect();
        c[3].downloaded_last_round = 1000;
        c[5].downloaded_last_round = 500;
        c[1].uploaded_last_round = 900;
        c[2].uploaded_last_round = 800;
        let now = Instant::now();
        // 5 slots, none optimistic yet: 1 is reserved for the optimistic unchoke.
        let (d, slots) = rechoke(
            &cfg(Some(5), SeedChokingAlgorithm::FastestUpload),
            &torrents(1, None),
            &c,
            ROUND,
            now,
        );
        assert_eq!(slots, Some(5));
        assert_eq!(unchoked(&d), vec![1, 2, 3, 5]);
    }

    #[test]
    fn unlimited_unchokes_everyone_and_respects_torrent_limit() {
        let c: Vec<_> = (0..10).map(|i| cand(i % 2)).collect();
        let now = Instant::now();
        let (d, slots) = rechoke(
            &cfg(None, SeedChokingAlgorithm::FastestUpload),
            &torrents(2, None),
            &c,
            ROUND,
            now,
        );
        assert_eq!(slots, None);
        assert_eq!(unchoked(&d).len(), 10);

        let t = vec![
            TorrentChokeInfo {
                max_uploads: Some(2),
                piece_length: 1,
            },
            TorrentChokeInfo {
                max_uploads: None,
                piece_length: 1,
            },
        ];
        let (d, _) = rechoke(
            &cfg(None, SeedChokingAlgorithm::FastestUpload),
            &t,
            &c,
            ROUND,
            now,
        );
        let u = unchoked(&d);
        assert_eq!(u.iter().filter(|i| *i % 2 == 0).count(), 2);
        assert_eq!(u.iter().filter(|i| *i % 2 == 1).count(), 5);

        // max_uploads = 0 disables uploading for that torrent.
        let t = vec![
            TorrentChokeInfo {
                max_uploads: Some(0),
                piece_length: 1,
            },
            TorrentChokeInfo {
                max_uploads: Some(1),
                piece_length: 1,
            },
        ];
        let (d, _) = rechoke(
            &cfg(Some(20), SeedChokingAlgorithm::FastestUpload),
            &t,
            &c,
            ROUND,
            now,
        );
        assert_eq!(unchoked(&d).len(), 1);
    }

    #[test]
    fn global_slots_shared_across_torrents_and_optimistic_kept() {
        let mut c: Vec<_> = (0..8).map(|i| cand(i % 2)).collect();
        for (i, c) in c.iter_mut().enumerate() {
            c.uploaded_last_round = 100 * i as u64;
        }
        c[0].optimistic = true;
        c[0].choked = false;
        let now = Instant::now();
        let (d, _) = rechoke(
            &cfg(Some(4), SeedChokingAlgorithm::FastestUpload),
            &torrents(2, None),
            &c,
            ROUND,
            now,
        );
        // 1 optimistic in place -> 3 regular slots for the fastest ones.
        assert_eq!(unchoked(&d), vec![5, 6, 7]);
        assert_eq!(d[0], ChokeDecision::KeepOptimistic);
    }

    #[test]
    fn anti_leech_prefers_new_and_almost_done_peers() {
        assert_eq!(anti_leech_score(1000, 10, 0, 0), 1000);
        assert_eq!(anti_leech_score(1000, 10, 50, 0), 0);
        assert_eq!(anti_leech_score(1000, 10, 100, 0), 1000);
        assert_eq!(anti_leech_score(1000, 10, 75, 0), 500);
        assert_eq!(anti_leech_score(0, 10, 75, 0), 0);

        let mut c: Vec<_> = (0..3).map(|_| cand(0)).collect();
        c[0].anti_leech_score = 0;
        c[1].anti_leech_score = 900;
        c[2].anti_leech_score = 400;
        let (d, _) = rechoke(
            &cfg(Some(2), SeedChokingAlgorithm::AntiLeech),
            &torrents(1, None),
            &c,
            ROUND,
            Instant::now(),
        );
        assert_eq!(unchoked(&d), vec![1]);
    }

    #[test]
    fn round_robin_rotates_peers_that_used_their_quota() {
        let now = Instant::now();
        let long_ago = now - Duration::from_secs(120);
        let mut c: Vec<_> = (0..3).map(|_| cand(0)).collect();
        // Peer 0 is unchoked, used its quota and has been unchoked > 1 minute.
        c[0].choked = false;
        c[0].last_unchoke = Some(long_ago);
        c[0].uploaded_since_unchoke = 16384 * SEEDING_PIECE_QUOTA + 1;
        c[0].uploaded_last_round = 10_000;
        // Peer 1 is unchoked and still within its quota.
        c[1].choked = false;
        c[1].last_unchoke = Some(now - Duration::from_secs(10));
        c[1].uploaded_last_round = 10;
        // Peer 2 waits.
        c[2].last_unchoke = Some(long_ago);
        let (d, _) = rechoke(
            &cfg(Some(3), SeedChokingAlgorithm::RoundRobin),
            &torrents(1, None),
            &c,
            ROUND,
            now,
        );
        // 3 slots: 1 reserved optimistic, 2 regular: peer 1 keeps, peer 2 replaces peer 0.
        assert_eq!(unchoked(&d), vec![1, 2]);
    }

    #[test]
    fn rate_based_slots_grow_with_upload_rate() {
        let secs = ROUND.as_secs();
        let mut c: Vec<_> = (0..5).map(|_| cand(0)).collect();
        assert_eq!(rate_based_slots(&c, ROUND), 1);
        // Rates 10k, 5k, 4k, 1k: thresholds 1k, 3k, 5k, 7k -> 2 pass.
        for (c, rate) in c.iter_mut().zip([10_240u64, 5_000, 4_000, 1_000]) {
            c.uploaded_last_round = rate * secs;
        }
        assert_eq!(rate_based_slots(&c, ROUND), 3);
        let config = ChokerConfig {
            global_slots: Some(100),
            algorithm: ChokingAlgorithm::RateBased,
            seed_algorithm: SeedChokingAlgorithm::FastestUpload,
        };
        let (d, slots) = rechoke(&config, &torrents(1, None), &c, ROUND, Instant::now());
        assert_eq!(slots, Some(3));
        // 3 slots, 1 reserved for the optimistic unchoke.
        assert_eq!(unchoked(&d), vec![0, 1]);
    }

    #[test]
    fn optimistic_rotation_picks_longest_waiting() {
        let now = Instant::now();
        let mut c: Vec<_> = (0..6).map(|_| cand(0)).collect();
        c[0].optimistic = true;
        c[0].choked = false;
        c[0].last_optimistic = Some(now - Duration::from_secs(30));
        c[1].last_optimistic = Some(now - Duration::from_secs(100));
        c[2].last_optimistic = None;
        c[3].last_optimistic = Some(now - Duration::from_secs(50));
        c[4].choked = false; // regular unchoke, not eligible
        c[5].last_optimistic = Some(now);
        // 10 slots -> 2 optimistic.
        let d = rotate_optimistic(Some(10), &torrents(1, None), &c);
        assert_eq!(d[2], OptimisticDecision::Unchoke);
        assert_eq!(d[1], OptimisticDecision::Unchoke);
        assert_eq!(d[0], OptimisticDecision::Choke);
        assert_eq!(d[3], OptimisticDecision::Leave);
        assert_eq!(d[4], OptimisticDecision::Leave);

        // The per-torrent limit is respected: 1 regular unchoke already fills it.
        let d = rotate_optimistic(Some(10), &torrents(1, Some(1)), &c);
        assert!(!d.contains(&OptimisticDecision::Unchoke));
        assert_eq!(d[0], OptimisticDecision::Choke);

        // No slots, no optimistic unchokes.
        let d = rotate_optimistic(Some(0), &torrents(1, None), &c);
        assert!(!d.contains(&OptimisticDecision::Unchoke));
    }

    #[test]
    fn rechoke_round_fills_the_optimistic_slot_at_once() {
        let now = Instant::now();
        // One slot: peer 0 got it preemptively, peer 1 waits.
        let mut c: Vec<_> = (0..2).map(|_| cand(0)).collect();
        c[0].choked = false;
        c[0].last_unchoke = Some(now - Duration::from_secs(10));
        let (a, _) = rechoke_round(
            &cfg(Some(1), SeedChokingAlgorithm::FastestUpload),
            &torrents(1, None),
            &c,
            ROUND,
            now,
        );
        // The single slot is reserved for the optimistic unchoke, which goes to
        // the peer that waited.
        assert_eq!(
            a,
            vec![RechokeAction::Choke, RechokeAction::UnchokeOptimistic]
        );

        // An optimistic peer in place: nothing extra.
        c[0].optimistic = true;
        let (a, _) = rechoke_round(
            &cfg(Some(1), SeedChokingAlgorithm::FastestUpload),
            &torrents(1, None),
            &c,
            ROUND,
            now,
        );
        assert_eq!(a, vec![RechokeAction::Leave, RechokeAction::Choke]);

        // Enough slots: regular unchokes plus one optimistic for the rest.
        let c: Vec<_> = (0..8).map(|_| cand(0)).collect();
        let (a, _) = rechoke_round(
            &cfg(Some(5), SeedChokingAlgorithm::FastestUpload),
            &torrents(1, None),
            &c,
            ROUND,
            now,
        );
        assert_eq!(
            a.iter().filter(|a| **a == RechokeAction::Unchoke).count(),
            4
        );
        assert_eq!(
            a.iter()
                .filter(|a| **a == RechokeAction::UnchokeOptimistic)
                .count(),
            1
        );
    }
}
