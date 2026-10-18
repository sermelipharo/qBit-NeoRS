use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use dashmap::DashMap;
use librqbit_core::lengths::ValidPieceIndex;
use parking_lot::RwLock;
use peer_binary_protocol::Message;
use tracing::debug;

use crate::{
    Error,
    peer_connection::WriterRequest,
    torrent_state::utils::{TimedExistence, atomic_inc},
    type_aliases::{BF, PeerHandle},
};

use self::stats::{AggregatePeerStats, AggregatePeerStatsAtomic};

use super::{
    availability::PieceAvailability,
    peer::{LivePeerState, Peer, PeerRx, PeerState, PeerTx},
    peer_details::PeerSource,
};

pub mod stats;

/// Known peers per torrent learned from untrusted sources (libtorrent's
/// `max_peerlist_size` is 3000).
pub(crate) const MAX_KNOWN_PEERS: usize = 4000;

/// Size of the peers map above which entries that can never be used again
/// are evicted (down to 3/4 of it).
///
/// Every incoming connection creates an entry. Incoming peers are not
/// retried and usually come from ephemeral ports, so once they die or are
/// no longer needed their entries are dead weight; on a torrent with peer
/// churn the map grew without bound (and counted against
/// [`MAX_KNOWN_PEERS`], shutting out new peers from trackers and DHT).
pub(crate) const MAX_TRACKED_PEERS: usize = 1024;

pub(crate) struct PeerStates {
    pub session_stats: Arc<AggregatePeerStatsAtomic>,

    // This keeps track of live addresses we connected to, for PEX.
    pub live_outgoing_peers: RwLock<HashSet<PeerHandle>>,
    pub stats: AggregatePeerStatsAtomic,
    pub states: DashMap<PeerHandle, Peer>,
    // How many live peers have each piece.
    pub availability: PieceAvailability,
    // A prune that found too little to evict is not retried before the map
    // reaches this size, so a map full of useful entries isn't rescanned on
    // every addition.
    pub prune_floor: AtomicUsize,
}

impl Drop for PeerStates {
    fn drop(&mut self) {
        for (_, p) in std::mem::take(&mut self.states).into_iter() {
            p.destroy(self);
        }
    }
}

impl PeerStates {
    pub fn stats(&self) -> AggregatePeerStats {
        self.stats.snapshot()
    }

    pub fn add_if_not_seen(&self, addr: SocketAddr, source: PeerSource) -> Option<PeerHandle> {
        use dashmap::mapref::entry::Entry;
        self.maybe_prune();
        // Trackers, DHT nodes, PeX and LSD are untrusted: they can't grow
        // the list of known peers past the cap (dead entries leave it once
        // their reconnect backoff is exhausted). Peers the user adds are
        // always taken.
        if !matches!(source, PeerSource::Manual | PeerSource::Incoming)
            && self.states.len() >= MAX_KNOWN_PEERS
            && !self.states.contains_key(&addr)
        {
            return None;
        }
        match self.states.entry(addr) {
            Entry::Occupied(mut occ) => {
                occ.get_mut().sources.insert(source);
                None
            }
            Entry::Vacant(vac) => {
                vac.insert(Peer::new_with_outgoing_address(addr, source));
                atomic_inc(&self.stats.queued);
                atomic_inc(&self.session_stats.queued);

                atomic_inc(&self.stats.seen);
                atomic_inc(&self.session_stats.seen);
                Some(addr)
            }
        }
    }
    pub fn with_peer<R>(&self, addr: PeerHandle, f: impl FnOnce(&Peer) -> R) -> Option<R> {
        self.states.get(&addr).map(|e| f(e.value()))
    }

    pub fn with_peer_mut<R>(
        &self,
        addr: PeerHandle,
        reason: &'static str,
        f: impl FnOnce(&mut Peer) -> R,
    ) -> Option<R> {
        use crate::torrent_state::utils::timeit;
        timeit(reason, || self.states.get_mut(&addr))
            .map(|e| f(TimedExistence::new(e, reason).value_mut()))
    }

    pub fn with_live<R>(&self, addr: PeerHandle, f: impl FnOnce(&LivePeerState) -> R) -> Option<R> {
        self.with_peer(addr, |peer| peer.get_live().map(f))
            .flatten()
    }

