pub mod stats;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, atomic::Ordering};
use std::time::Instant;

use librqbit_core::hash_id::Id20;
use librqbit_core::lengths::{ChunkInfo, ValidPieceIndex};
use peer_binary_protocol::{Message, Request};

use tokio::sync::{
    Notify,
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};
use tracing::debug;

use crate::peer_connection::WriterRequest;
use crate::stream_connect::ConnectionKind;
use crate::type_aliases::BF;

use super::PeerStates;
use super::peer_details::{PeerSource, PeerSources};

pub(crate) type InflightRequest = ChunkInfo;
pub(crate) type PeerRx = UnboundedReceiver<WriterRequest>;
pub(crate) type PeerTx = UnboundedSender<WriterRequest>;

#[derive(Debug)]
pub(crate) struct Peer {
    pub addr: SocketAddr,
    state: PeerState,
    pub stats: stats::atomic::PeerStats,
    pub outgoing_address: Option<SocketAddr>,
    // Where we learned about this peer.
    pub sources: PeerSources,
    // Set by an explicit disconnect: the peer isn't reconnected when it dies.
    pub disconnect_requested: bool,
}

impl Peer {
    pub fn new_live_for_incoming_connection(
        addr: SocketAddr,
        peer_id: Id20,
        tx: PeerTx,
        counters: &PeerStates,
        connection_kind: ConnectionKind,
    ) -> Self {
        let state = PeerState::Live(Box::new(LivePeerState::new(
            peer_id,
            tx,
            true,
            connection_kind,
            Default::default(),
        )));
        for counter in [&counters.session_stats, &counters.stats] {
            counter.inc(&state);
        }
        Self {
            addr,
            state,
            stats: Default::default(),
            outgoing_address: None,
            sources: PeerSources::from_source(PeerSource::Incoming),
            disconnect_requested: false,
        }
    }

    pub fn new_with_outgoing_address(addr: SocketAddr, source: PeerSource) -> Self {
        Self {
            addr,
            outgoing_address: Some(addr),
            stats: Default::default(),
            state: Default::default(),
            sources: PeerSources::from_source(source),
            disconnect_requested: false,
        }
    }