    pub fn with_live_mut<R>(
        &self,
        addr: PeerHandle,
        reason: &'static str,
        f: impl FnOnce(&mut LivePeerState) -> R,
    ) -> Option<R> {
        self.with_peer_mut(addr, reason, |peer| peer.get_live_mut().map(f))
            .flatten()
    }

    pub fn drop_peer(&self, handle: PeerHandle) -> Option<Peer> {
        let p = self.states.remove(&handle).map(|r| r.1)?;
        let s = p.get_state();
        self.stats.dec(s);
        self.session_stats.dec(s);
        if let PeerState::Live(l) = s {
            self.availability.remove_bitfield(&l.bitfield);
        }

        Some(p)
    }

    /// Removes the peer if its state satisfies `cond`, keeping the aggregate
    /// counters consistent. Unlike [`PeerStates::drop_peer`] the removal is
    /// conditional, so a peer that became alive again meanwhile stays.
    fn drop_peer_if(&self, handle: PeerHandle, cond: impl Fn(&Peer) -> bool) -> Option<Peer> {
        let p = self
            .states
            .remove_if(&handle, |_, p| cond(p))
            .map(|(_, p)| p)?;
        let s = p.get_state();
        self.stats.dec(s);
        self.session_stats.dec(s);
        Some(p)
    }

    /// Called before an entry is added: keeps the map near
    /// [`MAX_TRACKED_PEERS`] (amortized, at most one scan per quarter of it).
    pub(crate) fn maybe_prune(&self) {
        let len = self.states.len();
        if len < MAX_TRACKED_PEERS || len < self.prune_floor.load(Ordering::Relaxed) {
            return;
        }
        let keep = MAX_TRACKED_PEERS - MAX_TRACKED_PEERS / 4;
        self.prune_useless_peers(keep);
        let after = self.states.len();
        let floor = if after > keep {
            after + MAX_TRACKED_PEERS / 4
        } else {
            0
        };
        self.prune_floor.store(floor, Ordering::Relaxed);
    }

    /// Evicts entries that can never be used again until at most `max_keep`
    /// are tracked. Returns how many were removed. In this order:
    ///
    /// 1. `NotNeeded` peers we have no outgoing address for (they connected
    ///    to us): never retried, usually from an ephemeral port.
    /// 2. `Dead` peers without an outgoing address: incoming peers are never
    ///    re-queued.
    /// 3. `NotNeeded` peers we could connect to ourselves: revivable (e.g.
    ///    when the selected files change), but learned again from trackers
    ///    and DHT, so evicted only as a last resort.
    ///
    /// Live, connecting, queued and dead outgoing peers (waiting for their
    /// reconnect backoff) are never touched.
    pub(crate) fn prune_useless_peers(&self, max_keep: usize) -> usize {
        let total = self.states.len();
        if total <= max_keep {
            return 0;
        }
        let mut to_remove = total - max_keep;
        let mut removed = 0;

        let evict_passes: [fn(&Peer) -> bool; 3] = [
            |p| matches!(p.get_state(), PeerState::NotNeeded) && p.outgoing_address.is_none(),
            |p| matches!(p.get_state(), PeerState::Dead) && p.outgoing_address.is_none(),
            |p| matches!(p.get_state(), PeerState::NotNeeded),
        ];
        for is_evictable in evict_passes {
            if to_remove == 0 {
                break;
            }
            let candidates: Vec<PeerHandle> = self
                .states
                .iter()
                .filter(|e| is_evictable(e.value()))
                .take(to_remove)
                .map(|e| *e.key())
                .collect();
            for handle in candidates {
                // remove_if checks the state again under the shard lock.
                if self.drop_peer_if(handle, is_evictable).is_some() {
                    removed += 1;
                    to_remove -= 1;
                    if to_remove == 0 {
                        break;
                    }
                }
            }
        }
        if removed > 0 {
            debug!(
                removed,
                remaining = self.states.len(),
                "pruned useless peers"
            );
        }
        removed
    }

    pub fn is_peer_not_interested_and_has_full_torrent(
        &self,
        handle: PeerHandle,
        total_pieces: usize,
    ) -> bool {
        self.with_live(handle, |live| {
            !live.peer_interested && live.has_full_torrent(total_pieces)
        })
        .unwrap_or(false)
    }

    pub fn mark_peer_interested(&self, handle: PeerHandle, is_interested: bool) -> Option<bool> {
        self.with_live_mut(handle, "mark_peer_interested", |live| {
            let prev = live.peer_interested;
            live.peer_interested = is_interested;
            prev
        })
    }

    pub fn update_bitfield(
        &self,
        handle: PeerHandle,
        bitfield: BF,
        total_pieces: usize,
    ) -> Option<()> {
        self.with_live_mut(handle, "update_bitfield", |live| {
            self.availability.remove_bitfield(&live.bitfield);
            self.availability.add_bitfield(&bitfield);
            live.bitfield = bitfield;
            self.update_seeder_flag(live, total_pieces);
        })
    }

    // Record a "have" from a live peer. Returns false if the index is out of range.
    pub fn mark_have(&self, live: &mut LivePeerState, piece: usize, total_pieces: usize) -> bool {
        if live.bitfield.is_empty() {
            live.bitfield =
                BF::from_boxed_slice(vec![0u8; total_pieces.div_ceil(8)].into_boxed_slice());
        }
        if piece >= total_pieces {
            return false;
        }
        match live.bitfield.get_mut(piece) {
            Some(mut v) => {
                if !*v {
                    *v = true;
                    self.availability.inc(piece);
                }
            }
            None => return false,
        }
        self.update_seeder_flag(live, total_pieces);
        true
    }

    /// Choke or unchoke a live peer: sends the message and keeps the unchoked
    /// counters in sync. An unchoked peer can also be switched between
    /// regular and optimistic. Returns true if the choke state changed.
    pub fn set_choking(
        &self,
        peer: &mut Peer,
        choke: bool,
        optimistic: bool,
        now: Instant,
    ) -> bool {
        let uploaded = peer.stats.counters.uploaded_bytes.load(Ordering::Relaxed);
        let Some(live) = peer.get_live_mut() else {
            return false;
        };
        if live.am_choking == choke {
            if !choke && live.optimistic != optimistic {
                live.optimistic = optimistic;
                if optimistic {
                    live.last_optimistic = Some(now);
                }
            }
            return false;
        }
        live.am_choking = choke;
        if choke {
            live.optimistic = false;
            let _ = live.tx.send(WriterRequest::Message(Message::Choke));
        } else {
            live.optimistic = optimistic;
            live.last_unchoke = Some(now);
            live.uploaded_at_unchoke = uploaded;
            if optimistic {
                live.last_optimistic = Some(now);
            }
            let _ = live.tx.send(WriterRequest::Message(Message::Unchoke));
        }
        self.stats.unchoked_changed(!choke);
        self.session_stats.unchoked_changed(!choke);
        true
    }

    // Recompute the cached "this peer has the full torrent" flag after its bitfield
    // changed, keeping the aggregate seeder counters in sync. Must be called on every
    // bitfield change of a live peer.
    pub fn update_seeder_flag(&self, live: &mut LivePeerState, total_pieces: usize) {
        let seeder = live.has_full_torrent(total_pieces);
        if seeder == live.seeder {
            return;
        }
        live.seeder = seeder;
        self.stats.seeder_flag_changed(seeder);
        self.session_stats.seeder_flag_changed(seeder);
    }

    pub fn mark_peer_connecting(&self, h: PeerHandle) -> crate::Result<(PeerRx, PeerTx)> {
        let rx = self
            .with_peer_mut(h, "mark_peer_connecting", |peer| {
                peer.idle_to_connecting(self)
                    .ok_or(Error::BugInvalidPeerState)
            })
            .ok_or(Error::BugPeerNotFound)??;
        Ok(rx)
    }

    pub fn reset_peer_backoff(&self, handle: PeerHandle) {
        self.with_peer_mut(handle, "reset_peer_backoff", |p| {
            p.stats.reset_backoff();
        });
    }

    pub fn mark_peer_not_needed(&self, handle: PeerHandle) -> Option<PeerState> {
        let prev = self.with_peer_mut(handle, "mark_peer_not_needed", |peer| {
            peer.set_not_needed(self)
        })?;
        Some(prev)
    }