    // The byte counters a new live connection starts its per-round and rate
    // accounting from.
    fn counters_baseline(&self) -> CountersBaseline {
        CountersBaseline {
            fetched: self.stats.counters.fetched_bytes.load(Ordering::Relaxed),
            uploaded: self.stats.counters.uploaded_bytes.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn reconnect_not_needed_peer(
        &mut self,
        counters: &PeerStates,
    ) -> Option<SocketAddr> {
        if let PeerState::NotNeeded = self.get_state() {
            match self.outgoing_address {
                None => None,
                Some(socket_addr) if self.addr == socket_addr => {
                    self.set_state(PeerState::Queued, counters);
                    Some(socket_addr)
                }
                Some(socket_addr) => {
                    debug!(
                        peer = %self.addr,
                        outgoing_addr = %socket_addr,
                        "peer will by retried on different address",
                    );
                    Some(socket_addr)
                }
            }
        } else {
            None
        }
    }
}

#[derive(Debug, Default)]
pub(crate) enum PeerState {
    #[default]
    // Will be tried to be connected as soon as possible.
    Queued,
    Connecting(PeerTx),
    // Boxed: most peers in the table are not live, keep their entries small.
    Live(Box<LivePeerState>),
    // There was an error, and it's waiting for exponential backoff.
    Dead,
    // We don't need to do anything with the peer any longer.
    // The peer has the full torrent, and we have the full torrent, so no need
    // to keep talking to it.
    NotNeeded,
}

impl std::fmt::Display for PeerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl PeerState {
    pub fn name(&self) -> &'static str {
        match self {
            PeerState::Queued => "queued",
            PeerState::Connecting(_) => "connecting",
            PeerState::Live(_) => "live",
            PeerState::Dead => "dead",
            PeerState::NotNeeded => "not needed",
        }
    }

    pub fn take_live_no_counters(self) -> Option<LivePeerState> {
        match self {
            PeerState::Live(l) => Some(*l),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IncomingConnectionResult {
    #[error("peer already active")]
    AlreadyActive,
}

impl Peer {
    pub fn get_state(&self) -> &PeerState {
        &self.state
    }

    pub fn take_state(&mut self, counters: &PeerStates) -> PeerState {
        self.set_state(Default::default(), counters)
    }

    pub fn destroy(self, counters: &PeerStates) {
        for counter in [&counters.session_stats, &counters.stats] {
            counter.dec(&self.state);
        }
        if let PeerState::Live(l) = &self.state {
            counters.availability.remove_bitfield(&l.bitfield);
        }
        if let (Some(addr), PeerState::Live(..)) = (self.outgoing_address, &self.state) {
            counters.live_outgoing_peers.write().remove(&addr);
        }
    }

    pub fn set_state(&mut self, new: PeerState, counters: &PeerStates) -> PeerState {
        for counter in [&counters.session_stats, &counters.stats] {
            counter.incdec(&self.state, &new);
        }
        // The pieces of a peer leaving the live state are no longer available.
        if let PeerState::Live(l) = &self.state {
            counters.availability.remove_bitfield(&l.bitfield);
        }
        if let Some(addr) = self.outgoing_address {
            if matches!(&self.state, PeerState::Live(..)) {
                counters.live_outgoing_peers.write().remove(&addr);
            }
            if matches!(&new, PeerState::Live(..))
                && self
                    .stats
                    .counters
                    .outgoing_connections
                    .load(Ordering::Relaxed)
                    > 0
            {
                counters.live_outgoing_peers.write().insert(addr);
            }
        }

        std::mem::replace(&mut self.state, new)
    }

    pub fn get_live(&self) -> Option<&LivePeerState> {
        match &self.state {
            PeerState::Live(l) => Some(l.as_ref()),
            _ => None,
        }
    }

    pub fn get_live_mut(&mut self) -> Option<&mut LivePeerState> {
        match &mut self.state {
            PeerState::Live(l) => Some(l.as_mut()),
            _ => None,
        }
    }

    pub fn idle_to_connecting(&mut self, counters: &PeerStates) -> Option<(PeerRx, PeerTx)> {
        match &self.state {
            PeerState::Queued | PeerState::NotNeeded => {
                let (tx, rx) = unbounded_channel();
                let tx_2 = tx.clone();
                self.set_state(PeerState::Connecting(tx), counters);
                Some((rx, tx_2))
            }
            _ => None,
        }
    }

    pub fn incoming_connection(
        &mut self,
        peer_id: Id20,
        tx: PeerTx,
        counters: &PeerStates,
        connection_kind: ConnectionKind,
    ) -> Result<(), IncomingConnectionResult> {
        if matches!(&self.state, PeerState::Connecting(..) | PeerState::Live(..)) {
            return Err(IncomingConnectionResult::AlreadyActive);
        }
        self.sources.insert(PeerSource::Incoming);
        let baseline = self.counters_baseline();
        match self.take_state(counters) {
            PeerState::Queued | PeerState::Dead | PeerState::NotNeeded => {
                self.set_state(
                    PeerState::Live(Box::new(LivePeerState::new(
                        peer_id,
                        tx,
                        true,
                        connection_kind,
                        baseline,
                    ))),
                    counters,
                );
            }
            PeerState::Connecting(..) | PeerState::Live(..) => unreachable!(),
        }
        Ok(())
    }

    pub fn connecting_to_live(
        &mut self,
        peer_id: Id20,
        counters: &PeerStates,
        conn_kind: ConnectionKind,
    ) -> Option<&mut LivePeerState> {
        if let PeerState::Connecting(_) = &self.state {
            let baseline = self.counters_baseline();
            let tx = match self.take_state(counters) {
                PeerState::Connecting(tx) => tx,
                _ => unreachable!(),
            };
            self.set_state(
                PeerState::Live(Box::new(LivePeerState::new(
                    peer_id, tx, false, conn_kind, baseline,
                ))),
                counters,
            );
            self.get_live_mut()
        } else {
            None
        }
    }

    pub fn set_not_needed(&mut self, counters: &PeerStates) -> PeerState {
        self.set_state(PeerState::NotNeeded, counters)
    }
}

/// Byte counters of a peer at the moment its connection went live.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CountersBaseline {
    pub fetched: u64,
    pub uploaded: u64,
}

/// Payload rates of a live peer (libtorrent-style 5 second moving average,
/// updated once a second by the session peer tick).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct PeerRates {
    pub down_bps: u64,
    pub up_bps: u64,
    prev_fetched: u64,
    prev_uploaded: u64,
}

impl PeerRates {
    fn new(baseline: CountersBaseline) -> Self {
        Self {
            down_bps: 0,
            up_bps: 0,
            prev_fetched: baseline.fetched,
            prev_uploaded: baseline.uploaded,
        }
    }

    pub fn update(&mut self, fetched: u64, uploaded: u64, elapsed_ms: u64) {
        let elapsed_ms = elapsed_ms.max(1);
        let down = fetched.saturating_sub(self.prev_fetched) * 1000 / elapsed_ms;
        let up = uploaded.saturating_sub(self.prev_uploaded) * 1000 / elapsed_ms;
        self.prev_fetched = fetched;
        self.prev_uploaded = uploaded;
        self.down_bps = self.down_bps * 4 / 5 + down / 5;
        self.up_bps = self.up_bps * 4 / 5 + up / 5;
    }
}

#[derive(Debug)]
pub(crate) struct LivePeerState {
    pub peer_id: Id20,

    pub client_name: Option<String>,

    pub peer_interested: bool,

    // The peer connected to us.
    pub incoming: bool,
    // We told the peer we are interested.
    pub am_interested: bool,
    // The peer is choking us.
    pub peer_choking: bool,
    // We are choking the peer. Every connection starts choked.
    pub am_choking: bool,
    // The unchoke is an optimistic one.
    pub optimistic: bool,
    pub last_unchoke: Option<Instant>,
    pub last_optimistic: Option<Instant>,
    // counters.uploaded_bytes when we last unchoked the peer.
    pub uploaded_at_unchoke: u64,
    // Counter values at the start of the current choker round.
    pub round_fetched_base: u64,
    pub round_uploaded_base: u64,
    // When the peer last sent us piece data (or the connection went live).
    pub last_data_received: Instant,
    pub rates: PeerRates,
    // This connection is in super-seeding mode (we didn't send our bitfield).
    pub super_seeding: bool,
    // The pieces we revealed to the peer while super seeding (newest first).
    pub superseed_pieces: super::superseed::SuperseedPieces,

    // This is used to track the pieces the peer has.
    pub bitfield: BF,

    // Whether the bitfield above covers the whole torrent, i.e. the peer is a seeder.
    // Cached so that the aggregate seeder counter can be maintained on transitions
    // instead of scanning every peer's bitfield on each stats call. Kept in sync
    // with the bitfield by PeerStates::update_seeder_flag.
    pub seeder: bool,

    // When the peer sends us data this is used to track if we asked for it.
    inflight_requests: HashSet<InflightRequest>,

    // Bounded tolerance for chunks that arrive after we cancel requests.
    // This is intentionally approximate: we track a count instead of storing every
    // canceled chunk, so a peer is not disconnected for racing our cancel messages.
    late_cancelled_request_tolerance: u32,

    request_slots_changed: Arc<Notify>,

    // The main channel to send requests to peer.
    pub tx: PeerTx,

    pub connection_kind: ConnectionKind,

    /// The connection uses MSE/PE with RC4.
    pub encrypted: bool,
}

impl LivePeerState {
    pub fn new(
        peer_id: Id20,
        tx: PeerTx,
        incoming: bool,
        connection_kind: ConnectionKind,
        baseline: CountersBaseline,
    ) -> Self {
        LivePeerState {
            peer_id,
            client_name: None,
            peer_interested: false,
            incoming,
            am_interested: false,
            peer_choking: true,
            am_choking: true,
            optimistic: false,
            last_unchoke: None,
            last_optimistic: None,
            uploaded_at_unchoke: baseline.uploaded,
            round_fetched_base: baseline.fetched,
            round_uploaded_base: baseline.uploaded,
            last_data_received: Instant::now(),
            rates: PeerRates::new(baseline),
            super_seeding: false,
            superseed_pieces: [None, None],
            bitfield: BF::default(),
            seeder: false,
            inflight_requests: Default::default(),
            late_cancelled_request_tolerance: 0,
            request_slots_changed: Default::default(),
            tx,
            connection_kind,
            encrypted: false,
        }
    }

    pub fn has_full_torrent(&self, total_pieces: usize) -> bool {
        self.bitfield.get(0..total_pieces).is_some_and(|s| s.all())
    }

    pub fn request_slots_changed(&self) -> Arc<Notify> {
        self.request_slots_changed.clone()
    }

    pub fn requested_inflight_count(&self) -> usize {
        self.inflight_requests.len()
    }

    pub fn add_inflight_request(&mut self, chunk: ChunkInfo) -> bool {
        self.inflight_requests.insert(chunk)
    }

    pub fn remove_inflight_request(&mut self, chunk: &ChunkInfo) -> RemoveInflightRequestResult {
        if self.inflight_requests.remove(chunk) {
            self.request_slots_changed.notify_waiters();
            return RemoveInflightRequestResult::Expected;
        }

        if self.late_cancelled_request_tolerance > 0 {
            // This may be any unexpected chunk, not necessarily the exact canceled
            // one, but the tolerance is bounded by cancels we sent.
            self.late_cancelled_request_tolerance -= 1;
            RemoveInflightRequestResult::LateCanceled
        } else {
            RemoveInflightRequestResult::Unexpected
        }
    }

    pub fn cancel_inflight_requests_for_piece(&mut self, piece: ValidPieceIndex) {
        let tx = &self.tx;
        let late_cancelled_request_tolerance = &mut self.late_cancelled_request_tolerance;
        let before = self.inflight_requests.len();
        self.inflight_requests.retain(|req| {
            if req.piece_index == piece {
                let _ = tx.send(WriterRequest::Message(Message::Cancel(Request {
                    index: piece.get(),
                    begin: req.offset,
                    length: req.size,
                })));
                *late_cancelled_request_tolerance += 1;
                false
            } else {
                true
            }
        });

        if self.inflight_requests.len() != before {
            self.request_slots_changed.notify_waiters();
        }
    }

    pub fn inflight_requests(&self) -> impl Iterator<Item = &InflightRequest> {
        self.inflight_requests.iter()
    }

    pub fn inflight_requests_debug(&self) -> &HashSet<InflightRequest> {
        &self.inflight_requests
    }
}

pub(crate) enum RemoveInflightRequestResult {
    Expected,
    LateCanceled,
    Unexpected,
}