    pub(crate) fn on_steal(
        &self,
        from_peer: SocketAddr,
        to_peer: SocketAddr,
        stolen_idx: ValidPieceIndex,
    ) {
        self.with_peer(to_peer, |p| {
            atomic_inc(&p.stats.counters.times_i_stole);
        });
        self.with_peer(from_peer, |p| {
            atomic_inc(&p.stats.counters.times_stolen_from_me);
        });
        self.stats.inc_steals();
        self.session_stats.inc_steals();

        self.with_live_mut(from_peer, "send_cancellations", |live| {
            live.cancel_inflight_requests_for_piece(stolen_idx);
        });
    }
}

#[cfg(test)]
mod tests {
    use librqbit_core::hash_id::Id20;

    use super::*;
    use crate::stream_connect::ConnectionKind;

    const TOTAL_PIECES: usize = 8;

    fn full_bitfield() -> BF {
        BF::from_boxed_slice(vec![0xffu8; 1].into_boxed_slice())
    }

    fn empty_bitfield() -> BF {
        BF::from_boxed_slice(vec![0u8; 1].into_boxed_slice())
    }

    fn make_peer_states() -> PeerStates {
        PeerStates {
            session_stats: Default::default(),
            live_outgoing_peers: Default::default(),
            stats: Default::default(),
            states: Default::default(),
            availability: PieceAvailability::new(TOTAL_PIECES),
            prune_floor: Default::default(),
        }
    }

    fn insert_incoming(peers: &PeerStates, port: u16, state: Option<PeerState>) -> PeerRx {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut peer = Peer::new_live_for_incoming_connection(
            addr,
            Id20::new(*b"01234567890123456789"),
            tx,
            peers,
            ConnectionKind::Tcp,
        );
        if let Some(state) = state {
            peer.set_state(state, peers);
        }
        peers.states.insert(addr, peer);
        rx
    }

    fn insert_outgoing(peers: &PeerStates, port: u16, state: PeerState) {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        peers.add_if_not_seen(addr, PeerSource::Tracker).unwrap();
        peers.with_peer_mut(addr, "test", |p| {
            p.set_state(state, peers);
        });
    }

    fn has(peers: &PeerStates, port: u16) -> bool {
        peers
            .states
            .contains_key(&SocketAddr::from(([127, 0, 0, 1], port)))
    }

    #[test]
    fn prune_evicts_useless_entries_in_order() {
        let peers = make_peer_states();
        // Not needed and dead incoming: removable.
        for port in 1..=4 {
            insert_incoming(&peers, port, Some(PeerState::NotNeeded));
        }
        for port in 5..=7 {
            insert_incoming(&peers, port, Some(PeerState::Dead));
        }
        // Not needed outgoing: last resort. Dead outgoing: never.
        for port in 8..=9 {
            insert_outgoing(&peers, port, PeerState::NotNeeded);
        }
        for port in 10..=11 {
            insert_outgoing(&peers, port, PeerState::Dead);
        }
        // Live incoming and queued outgoing: never.
        let _rx12 = insert_incoming(&peers, 12, None);
        let _rx13 = insert_incoming(&peers, 13, None);
        peers
            .add_if_not_seen(SocketAddr::from(([127, 0, 0, 1], 14)), PeerSource::Tracker)
            .unwrap();

        assert_eq!(peers.prune_useless_peers(8), 6);
        assert_eq!(peers.states.len(), 8);
        assert!((1..=4).all(|p| !has(&peers, p)));
        assert_eq!((5..=7).filter(|p| !has(&peers, *p)).count(), 2);
        assert!((8..=14).all(|p| has(&peers, p)));

        let s = peers.stats.snapshot();
        assert_eq!((s.not_needed, s.dead, s.live, s.queued), (2, 3, 2, 1));

        // Asking for less evicts the not-needed outgoing peers, nothing else.
        assert_eq!(peers.prune_useless_peers(0), 3);
        assert!((10..=14).all(|p| has(&peers, p)));
        assert_eq!(peers.prune_useless_peers(0), 0);
    }

    #[test]
    fn incoming_churn_keeps_the_map_bounded() {
        let peers = make_peer_states();
        // Peers that connected to us and left, from ever new ports.
        for port in 1..=u16::try_from(3 * MAX_TRACKED_PEERS).unwrap() {
            peers.maybe_prune();
            insert_incoming(&peers, port, Some(PeerState::Dead));
        }
        assert!(peers.states.len() <= MAX_TRACKED_PEERS);
        // Trackers and DHT are still heard.
        let addr = SocketAddr::from(([10, 0, 0, 1], 6881));
        assert!(peers.add_if_not_seen(addr, PeerSource::Dht).is_some());
    }

    #[test]
    fn untrusted_sources_cannot_grow_the_peer_list_without_bound() {
        let peers = make_peer_states();
        for i in 0..u32::try_from(super::MAX_KNOWN_PEERS).unwrap() {
            let addr = SocketAddr::from((std::net::Ipv4Addr::from(0x0a00_0000 + i), 6881));
            assert!(peers.add_if_not_seen(addr, PeerSource::Dht).is_some());
        }
        let extra: SocketAddr = "203.0.113.1:6881".parse().unwrap();
        assert!(peers.add_if_not_seen(extra, PeerSource::Dht).is_none());
        assert!(peers.add_if_not_seen(extra, PeerSource::Pex).is_none());
        assert!(!peers.states.contains_key(&extra));
        // Known peers still record new sources; the user's peers get in.
        let known = SocketAddr::from((std::net::Ipv4Addr::from(0x0a00_0000), 6881));
        assert!(peers.add_if_not_seen(known, PeerSource::Tracker).is_none());
        assert!(peers.with_peer(known, |p| p.sources.contains(PeerSource::Tracker)).unwrap());
        assert!(peers.add_if_not_seen(extra, PeerSource::Manual).is_some());
    }

    // Returns the peer handle, keeping the receiving end of the peer channel alive.
    fn add_live_peer(peers: &PeerStates, addr: &str) -> (PeerHandle, PeerRx) {
        let addr: SocketAddr = addr.parse().unwrap();
        peers.add_if_not_seen(addr, PeerSource::Tracker).unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        peers
            .with_peer_mut(addr, "test", |peer| {
                peer.incoming_connection(Id20::default(), tx, peers, ConnectionKind::Tcp)
            })
            .unwrap()
            .unwrap();
        (addr, rx)
    }

    fn live_seeders(peers: &PeerStates) -> (u32, u32) {
        (
            peers.stats().live_seeders,
            peers.session_stats.snapshot().live_seeders,
        )
    }

    #[test]
    fn full_bitfield_counts_as_seeder_and_is_uncounted_on_disconnect() {
        let peers = make_peer_states();
        let (addr, _rx) = add_live_peer(&peers, "127.0.0.1:1234");
        assert_eq!(live_seeders(&peers), (0, 0));

        peers
            .update_bitfield(addr, empty_bitfield(), TOTAL_PIECES)
            .unwrap();
        assert_eq!(live_seeders(&peers), (0, 0));

        peers
            .update_bitfield(addr, full_bitfield(), TOTAL_PIECES)
            .unwrap();
        assert_eq!(live_seeders(&peers), (1, 1));

        // Repeated updates must not double-count.
        peers
            .update_bitfield(addr, full_bitfield(), TOTAL_PIECES)
            .unwrap();
        assert_eq!(live_seeders(&peers), (1, 1));

        peers.drop_peer(addr).unwrap();
        assert_eq!(live_seeders(&peers), (0, 0));
    }

    #[test]
    fn bitfield_completed_piece_by_piece_counts_as_seeder() {
        let peers = make_peer_states();
        let (addr, _rx) = add_live_peer(&peers, "127.0.0.1:1234");
        peers
            .update_bitfield(addr, empty_bitfield(), TOTAL_PIECES)
            .unwrap();

        for piece in 0..TOTAL_PIECES {
            peers
                .with_live_mut(addr, "test", |live| {
                    *live.bitfield.get_mut(piece).unwrap() = true;
                    peers.update_seeder_flag(live, TOTAL_PIECES);
                })
                .unwrap();
            let expected = u32::from(piece == TOTAL_PIECES - 1);
            assert_eq!(live_seeders(&peers), (expected, expected));
        }
    }

    #[test]
    fn seeder_is_uncounted_when_peer_leaves_live_state() {
        let peers = make_peer_states();
        let (addr, _rx) = add_live_peer(&peers, "127.0.0.1:1234");
        peers
            .update_bitfield(addr, full_bitfield(), TOTAL_PIECES)
            .unwrap();
        assert_eq!(live_seeders(&peers), (1, 1));

        peers.mark_peer_not_needed(addr).unwrap();
        assert_eq!(live_seeders(&peers), (0, 0));
    }

    #[test]
    fn seeders_are_counted_per_peer() {
        let peers = make_peer_states();
        let (a, _rx_a) = add_live_peer(&peers, "127.0.0.1:1234");
        let (b, _rx_b) = add_live_peer(&peers, "127.0.0.1:1235");

        peers
            .update_bitfield(a, full_bitfield(), TOTAL_PIECES)
            .unwrap();
        peers
            .update_bitfield(b, empty_bitfield(), TOTAL_PIECES)
            .unwrap();
        assert_eq!(live_seeders(&peers), (1, 1));

        peers
            .update_bitfield(b, full_bitfield(), TOTAL_PIECES)
            .unwrap();
        assert_eq!(live_seeders(&peers), (2, 2));

        peers.drop_peer(a).unwrap();
        assert_eq!(live_seeders(&peers), (1, 1));
    }

    #[test]
    fn availability_follows_bitfields_and_disconnects() {
        let peers = make_peer_states();
        let (a, _rx_a) = add_live_peer(&peers, "127.0.0.1:1234");
        let (b, _rx_b) = add_live_peer(&peers, "127.0.0.1:1235");
        peers
            .update_bitfield(a, full_bitfield(), TOTAL_PIECES)
            .unwrap();
        peers
            .update_bitfield(b, empty_bitfield(), TOTAL_PIECES)
            .unwrap();
        peers
            .with_live_mut(b, "test", |live| {
                assert!(peers.mark_have(live, 3, TOTAL_PIECES));
                // Repeated haves count once.
                assert!(peers.mark_have(live, 3, TOTAL_PIECES));
                assert!(!peers.mark_have(live, 99, TOTAL_PIECES));
            })
            .unwrap();
        assert_eq!(peers.availability.snapshot(), vec![1, 1, 1, 2, 1, 1, 1, 1]);
        // Replacing a bitfield replaces its contribution.
        peers
            .update_bitfield(a, empty_bitfield(), TOTAL_PIECES)
            .unwrap();
        assert_eq!(peers.availability.snapshot(), vec![0, 0, 0, 1, 0, 0, 0, 0]);
        // Leaving the live state removes it.
        peers.mark_peer_not_needed(b).unwrap();
        assert_eq!(peers.availability.snapshot(), vec![0; 8]);
    }

    #[test]
    fn choking_updates_unchoked_counters() {
        let peers = make_peer_states();
        let (a, mut rx) = add_live_peer(&peers, "127.0.0.1:1234");
        let now = Instant::now();
        let unchoked = |p: &PeerStates| (p.stats().unchoked, p.session_stats.snapshot().unchoked);
        assert_eq!(unchoked(&peers), (0, 0));
        assert!(
            peers
                .with_peer_mut(a, "t", |p| peers.set_choking(p, false, true, now))
                .unwrap()
        );
        assert_eq!(unchoked(&peers), (1, 1));
        assert!(matches!(
            rx.try_recv(),
            Ok(WriterRequest::Message(Message::Unchoke))
        ));
        peers
            .with_live(a, |l| {
                assert!(l.optimistic && !l.am_choking && l.last_optimistic.is_some());
            })
            .unwrap();
        // Promote to a regular unchoke: no message, no counter change.
        assert!(
            !peers
                .with_peer_mut(a, "t", |p| peers.set_choking(p, false, false, now))
                .unwrap()
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(unchoked(&peers), (1, 1));
        // Disconnecting an unchoked peer releases its slot.
        peers.mark_peer_not_needed(a).unwrap();
        assert_eq!(unchoked(&peers), (0, 0));
    }
}
