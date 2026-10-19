// The main logic of rqbit is here - connecting to peers, reading and writing messages
// to them, tracking peer state etc.
//
// ## Architecture
// There are many tasks cooperating to download the torrent. Tasks communicate both with message passing
// and shared memory.
//
// ### Shared locked state
// Shared state is access by almost all actors through RwLocks.
//
// There's one source of truth (TorrentStateLocked) for which chunks we have, need, and what peers are we waiting them from.
//
// Peer states that are important to the outsiders (tasks other than manage_peer) are in a sharded hash-map (DashMap)
//
// ### Tasks (actors)
// Peer adder task:
// - spawns new peers as they become known. It pulls them from a queue. The queue is filled in by DHT and torrent trackers.
//   Also gets updated when peers are reconnecting after errors.
//
// Each peer has one main task "manage_peer". It's composed of 2 futures running as one task through tokio::select:
// - "manage_peer" - this talks to the peer over network and calls callbacks on PeerHandler. The callbacks are not async,
//   and are supposed to finish quickly (apart from writing to disk, which is accounted for as "spawn_blocking").
// - "peer_chunk_requester" - this continuously sends requests for chunks to the peer.
//   it may steal chunks/pieces from other peers.
//
// ## Peer lifecycle
// State transitions:
// - queued (initial state) -> connected
// - connected -> live
// - ANY STATE -> dead (on error)
// - ANY STATE -> not_needed (when we don't need to talk to the peer anymore)
//
// When the peer dies, it's rescheduled with exponential backoff.
//
// > NOTE: deadlock notice:
// > peers and stateLocked are behind 2 different locks.
// > if you lock them in different order, this may deadlock.
// >
// > so don't lock them both at the same time at all, or at the worst lock them in the
// > same order (peers one first, then the global one).

pub(crate) mod availability;
pub mod choker;
pub mod peer;
pub mod peer_details;
pub mod peers;
mod piece_info;
pub mod priorities;
pub mod stats;
mod superseed;
mod upload_queue;
mod v2_hashes;
mod v2_missing;
mod webseed;

use std::{
    borrow::Cow,
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use buffers::{ByteBuf, ByteBufOwned};
use clone_to_owned::CloneToOwned;
use librqbit_core::{
    constants::CHUNK_SIZE,
    hash_id::Id20,
    lengths::{ChunkInfo, Lengths, ValidPieceIndex},
    spawn_utils::spawn_with_cancel,
    speed_estimator::SpeedEstimator,
    torrent_metainfo::ValidatedTorrentMetaV1Info,
};
use parking_lot::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use peer_binary_protocol::{
    HashRequest, Handshake, Message, Piece, Request,
    extended::{
        self, ExtendedMessage,
        handshake::ExtendedHandshake,
        ut_metadata::{UtMetadata, UtMetadataData},
        ut_pex::UtPex,
    },
};
use tokio::sync::{
    Notify,
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, debug_span, error, info, trace, warn};

use crate::{
    Error,
    chunk_tracker::{ChunkMarkingResult, ChunkTracker, HaveNeededSelected},
    conn_limiter::{ConnLimiter, ConnPermit},
    file_ops::FileOps,
    net::external_ip::IpVoteSource,
    peer_connection::{
        PeerConnection, PeerConnectionHandler, PeerConnectionOptions, WriterRequest,
    },
    peer_policy::{estimate_ip_overhead, is_lan_addr},
    piece_tracker::{AcquireRequest, AcquireResult, PieceTracker},
    session::CheckedIncomingConnection,
    session_stats::SessionStats,
    stream_connect::ConnectionKind,
    torrent_state::{peer::Peer, utils::atomic_inc},
    type_aliases::{BF, BS, FileStorage, PeerHandle},
};

use self::{
    availability::PieceAvailability,
    peer::{
        PeerRx, PeerState, PeerTx, RemoveInflightRequestResult,
        stats::{
            atomic::PeerCountersAtomic as AtomicPeerCounters,
            snapshot::{PeerStatsFilter, PeerStatsSnapshot},
        },
    },
    peer_details::PeerSource,
    peers::PeerStates,
    priorities::compute_piece_priorities,
    stats::{atomic::AtomicStats, snapshot::StatsSnapshot},
    superseed::superseed_allows,
    upload_queue::{
        MAX_QUEUED_HASH_REPLIES, MAX_QUEUED_METADATA_PIECES, MAX_UPLOAD_REQUESTS, PushResult,
        UploadQueue,
    },
};

use super::{
    ManagedTorrentShared, TorrentMetadata,
    paused::TorrentStatePaused,
    streaming::TorrentStreams,
    utils::{TimedExistence, timeit},
};

pub(crate) struct TorrentStateLocked {
    // Coordinates piece state: what chunks we have, need, and what pieces are in-flight.
    // If this is None, the torrent was paused, and this live state is useless, and needs to be dropped.
    pub(crate) pieces: Option<PieceTracker>,

    // Download priority (0..=7) of every piece, derived from file priorities.
    pub(crate) piece_priorities: Box<[u8]>,

    // A pure v2 torrent missing piece layers: pieces that can't be verified
    // yet (their file's layer is unknown), never handed to a peer or web
    // seed. Cleared file by file as layers arrive; `None` once none is left.
    pub(crate) v2_blocked: Option<BF>,

    // If this is None, then it was already used
    fatal_errors_tx: Option<tokio::sync::oneshot::Sender<anyhow::Error>>,

    unflushed_bitv_bytes: u64,
}

impl TorrentStateLocked {
    pub(crate) fn get_chunks(&self) -> crate::Result<&ChunkTracker> {
        self.pieces
            .as_ref()
            .map(|p| p.chunks())
            .ok_or(Error::ChunkTrackerEmpty)
    }

    pub(crate) fn get_pieces(&self) -> crate::Result<&PieceTracker> {
        self.pieces.as_ref().ok_or(Error::ChunkTrackerEmpty)
    }

    pub(crate) fn get_pieces_mut(&mut self) -> crate::Result<&mut PieceTracker> {
        self.pieces.as_mut().ok_or(Error::ChunkTrackerEmpty)
    }

    fn try_flush_bitv(&mut self, shared: &ManagedTorrentShared, flush_async: bool) {
        if self.unflushed_bitv_bytes == 0 {
            return;
        }
        trace!("trying to flush bitfield");
        if let Some(Err(e)) = self
            .pieces
            .as_mut()
            .map(|pt| pt.flush_have_pieces(flush_async))
        {
            warn!(id=?shared.id, info_hash = ?shared.info_hash, "error flushing bitfield: {e:#}");
        } else {
            trace!("flushed bitfield");
            self.unflushed_bitv_bytes = 0;
        }
    }
}

/// Whether `piece` is held back (`TorrentStateLocked::v2_blocked`).
pub(crate) fn is_blocked(blocked: Option<&BS>, piece: ValidPieceIndex) -> bool {
    blocked.is_some_and(|b| b.get(piece.get_usize()).is_some_and(|v| *v))
}

const FLUSH_BITV_EVERY_BYTES: u64 = 16 * 1024 * 1024;

pub enum AddIncomingPeerResult {
    Added,
    AlreadyActive,
    ConcurrencyLimitReached,
}

// The connection slots (torrent and session-wide) a peer connection holds.
struct PeerPermits {
    _torrent: ConnPermit,
    _session: Option<ConnPermit>,
}

pub struct TorrentStateLive {
    peers: PeerStates,
    pub(crate) shared: Arc<ManagedTorrentShared>,
    metadata: Arc<TorrentMetadata>,
    _locked: RwLock<TorrentStateLocked>,

    pub(crate) files: FileStorage,

    // Striped: piece `i` uses lock `i % len`. Stealing only try-locks, so a
    // shared stripe can at worst keep two pieces from being stolen at once.
    per_piece_locks: Box<[RwLock<()>]>,

    stats: AtomicStats,
    lengths: Lengths,

    // Limits how many active (occupying network resources) peers there are at a moment in time.
    peer_limiter: ConnLimiter,

    // The queue for peer manager to connect to them.
    peer_queue_tx: UnboundedSender<SocketAddr>,

    finished_notify: Notify,
    new_pieces_notify: Notify,

    down_speed_estimator: SpeedEstimator,
    /// The speed sampler task of this torrent sleeps (no timer) while the
    /// torrent moves no data; the session's 1 s peer tick then samples for it
    /// and wakes it as soon as bytes move (see `estimator_idle_tick`).
    estimator_parked: std::sync::atomic::AtomicBool,
    estimator_wake: Arc<Notify>,
    estimator_parked_moved: std::sync::Mutex<(u64, u64)>,
    up_speed_estimator: SpeedEstimator,
    cancellation_token: CancellationToken,

    session_stats: Arc<SessionStats>,

    pub(crate) streams: Arc<TorrentStreams>,
    have_broadcast_tx: tokio::sync::broadcast::Sender<ValidPieceIndex>,


    // Seed of the piece picker's rarest-first tie-break.
    picker_seed: u32,

    // BEP 52 hash requests (pure v2 torrents).
    v2_hashes: v2_hashes::V2HashServer,
}

/// Upper bound of `per_piece_locks`; one 8-byte lock per piece cost 48 KB for
/// a 6000-piece torrent.
const PIECE_LOCK_STRIPES: u32 = 128;

/// Shards of the per-torrent peer map. dashmap's default is 4 per CPU, each
/// cache-line padded (about 10 KB per torrent), and a torrent has few peers.
const PEER_SHARDS: usize = 4;

impl TorrentStateLive {
    fn piece_lock(&self, piece: usize) -> &RwLock<()> {
        &self.per_piece_locks[piece % self.per_piece_locks.len()]
    }

    fn max_request_window(&self) -> usize {
        self.shared
            .options
            .peer_max_request_window
            .unwrap_or(DEFAULT_PEER_REQUEST_WINDOW)
            .max(1)
    }

    pub(crate) fn new(
        paused: TorrentStatePaused,
        file_priorities: &[u8],
        fatal_errors_tx: tokio::sync::oneshot::Sender<anyhow::Error>,
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<Arc<Self>> {
        let (peer_queue_tx, peer_queue_rx) = unbounded_channel();
        let session = paused
            .shared
            .session
            .upgrade()
            .context("session is dead, cannot start torrent")?;
        let session_stats = session.stats.clone();
        let down_speed_estimator = SpeedEstimator::default();
        let up_speed_estimator = SpeedEstimator::default();

        let have_bytes = paused.chunk_tracker.get_hns().have_bytes;
        let lengths = *paused.chunk_tracker.get_lengths();

        let piece_priorities = compute_piece_priorities(
            lengths.total_pieces() as usize,
            lengths.default_piece_length(),
            &paused.metadata.file_infos,
            file_priorities,
            paused.shared.runtime.first_last_piece_priority(),
        );

        let (have_broadcast_tx, _) = tokio::sync::broadcast::channel(128);

        let state = Arc::new(TorrentStateLive {
            shared: paused.shared.clone(),
            metadata: paused.metadata.clone(),
            peers: PeerStates {
                session_stats: session_stats.peers.clone(),
                stats: Default::default(),
                states: dashmap::DashMap::with_shard_amount(PEER_SHARDS),
                live_outgoing_peers: Default::default(),
                availability: PieceAvailability::new(lengths.total_pieces() as usize),
                prune_floor: Default::default(),
            },
            _locked: RwLock::new(TorrentStateLocked {
                pieces: Some(PieceTracker::new(paused.chunk_tracker)),
                piece_priorities,
                v2_blocked: super::missing_layers::unknown_pieces(&paused.metadata.info),
                fatal_errors_tx: Some(fatal_errors_tx),
                unflushed_bitv_bytes: 0,
            }),
            files: paused.files,
            stats: AtomicStats {
                have_bytes: AtomicU64::new(have_bytes),
                ..Default::default()
            },
            lengths,
            peer_limiter: ConnLimiter::new(paused.shared.runtime.peer_limit()),
            new_pieces_notify: Notify::new(),
            peer_queue_tx,
            finished_notify: Notify::new(),
            down_speed_estimator,
            up_speed_estimator,
            cancellation_token,
            have_broadcast_tx,
            session_stats,
            streams: paused.streams,
            per_piece_locks: (0..lengths.total_pieces().clamp(1, PIECE_LOCK_STRIPES))
                .map(|_| RwLock::new(()))
                .collect(),
            picker_seed: rand::random(),
            v2_hashes: Default::default(),
            estimator_parked: Default::default(),
            estimator_wake: Default::default(),
            estimator_parked_moved: Default::default(),
        });

        state.spawn(
            debug_span!(parent: state.shared.span.clone(), "speed_estimator_updater"),
            format!("[{}]speed_estimator_updater", state.shared.id),
            {
                let state = Arc::downgrade(&state);
                async move {
                    // Idle torrents (no bytes moved for a while) are sampled
                    // once a second instead of ten times a second: with
                    // hundreds of torrents the timers alone kept the process
                    // busy. Sampling returns to 100 ms as soon as data moves.
                    let mut last_moved = (u64::MAX, u64::MAX);
                    let mut idle_ticks = 0u32;
                    loop {
                        let state = match state.upgrade() {
                            Some(state) => state,
                            None => return Ok(()),
                        };
                        let moved = state.sample_speed(Instant::now())?;
                        if moved == last_moved {
                            idle_ticks = idle_ticks.saturating_add(1);
                        } else {
                            idle_ticks = 0;
                            last_moved = moved;
                        }
                        if idle_ticks > 10 {
                            // Idle: stop the timer. The session's peer tick
                            // samples once a second and wakes this task when
                            // data moves.
                            let wake = state.estimator_wake.clone();
                            *state.estimator_parked_moved.lock().unwrap() = moved;
                            state.estimator_parked.store(true, Ordering::Release);
                            drop(state);
                            wake.notified().await;
                            idle_ticks = 0;
                            continue;
                        }
                        drop(state);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            },
        );

        // Layers that became known while the torrent wasn't live: the data
        // on disk for them is checked before those pieces are picked.
        if let Some(extra) = state.v2_known_unverified() {
            {
                let mut g = state.lock_write("v2_blocked");
                match g.v2_blocked.as_mut() {
                    Some(b) => *b |= &extra[..],
                    None => g.v2_blocked = Some(extra.clone()),
                }
            }
            state.check_and_unblock(extra.iter_ones().map(|p| p..p + 1).collect());
        }

        state.spawn(
            debug_span!(parent: state.shared.span.clone(), "peer_adder"),
            format!("[{}]peer_adder", state.shared.id),
            state.clone().task_peer_adder(peer_queue_rx),
        );

        state.spawn(
            debug_span!(parent: state.shared.span.clone(), "webseeds"),
            format!("[{}]webseeds", state.shared.id),
            state.clone().task_webseeds(),
        );

        // Peers added while the torrent wasn't live.
        for (addr, source) in state.shared.runtime.take_pending_peers() {
            state.add_peer_with_source(addr, source)?;
        }
        Ok(state)
    }

    /// Feeds both speed estimators; returns (fetched, uploaded) bytes.
    fn sample_speed(&self, now: Instant) -> crate::Result<(u64, u64)> {
        let stats = self.stats_snapshot();
        let remaining = self
            .lock_read("get_remaining_bytes")
            .get_chunks()?
            .get_remaining_bytes();
        self.down_speed_estimator
            .add_snapshot(stats.fetched_bytes, Some(remaining), now);
        self.up_speed_estimator
            .add_snapshot(stats.uploaded_bytes, None, now);
        Ok((stats.fetched_bytes, stats.uploaded_bytes))
    }

    /// Whether the speed sampler task is parked and both speeds read zero:
    /// nothing for the session peer tick to sample.
    pub(crate) fn estimator_settled(&self) -> bool {
        self.estimator_parked.load(Ordering::Acquire)
            && self.down_speed_estimator.bps() == 0
            && self.up_speed_estimator.bps() == 0
    }

    /// Starts fresh speed windows after the session peer tick was parked, so
    /// the first samples are not averaged over the parked time.
    pub(crate) fn estimator_rebase(&self, now: Instant) {
        if !self.estimator_parked.load(Ordering::Acquire) {
            return;
        }
        self.down_speed_estimator.reset();
        self.up_speed_estimator.reset();
        if let Ok(moved) = self.sample_speed(now) {
            *self.estimator_parked_moved.lock().unwrap() = moved;
        }
    }

    /// Called once a second by the session for every live torrent: while the
    /// torrent's own sampler task is parked, keeps the 1 s samples going and
    /// wakes the task when data moves.
    pub(crate) fn estimator_idle_tick(&self, now: Instant) {
        if !self.estimator_parked.load(Ordering::Acquire) {
            return;
        }
        let Ok(moved) = self.sample_speed(now) else {
            return;
        };
        let mut last = self.estimator_parked_moved.lock().unwrap();
        if *last != moved {
            *last = moved;
            self.estimator_parked.store(false, Ordering::Release);
            self.estimator_wake.notify_one();
        }
    }

    // A connection slot of this torrent and of the session, if both are free.
    fn try_acquire_peer_permits(&self) -> Option<PeerPermits> {
        let torrent = self.peer_limiter.try_acquire()?;
        let session = match self.shared.session.upgrade() {
            Some(s) => Some(s.peer_policy.connections.try_acquire()?),
            None => None,
        };
        Some(PeerPermits {
            _torrent: torrent,
            _session: session,
        })
    }

    /// Change the connection limit of this live torrent. When lowered below
    /// the current number of connections, the least useful peers are
    /// disconnected (they may be retried later, within the limit).
    pub(crate) fn set_peer_limit(&self, limit: Option<usize>) {
        self.peer_limiter.set_limit(limit);
        let Some(limit) = limit else {
            return;
        };
        let stats = self.peers.stats.snapshot();
        let active = (stats.live + stats.connecting) as usize;
        if active <= limit {
            return;
        }
        let mut excess = active - limit;
        // Connecting peers go first, then live ones that are of no use to
        // either side, then the slowest.
        let mut victims: Vec<(u8, u64, PeerHandle)> = self
            .peers
            .states
            .iter()
            .filter_map(|e| match e.value().get_state() {
                PeerState::Connecting(_) => Some((0, 0, *e.key())),
                PeerState::Live(l) => {
                    let useful = l.am_interested || l.peer_interested;
                    Some((
                        1 + u8::from(useful),
                        l.rates.down_bps + l.rates.up_bps,
                        *e.key(),
                    ))
                }
                _ => None,
            })
            .collect();
        victims.sort_unstable();
        for (_, _, addr) in victims {
            if excess == 0 {
                break;
            }
            if self.kick_peer(addr, false) {
                excess -= 1;
            }
        }
    }

    // Close the connection to a peer. With `forget`, it is not reconnected.
    pub(crate) fn kick_peer(&self, addr: PeerHandle, forget: bool) -> bool {
        self.peers
            .with_peer_mut(addr, "kick_peer", |p| {
                let tx = match p.get_state() {
                    PeerState::Live(l) => l.tx.clone(),
                    PeerState::Connecting(tx) => tx.clone(),
                    _ => return false,
                };
                if forget {
                    p.disconnect_requested = true;
                }
                tx.send(WriterRequest::Disconnect(Ok(()))).is_ok()
            })
            .unwrap_or(false)
    }

    fn add_wasted(&self, failed: u64, redundant: u64) {
        let runtime = &self.shared.runtime;
        let counters = &self.session_stats.counters;
        if failed > 0 {
            runtime
                .wasted_failed_bytes
                .fetch_add(failed, Ordering::Relaxed);
            counters
                .wasted_failed_bytes
                .fetch_add(failed, Ordering::Relaxed);
        }
        if redundant > 0 {
            runtime
                .wasted_redundant_bytes
                .fetch_add(redundant, Ordering::Relaxed);
            counters
                .wasted_redundant_bytes
                .fetch_add(redundant, Ordering::Relaxed);
        }
    }

    #[track_caller]
    pub(crate) fn spawn(
        &self,
        span: tracing::Span,
        name: impl Into<Cow<'static, str>>,
        fut: impl std::future::Future<Output = crate::Result<()>> + Send + 'static,
    ) {
        spawn_with_cancel(span, name, self.cancellation_token.clone(), fut);
    }

    pub fn down_speed_estimator(&self) -> &SpeedEstimator {
        &self.down_speed_estimator
    }

    pub fn up_speed_estimator(&self) -> &SpeedEstimator {
        &self.up_speed_estimator
    }

    pub(crate) fn add_incoming_peer(
        self: &Arc<Self>,
        checked_peer: CheckedIncomingConnection,
    ) -> anyhow::Result<AddIncomingPeerResult> {
        use dashmap::mapref::entry::Entry;
        let (tx, rx) = unbounded_channel();
        let permit = match self.try_acquire_peer_permits() {
            Some(permit) => permit,
            None => {
                debug!("limit of live peers reached, dropping incoming peer");
                self.peers.with_peer(checked_peer.addr, |p| {
                    atomic_inc(&p.stats.counters.incoming_connections);
                });
                return Ok(AddIncomingPeerResult::ConcurrencyLimitReached);
            }
        };

        // Every new incoming address adds an entry; keep the map bounded
        // (before taking the entry: pruning visits every shard).
        self.peers.maybe_prune();
        let counters = match self.peers.states.entry(checked_peer.addr) {
            Entry::Occupied(mut occ) => {
                let peer = occ.get_mut();
                if let Err(e) = peer.incoming_connection(
                    checked_peer.handshake.peer_id,
                    tx.clone(),
                    &self.peers,
                    checked_peer.kind,
                ) {
                    match e {
                        peer::IncomingConnectionResult::AlreadyActive => {
                            debug!(
                                addr = %checked_peer.addr,
                                kind = %checked_peer.kind,
                                "peer already active, ignoring incoming connection"
                            );
                            return Ok(AddIncomingPeerResult::AlreadyActive);
                        }
                    }
                }
                peer.stats.counters.clone()
            }
            Entry::Vacant(vac) => {
                atomic_inc(&self.peers.stats.seen);
                let peer = Peer::new_live_for_incoming_connection(
                    *vac.key(),
                    checked_peer.handshake.peer_id,
                    tx.clone(),
                    &self.peers,
                    checked_peer.kind,
                );
                let counters = peer.stats.counters.clone();
                vac.insert(peer);
                counters
            }
        };
        atomic_inc(&counters.incoming_connections);
        self.session_stats.wake_peer_tick();

        self.spawn(
            debug_span!(
                parent: self.shared.span.clone(),
                "manage_incoming_peer",
                addr = %checked_peer.addr
            ),
            format!(
                "[{}][addr={}]manage_incoming_peer",
                self.shared.id, checked_peer.addr
            ),
            aframe!(
                self.clone()
                    .task_manage_incoming_peer(checked_peer, counters, tx, rx, permit)
            ),
        );
        Ok(AddIncomingPeerResult::Added)
    }

    async fn task_manage_incoming_peer(
        self: Arc<Self>,
        checked_peer: CheckedIncomingConnection,
        counters: Arc<AtomicPeerCounters>,
        tx: PeerTx,
        rx: PeerRx,
        permit: PeerPermits,
    ) -> crate::Result<()> {
        let upgrade_to_v2 = self.shared.info_hash_alt.is_some_and(|alt| {
            checked_peer.handshake.info_hash == alt
                || (checked_peer.handshake.info_hash == self.shared.info_hash
                    && checked_peer.handshake.supports_v2())
        });
        let handler = PeerHandler {
            addr: checked_peer.addr,
            incoming: true,
            is_lan: is_lan_addr(&checked_peer.addr),
            is_utp: AtomicBool::new(matches!(checked_peer.kind, ConnectionKind::Utp)),
            is_ipv6: checked_peer.addr.is_ipv6(),
            on_bitfield_notify: Default::default(),
            flow_control: Mutex::new(PeerFlowControl::new(self.max_request_window())),
            state: self.clone(),
            tx,
            counters,
            first_message_received: AtomicBool::new(false),
            pex: Default::default(),
            uploads: Default::default(),
            queued_metadata_pieces: AtomicU32::new(0),
            queued_hash_replies: AtomicU32::new(0),
            cancel_token: self.cancellation_token.child_token(),
            client_name_and_version: self.shared.client_name_and_version().to_owned(),
            // The peer used our v2 hash (a hybrid torrent's other hash), or
            // our v1 hash with the v2 bit (it supports upgrading the
            // connection to v2): we answer with the v2 hash, like libtorrent.
            v2_hash: AtomicBool::new(upgrade_to_v2),
        };
        let _token_guard = handler.cancel_token.clone().drop_guard();
        let options = PeerConnectionOptions {
            connect_timeout: self.shared.options.peer_connect_timeout,
            read_write_timeout: self.shared.options.peer_read_write_timeout,
            ..Default::default()
        };
        // The session only hands us handshakes with one of our hashes.
        let hash = match self.shared.info_hash_alt {
            Some(alt) if upgrade_to_v2 => alt,
            _ => self.shared.info_hash,
        };
        let peer_connection = PeerConnection::new(
            checked_peer.addr,
            hash,
            self.our_peer_id(),
            &handler,
            Some(options),
            self.shared.spawner.clone(),
            self.shared.connector.clone(),
        );
        let requester = handler.task_peer_chunk_requester();

        let res = tokio::select! {
            r = requester => {r}
            r = peer_connection.manage_peer_incoming(
                rx,
                checked_peer,
                self.have_broadcast_tx.subscribe()
            ) => {r}
        };

        match res {
            // We disconnected the peer ourselves as we don't need it
            Ok(()) => {
                handler.on_peer_died(None)?;
            }
            Err(e) => {
                debug!("error managing peer: {:#}", e);
                handler.on_peer_died(Some(e))?;
            }
        };
        drop(permit);
        Ok(())
    }

    async fn task_manage_outgoing_peer(
        self: Arc<Self>,
        addr: SocketAddr,
        permit: PeerPermits,
    ) -> crate::Result<()> {
        let state = self;
        let (rx, tx) = state.peers.mark_peer_connecting(addr)?;
        let counters = state
            .peers
            .with_peer(addr, |p| p.stats.counters.clone())
            .ok_or(Error::BugPeerNotFound)?;
        // Peers found under a hybrid torrent's v2 hash are connected to with
        // it (libtorrent's `protocol_v2` peers); everyone else with the v1 hash.
        let v2_alt = state
            .shared
            .info_hash_alt
            .filter(|_| state.shared.tracker_manager.peer_sources.learned_v2(addr));

        let handler = PeerHandler {
            addr,
            incoming: false,
            is_lan: is_lan_addr(&addr),
            is_utp: AtomicBool::new(false),
            is_ipv6: addr.is_ipv6(),
            on_bitfield_notify: Default::default(),
            flow_control: Mutex::new(PeerFlowControl::new(state.max_request_window())),
            state: state.clone(),
            tx,
            counters,
            first_message_received: AtomicBool::new(false),
            pex: Default::default(),
            uploads: Default::default(),
            queued_metadata_pieces: AtomicU32::new(0),
            queued_hash_replies: AtomicU32::new(0),
            cancel_token: state.cancellation_token.child_token(),
            client_name_and_version: state.shared.client_name_and_version().to_owned(),
            v2_hash: AtomicBool::new(v2_alt.is_some()),
        };
        let _token_guard = handler.cancel_token.clone().drop_guard();

        let options = PeerConnectionOptions {
            connect_timeout: state.shared.options.peer_connect_timeout,
            read_write_timeout: state.shared.options.peer_read_write_timeout,
            ..Default::default()
        };
        let peer_connection = PeerConnection::new(
            addr,
            v2_alt.unwrap_or(state.shared.info_hash),
            state.our_peer_id(),
            &handler,
            Some(options),
            state.shared.spawner.clone(),
            state.shared.connector.clone(),
        );
        let requester = aframe!(
            handler
                .task_peer_chunk_requester()
                .instrument(debug_span!("chunk_requester"))
        );
        let conn_manager = aframe!(
            peer_connection
                .manage_peer_outgoing(rx, state.have_broadcast_tx.subscribe())
                .instrument(debug_span!("peer_connection"))
        );

        handler
            .counters
            .outgoing_connection_attempts
            .fetch_add(1, Ordering::Relaxed);
        let res = tokio::select! {
            r = requester => {r}
            r = conn_manager => {r}
        };

        match res {
            // We disconnected the peer ourselves as we don't need it
            Ok(()) => {
                handler.on_peer_died(None)?;
            }
            Err(e) => {
                debug!("error managing peer: {:#}", e);
                handler.on_peer_died(Some(e))?;
            }
        }
        drop(permit);
        Ok(())
    }

    async fn task_peer_adder(
        self: Arc<Self>,
        mut peer_queue_rx: UnboundedReceiver<SocketAddr>,
    ) -> crate::Result<()> {
        let state = self;
        loop {
            let addr = peer_queue_rx.recv().await.ok_or(Error::TorrentIsNotLive)?;
            if state.shared.options.disable_upload() && state.is_finished_and_no_active_streams() {
                debug!(?addr, "ignoring peer as we are finished");
                state.peers.mark_peer_not_needed(addr);
                continue;
            }

            let session = state
                .shared
                .session
                .upgrade()
                .ok_or(Error::SessionDestroyed)?;

            if session.ipv4_only && addr.is_ipv6() && !crate::i2p::is_i2p_addr(addr) {
                debug!(?addr, "skipping ipv6 peer (ipv4_only=true)");
                continue;
            }

            if addr.port() == 0 {
                debug!(?addr, "skipping peer with port 0");
                continue;
            }

            if session.blocklist.has(crate::net::filter::canonical_ip(addr.ip())) {
                session
                    .stats
                    .counters
                    .blocked_outgoing
                    .fetch_add(1, Ordering::Relaxed);
                debug!(?addr, "blocked outgoing connection (by the blacklist)");
                continue;
            }

            if session
                .allowlist
                .as_ref()
                .is_some_and(|l| !l.has(crate::net::filter::canonical_ip(addr.ip())))
            {
                session
                    .stats
                    .counters
                    .blocked_outgoing
                    .fetch_add(1, Ordering::Relaxed);
                debug!(?addr, "blocked outgoing connection (by the allowlist)");
                continue;
            }

            if session.peer_filter.check_and_report(addr, false) {
                session
                    .stats
                    .counters
                    .blocked_outgoing
                    .fetch_add(1, Ordering::Relaxed);
                debug!(?addr, "blocked outgoing connection (IP filter)");
                continue;
            }

            if !session
                .i2p
                .mixed_allows_trackers(&state.shared.tracker_manager, addr)
            {
                session.peer_filter.report(
                    addr,
                    crate::net::PeerBlockReason::I2pMixedMode,
                    false,
                );
                continue;
            }

            // Don't keep the session alive while waiting for connection slots.
            let session_connections = session.peer_policy.connections.clone();
            let connect_limiter = session.connect_limiter.clone();
            drop(session);
            let torrent_permit = state.peer_limiter.acquire().await?;
            let session_permit = session_connections.acquire().await?;
            // Session-wide pacing of connection attempts ("connection speed").
            connect_limiter.acquire().await;
            let permit = PeerPermits {
                _torrent: torrent_permit,
                _session: Some(session_permit),
            };
            state.spawn(
                debug_span!(parent: state.shared.span.clone(), "manage_peer", peer = ?addr),
                format!("[{}][addr={addr}]manage_peer", state.shared.id),
                aframe!(state.clone().task_manage_outgoing_peer(addr, permit)),
            );
        }
    }

    pub fn torrent(&self) -> &ManagedTorrentShared {
        &self.shared
    }

    pub fn info(&self) -> &ValidatedTorrentMetaV1Info<ByteBufOwned> {
        &self.metadata.info
    }
    pub fn info_hash(&self) -> Id20 {
        self.shared.info_hash
    }
    pub fn peer_id(&self) -> Id20 {
        self.shared.peer_id
    }

    /// The peer id for new connections (a generic one in anonymous mode).
    pub(crate) fn our_peer_id(&self) -> Id20 {
        self.shared
            .session
            .upgrade()
            .map(|s| s.effective_peer_id())
            .unwrap_or(self.shared.peer_id)
    }

    /// Whether PeX is on for the session and the torrent (and the torrent
    /// isn't private).
    fn pex_enabled(&self) -> bool {
        !self.metadata.info.info().private
            && !self.shared.tracker_manager.disabled_discovery().pex
            && self
                .shared
                .session
                .upgrade()
                .is_some_and(|s| s.pex_enabled())
    }

    /// Disconnects (without reconnecting) the peers `filter` now blocks and
    /// reports them. Returns how many were disconnected.
    pub(crate) fn disconnect_blocked_peers(&self, filter: &crate::net::filter::PeerFilter) -> usize {
        let victims: Vec<(SocketAddr, crate::net::PeerBlockReason, bool)> = self
            .peers
            .states
            .iter()
            .filter_map(|e| {
                let incoming = match e.value().get_state() {
                    PeerState::Live(l) => l.incoming,
                    PeerState::Connecting(_) => false,
                    _ => return None,
                };
                filter
                    .check(e.key().ip())
                    .map(|reason| (*e.key(), reason, incoming))
            })
            .collect();
        let mut n = 0;
        for (addr, reason, incoming) in victims {
            if self.kick_peer(addr, true) {
                debug!(?addr, ?reason, "disconnecting blocked peer");
                filter.report(addr, reason, incoming);
                n += 1;
            }
        }
        n
    }

    fn v2_ctx(&self) -> v2_hashes::V2Ctx<'_> {
        v2_hashes::V2Ctx {
            info: &self.metadata.info,
            torrent_bytes: &self.metadata.torrent_bytes,
            fetched: self.shared.v2_fetched_layers.get().map(|b| &b[..]),
            spawner: &self.shared.spawner,
            budget: &self.shared.fetch_budget,
        }
    }

    pub(crate) fn file_ops(&self) -> FileOps<'_> {
        FileOps::new(&self.metadata.info, &*self.files, &self.metadata.file_infos)
    }

    pub(crate) fn lock_read(
        &self,
        reason: &'static str,
    ) -> TimedExistence<RwLockReadGuard<'_, TorrentStateLocked>> {
        TimedExistence::new(timeit(reason, || self._locked.read()), reason)
    }
    pub(crate) fn lock_write(
        &self,
        reason: &'static str,
    ) -> TimedExistence<RwLockWriteGuard<'_, TorrentStateLocked>> {
        TimedExistence::new(timeit(reason, || self._locked.write()), reason)
    }

    fn set_peer_live(
        &self,
        handle: PeerHandle,
        h: Handshake,
        connection_kind: ConnectionKind,
        encrypted: bool,
    ) {
        let super_seeding = self.super_seeding_active();
        self.session_stats.wake_peer_tick();
        self.peers.with_peer_mut(handle, "set_peer_live", |p| {
            // Incoming peers are live already.
            p.connecting_to_live(h.peer_id, &self.peers, connection_kind);
            if let Some(live) = p.get_live_mut() {
                live.super_seeding = super_seeding;
                live.encrypted = encrypted;
            }
        });
    }

    pub fn get_uploaded_bytes(&self) -> u64 {
        self.stats.uploaded_bytes.load(Ordering::Relaxed)
    }
    pub fn get_downloaded_bytes(&self) -> u64 {
        self.stats
            .downloaded_and_checked_bytes
            .load(Ordering::Acquire)
    }

    pub fn get_approx_have_bytes(&self) -> u64 {
        self.stats.have_bytes.load(Ordering::Relaxed)
    }

    pub fn get_hns(&self) -> Option<HaveNeededSelected> {
        self.lock_read("get_hns")
            .get_chunks()
            .ok()
            .map(|c| *c.get_hns())
    }

    fn transmit_haves(&self, index: ValidPieceIndex) {
        let _ = self.have_broadcast_tx.send(index);
    }

    // Peers from the torrent's peer stream (trackers by default).
    pub(crate) fn add_peer_if_not_seen(&self, addr: SocketAddr) -> crate::Result<bool> {
        self.add_peer_with_source(addr, PeerSource::Tracker)
    }

    /// Add a peer to connect to, remembering where it came from. Returns
    /// false if the peer was already known (its sources are still updated).
    pub fn add_peer_with_source(
        &self,
        addr: SocketAddr,
        source: PeerSource,
    ) -> crate::Result<bool> {
        match self.peers.add_if_not_seen(addr, source) {
            Some(handle) => handle,
            None => return Ok(false),
        };

        self.peer_queue_tx
            .send(addr)
            .ok()
            .ok_or(Error::TorrentIsNotLive)?;
        Ok(true)
    }

    pub fn stats_snapshot(&self) -> StatsSnapshot {
        use Ordering::*;
        let downloaded_bytes = self.stats.downloaded_and_checked_bytes.load(Relaxed);
        StatsSnapshot {
            downloaded_and_checked_bytes: downloaded_bytes,
            downloaded_and_checked_pieces: self.stats.downloaded_and_checked_pieces.load(Relaxed),
            fetched_bytes: self.stats.fetched_bytes.load(Relaxed),
            uploaded_bytes: self.stats.uploaded_bytes.load(Relaxed),
            total_piece_download_ms: self.stats.total_piece_download_ms.load(Relaxed),
            peer_stats: self.peers.stats(),
        }
    }

    pub fn per_peer_stats_snapshot(&self, filter: PeerStatsFilter) -> PeerStatsSnapshot {
        PeerStatsSnapshot {
            peers: self
                .peers
                .states
                .iter()
                .filter(|e| filter.state.matches(e.value().get_state()))
                .map(|e| (e.key().to_string(), e.value().into()))
                .collect(),
        }
    }

    pub async fn wait_until_completed(&self) {
        if self.is_finished() {
            return;
        }
        self.finished_notify.notified().await;
    }

    pub fn pause(&self) -> anyhow::Result<TorrentStatePaused> {
        self.cancellation_token.cancel();

        let mut g = self.lock_write("pause");

        // It should be impossible to make a fatal error after pausing.
        g.fatal_errors_tx.take();

        let piece_tracker = g
            .pieces
            .take()
            .context("bug: pausing already paused torrent")?;
        // into_chunks() will requeue any in-flight pieces
        let chunk_tracker = piece_tracker.into_chunks();

        Ok(TorrentStatePaused {
            shared: self.shared.clone(),
            metadata: self.metadata.clone(),
            files: self.files.take()?,
            chunk_tracker,
            streams: self.streams.clone(),
        })
    }

    fn on_fatal_error(&self, e: anyhow::Error) -> anyhow::Result<()> {
        let mut g = self.lock_write("fatal_error");
        let tx = g
            .fatal_errors_tx
            .take()
            .context("fatal_errors_tx already taken")?;
        let res = anyhow::anyhow!("fatal error: {:?}", e);
        if tx.send(e).is_err() {
            warn!(id=self.shared.id, info_hash=?self.shared.info_hash, "there's nowhere to send fatal error, receiver is dead");
        }
        Err(res)
    }

    // If we have all selected pieces but not necessarily all pieces.
    pub(crate) fn is_finished(&self) -> bool {
        self.get_hns().map(|h| h.finished()).unwrap_or_default()
    }

    fn has_active_streams_unfinished_files(&self, state: &TorrentStateLocked) -> bool {
        let chunks = match state.get_chunks() {
            Ok(c) => c,
            Err(_) => return false,
        };
        self.streams
            .streamed_file_ids()
            .any(|file_id| !chunks.is_file_finished(&self.metadata.file_infos[file_id]))
    }

    // We might have the torrent "finished" i.e. no selected files. But if someone is streaming files despite
    // them being selected, we aren't fully "finished".
    fn is_finished_and_no_active_streams(&self) -> bool {
        self.is_finished()
            && !self.has_active_streams_unfinished_files(
                &self.lock_read("is_finished_and_dont_need_peers"),
            )
    }

    fn on_piece_completed(&self, id: ValidPieceIndex) -> anyhow::Result<()> {
        if let Err(e) = self.files.on_piece_completed(id) {
            debug!(?id, "file storage errored in on_piece_completed(): {e:#}");
        }
        let mut g = self.lock_write("on_piece_completed");
        let locked = &mut **g;

        self.streams
            .wake_streams_on_piece_completed(id, self.metadata.lengths());

        locked.unflushed_bitv_bytes += self.metadata.lengths().piece_length(id) as u64;
        if locked.unflushed_bitv_bytes >= FLUSH_BITV_EVERY_BYTES {
            locked.try_flush_bitv(&self.shared, true)
        }

        let chunks = locked.get_chunks()?;
        if chunks.is_finished() {
            if chunks.get_selected_pieces()[id.get_usize()] {
                locked.try_flush_bitv(&self.shared, false);
                info!(id=self.shared.id, info_hash=?self.shared.info_hash, "torrent finished downloading");
            }
            self.finished_notify.notify_waiters();

            if !self.has_active_streams_unfinished_files(locked) {
                // prevent deadlocks.
                drop(g);
                // There is not point being connected to peers that have all the torrent, when
                // we don't need anything from them, and they don't need anything from us.
                self.disconnect_all_peers_that_have_full_torrent();
            }
        }
        Ok(())
    }

    fn disconnect_all_peers_that_have_full_torrent(&self) {
        for mut pe in self.peers.states.iter_mut() {
            if let PeerState::Live(l) = pe.value().get_state()
                && l.has_full_torrent(self.lengths.total_pieces() as usize)
            {
                let prev = pe.value_mut().set_not_needed(&self.peers);
                let _ = prev
                    .take_live_no_counters()
                    .unwrap()
                    .tx
                    .send(WriterRequest::Disconnect(Ok(())));
            }
        }
    }

    pub(crate) fn reconnect_all_not_needed_peers(&self) {
        self.peers
            .states
            .iter_mut()
            .filter_map(|mut p| p.value_mut().reconnect_not_needed_peer(&self.peers))
            .map(|socket_addr| self.peer_queue_tx.send(socket_addr))
            .take_while(|r| r.is_ok())
            .last();
        // Pieces are wanted again (a stream on an unwanted file): web seeds too.
        self.new_pieces_notify.notify_waiters();
    }

    async fn task_send_pex_to_peer(
        self: Arc<Self>,
        this_peer_addr: SocketAddr,
        tx: PeerTx,
    ) -> anyhow::Result<()> {
        // As per BEP 11 we should not send more than 50 peers at once
        // (here it also applies to fist message, should be OK as we anyhow really have more)
        const MAX_SENT_PEERS: usize = 50;
        // As per BEP 11 recommended interval is min 60 seconds (shorter in
        // tests so PeX can be observed quickly).
        const PEX_MESSAGE_INTERVAL: Duration = if cfg!(test) {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(60)
        };
        const PEX_FIRST_MESSAGE_DELAY: Duration = if cfg!(test) {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(10)
        };

        let mut connected = Vec::with_capacity(MAX_SENT_PEERS);
        let mut dropped = Vec::with_capacity(MAX_SENT_PEERS);
        let mut peer_view_of_live_peers = HashSet::new();

        // Wait 10 seconds before sending the first message to assure that peer will stay with us
        tokio::time::sleep(PEX_FIRST_MESSAGE_DELAY).await;

        let mut interval = tokio::time::interval(PEX_MESSAGE_INTERVAL);

        loop {
            interval.tick().await;

            // This task should die with the cancellation token, but check defensively just in case.
            if tx.is_closed() {
                return Ok(());
            }

            // PeX switched off for the session: send nothing (the peer's view
            // is kept so that switching it back on only sends the changes).
            if !self.pex_enabled() {
                continue;
            }

            {
                let live_peers = self.peers.live_outgoing_peers.read();
                connected.clear();
                dropped.clear();

                connected.extend(
                    live_peers
                        .difference(&peer_view_of_live_peers)
                        // Synthetic I2P addresses mean nothing to other peers.
                        .filter(|a| !crate::i2p::is_i2p_addr(**a))
                        .take(MAX_SENT_PEERS)
                        .copied(),
                );
                dropped.extend(
                    peer_view_of_live_peers
                        .difference(&live_peers)
                        .take(MAX_SENT_PEERS)
                        .copied(),
                );
            }

            trace!(connected_len = connected.len(), dropped_len = dropped.len());

            let peer_ip_non_local = match this_peer_addr.ip() {
                IpAddr::V4(a) => !a.is_loopback() && !a.is_private(),
                IpAddr::V6(a) => !a.is_loopback() && !a.is_unique_local(),
            };

            let other_ip_is_local = |addr: &IpAddr| match addr {
                IpAddr::V4(a) => a.is_loopback() || a.is_private(),
                IpAddr::V6(a) => {
                    a.is_loopback() || a.is_unicast_link_local() || a.is_unique_local()
                }
            };

            let filter = |addr: &SocketAddr| !(peer_ip_non_local && other_ip_is_local(&addr.ip()));

            // BEP 11 - Dont send closed if they are now in live
            // it's assured by mutual exclusion of two  above sets  if in sent_peers_live, it cannot be in addrs_live_to_sent,
            // and addrs_closed_to_sent are only filtered addresses from sent_peers_live

            if !connected.is_empty() || !dropped.is_empty() {
                let pex_msg = extended::ut_pex::UtPex::from_addrs(
                    connected.iter().copied(),
                    dropped.iter().copied(),
                );
                if tx.send(WriterRequest::UtPex(pex_msg)).is_err() {
                    return Ok(()); // Peer disconnected
                }

                for addr in &dropped {
                    peer_view_of_live_peers.remove(addr);
                }
                peer_view_of_live_peers.extend(connected.iter().filter(|a| filter(a)).copied());
            }
        }
    }
}

const DEFAULT_PEER_REQUEST_WINDOW: usize = 128;

// While a download limit applies, a peer's outstanding requests cover this
// much time at its current rate (libtorrent's `request_queue_time`), but never
// fewer than MIN_LIMITED_REQUEST_WINDOW blocks.
const LIMITED_REQUEST_QUEUE_SECS: u64 = 3;
const MIN_LIMITED_REQUEST_WINDOW: usize = 2;

/// The request window of a peer downloading at `down_bps` under a download
/// limit, capped at the unlimited `max_window`.
fn limited_request_window(down_bps: u64, max_window: usize) -> usize {
    let blocks = (down_bps * LIMITED_REQUEST_QUEUE_SECS).div_ceil(u64::from(CHUNK_SIZE));
    usize::try_from(blocks)
        .unwrap_or(usize::MAX)
        .clamp(MIN_LIMITED_REQUEST_WINDOW.min(max_window), max_window)
}

struct PeerFlowControl {
    i_am_choked: bool,
    request_window: usize,
}

impl PeerFlowControl {
    fn new(max_request_window: usize) -> Self {
        Self {
            i_am_choked: true,
            request_window: max_request_window,
        }
    }
}

// All peer state that would never be used by other actors should pe put here.
// This state tracks a live peer.
struct PeerHandler {
    state: Arc<TorrentStateLive>,
    counters: Arc<AtomicPeerCounters>,
    // Semantically, we don't need a lock here, as this is only requested from
    // one future (requester + manage_peer).
    //
    // However as PeerConnectionHandler takes &self everywhere, we need shared mutability.
    // RefCell would do, but tokio is unhappy when we use it.
    flow_control: Mutex<PeerFlowControl>,

    // This is used to unpause chunk requester once the bitfield
    // is received.
    on_bitfield_notify: Notify,

    addr: SocketAddr,
    incoming: bool,
    // The peer is on the local network (for the rate limit exemption).
    is_lan: bool,
    // The connection uses uTP (known once connected).
    is_utp: AtomicBool,
    is_ipv6: bool,
    tx: PeerTx,

    first_message_received: AtomicBool,

    // PEX abuse limits (one sender task, one accepted message per interval).
    pex: PexGuard,

    // Chunk requests from the peer not sent yet (capped, cancellable).
    uploads: Arc<Mutex<UploadQueue>>,
    // ut_metadata data messages queued for the writer (capped).
    queued_metadata_pieces: AtomicU32,
    // BEP 52 hashes / hash reject replies queued for the writer (capped).
    queued_hash_replies: AtomicU32,

    cancel_token: CancellationToken,

    client_name_and_version: String,

    /// The connection uses a hybrid torrent's truncated v2 hash: made with
    /// it, or a v1 connection upgraded to v2 (libtorrent's `protocol_v2`
    /// peers: they answer hash requests).
    v2_hash: AtomicBool,
}

#[derive(Default)]
struct PexGuard {
    task_spawned: AtomicBool,
    last_message: Mutex<Option<Instant>>,
}

/// BEP 11: at most 50 added peers per message, at most one message a minute
/// (a little slack for timer jitter).
const PEX_MAX_ADDED: usize = 50;
const PEX_MIN_INTERVAL: Duration = Duration::from_secs(45);

/// Addresses from a peer's PEX message worth connecting to. Peers are
/// untrusted: they must not make us connect to special addresses
/// (unspecified, multicast, broadcast, port 0) or, unless they are on that
/// network themselves, to our loopback / LAN / link-local addresses.
fn pex_candidates(added: impl Iterator<Item = SocketAddr>, from: SocketAddr) -> Vec<SocketAddr> {
    fn class(ip: IpAddr) -> u8 {
        match ip.to_canonical() {
            IpAddr::V4(v4) if v4.is_loopback() => 2,
            IpAddr::V4(v4) if v4.is_private() || v4.is_link_local() => 1,
            IpAddr::V6(v6) if v6.is_loopback() => 2,
            IpAddr::V6(v6) if v6.is_unique_local() || v6.is_unicast_link_local() => 1,
            _ => 0,
        }
    }
    let from_class = class(from.ip());
    added
        .filter(|a| {
            let ip = a.ip().to_canonical();
            let special = match ip {
                IpAddr::V4(v4) => v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast(),
                IpAddr::V6(v6) => v6.is_unspecified() || v6.is_multicast(),
            };
            // No I2P PeX: synthetic I2P addresses are never accepted.
            a.port() != 0 && !special && class(ip) <= from_class && !crate::i2p::is_synthetic_addr(*a)
        })
        .take(PEX_MAX_ADDED)
        .collect()
}

#[cfg(test)]
mod pex_tests {
    use super::*;

    fn addrs(list: &[&str]) -> Vec<SocketAddr> {
        list.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn pex_input_is_filtered_and_capped() {
        let from: SocketAddr = "8.8.8.8:6881".parse().unwrap();
        let got = pex_candidates(
            addrs(&[
                "1.2.3.4:1",
                "127.0.0.1:80",
                "0.0.0.0:1",
                "224.0.0.1:1",
                "255.255.255.255:1",
                "192.168.1.1:80",
                "169.254.169.254:80",
                "[::1]:80",
                "[ff02::1]:1",
                "5.6.7.8:0",
                "[2001:db8::1]:2",
            ])
            .into_iter(),
            from,
        );
        assert_eq!(got, addrs(&["1.2.3.4:1", "[2001:db8::1]:2"]));
        // A LAN peer may tell us about other LAN peers.
        let lan: SocketAddr = "192.168.1.5:6881".parse().unwrap();
        assert_eq!(pex_candidates(addrs(&["192.168.1.6:1", "127.0.0.1:1"]).into_iter(), lan), addrs(&["192.168.1.6:1"]));
        let many = (0..200u16).map(|p| SocketAddr::from(([1, 2, 3, 4], p + 1)));
        assert_eq!(pex_candidates(many, from).len(), PEX_MAX_ADDED);
        // A clearnet peer can't make us dial an I2P destination of its choice.
        assert!(pex_candidates(addrs(&["[100::1220:0:0:0]:1", "[100::1220:0:0:5]:6881"]).into_iter(), from).is_empty());
    }
}

impl PeerConnectionHandler for &'_ PeerHandler {
    fn on_connected(&self, connection_time: Duration) {
        self.counters
            .outgoing_connections
            .fetch_add(1, Ordering::Relaxed);
        #[allow(clippy::cast_possible_truncation)]
        self.counters
            .total_time_connecting_ms
            .fetch_add(connection_time.as_millis() as u64, Ordering::Relaxed);
    }

    async fn on_received_message(&self, message: Message<'_>) -> anyhow::Result<()> {
        // The first message must be "bitfield", but if it's not sent,
        // assume the bitfield is all zeroes and was sent.
        if !matches!(&message, Message::Bitfield(..))
            && !self.first_message_received.swap(true, Ordering::Relaxed)
        {
            self.on_bitfield_notify.notify_waiters();
            // The peer has nothing: reveal a piece if super seeding.
            self.state.superseed_check(self.addr);
        }

        // A hybrid torrent without piece layers (added by a magnet link)
        // fetches them from a v2 peer, to answer hash requests itself; a
        // pure v2 torrent the layers its .torrent lacked, to verify (and
        // download) those files.
        if (self.v2_hash.load(Ordering::Relaxed) || self.state.metadata.info.is_pure_v2())
            && !matches!(message, Message::Piece(_) | Message::Request(_))
            && self.state.v2_hashes.fetch_wanted()
        {
            for r in self.state.v2_hashes.maybe_start_fetch(
                &self.state.v2_ctx(),
                self.addr,
                &|s| self.state.v2_file_wanted(s),
            ) {
                self.tx
                    .send(WriterRequest::Message(Message::HashRequest(r)))?;
            }
        }

        match message {
            Message::Request(request) => {
                self.on_download_request(request)
                    .context("on_download_request")?;
            }
            Message::Bitfield(b) => {
                self.first_message_received.store(true, Ordering::Relaxed);
                self.on_bitfield(b.clone_to_owned(None))
                    .context("on_bitfield")?
            }
            Message::Choke => self.on_i_am_choked(),
            Message::Unchoke => self.on_i_am_unchoked(),
            Message::Interested => self.on_peer_interested(),
            Message::Piece(piece) => {
                // Holding the reader here stops reading the socket: the peer's
                // sends back up instead of arriving faster than the limit.
                self.throttle_download(piece.len()).await?;
                self.on_received_piece(piece)
                    .await
                    .context("on_received_piece")?
            }
            Message::KeepAlive => {
                trace!("keepalive received");
            }
            Message::Have(h) => self.on_have(h),
            Message::NotInterested => self.on_peer_not_interested(),
            Message::Cancel(request) => self.on_cancel_request(request),
            Message::Extended(ExtendedMessage::UtMetadata(UtMetadata::Request(
                metadata_piece_id,
            ))) => {
                if self.state.metadata.info.info().private {
                    warn!(
                        id = self.state.shared.id,
                        info_hash = ?self.state.shared.info_hash,
                        "received noncompliant ut_metadata message from {}, ignoring",
                        self.addr
                    );
                } else {
                    self.send_metadata_piece(metadata_piece_id)
                        .with_context(|| {
                            format!("error sending metadata piece {metadata_piece_id}")
                        })?;
                }
            }
            Message::Extended(ExtendedMessage::UtPex(pex)) => {
                if !self.state.metadata.info.info().private && !self.state.pex_enabled() {
                    trace!("PeX is disabled, ignoring PeX message");
                } else if self.state.metadata.info.info().private {
                    warn!(
                        id = self.state.shared.id,
                        info_hash = ?self.state.shared.info_hash,
                        "received noncompliant PEX message from {}, ignoring",
                        self.addr
                    );
                } else {
                    self.on_pex_message(pex);
                }
            }
            Message::HashRequest(req) => self.on_hash_request(req)?,
            Message::Hashes(req, data) => self.on_hashes_reply(&req, Some(data.as_ref()))?,
            Message::HashReject(req) => self.on_hashes_reply(&req, None)?,
            message => {
                warn!(
                    id = self.state.shared.id,
                    info_hash = ?self.state.shared.info_hash,
                    "received unsupported message {:?}, ignoring", message
                );
            }
        };
        Ok(())
    }

    fn serialize_bitfield_message_to_buf(&self, buf: &mut [u8]) -> anyhow::Result<usize> {
        let g = self.state.lock_read("serialize_bitfield_message_to_buf");
        let msg = Message::Bitfield(ByteBuf(g.get_chunks()?.get_have_pieces().as_bytes()));
        let len = msg.serialize(buf, &Default::default)?;
        trace!("sending: {:?}, length={}", &msg, len);
        Ok(len)
    }

    fn on_handshake(
        &self,
        handshake: Handshake,
        ckind: ConnectionKind,
        encrypted: bool,
    ) -> anyhow::Result<()> {
        self.is_utp
            .store(matches!(ckind, ConnectionKind::Utp), Ordering::Relaxed);
        if self.state.shared.info_hash_alt == Some(handshake.info_hash) {
            // Our v1 connection was upgraded to v2 (or made with the v2 hash).
            self.v2_hash.store(true, Ordering::Relaxed);
        }
        self.state
            .set_peer_live(self.addr, handshake, ckind, encrypted);
        Ok(())
    }

    fn on_protocol_bytes_received(&self, bytes: u64) {
        let c = &self.state.session_stats.counters;
        c.protocol_received_bytes
            .fetch_add(bytes, Ordering::Relaxed);
        c.ip_overhead_received_bytes.fetch_add(
            estimate_ip_overhead(bytes, self.is_ipv6, self.is_utp.load(Ordering::Relaxed)),
            Ordering::Relaxed,
        );
    }

    fn on_protocol_bytes_sent(&self, bytes: u64) {
        let c = &self.state.session_stats.counters;
        c.protocol_sent_bytes.fetch_add(bytes, Ordering::Relaxed);
        c.ip_overhead_sent_bytes.fetch_add(
            estimate_ip_overhead(bytes, self.is_ipv6, self.is_utp.load(Ordering::Relaxed)),
            Ordering::Relaxed,
        );
    }

    fn on_uploaded_bytes(&self, bytes: u32) {
        self.counters
            .uploaded_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
        self.state
            .stats
            .uploaded_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
        self.state
            .session_stats
            .counters
            .uploaded_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    fn read_chunk(&self, chunk: &ChunkInfo, buf: &mut [u8]) -> anyhow::Result<()> {
        self.state.file_ops().read_chunk(self.addr, chunk, buf)
    }

    async fn throttle_upload(&self, chunk: &ChunkInfo) {
        // Don't spend tokens on a request that won't be sent.
        if !self.upload_wanted(chunk) {
            return;
        }
        let Some(len) = NonZeroU32::new(chunk.size) else {
            return;
        };
        let _ = self
            .state
            .shared
            .runtime
            .ratelimits
            .prepare_for_upload(len)
            .await;
        let Some(session) = self.state.shared.session.upgrade() else {
            return;
        };
        if self.session_limits_apply() {
            let _ = session.ratelimits.prepare_for_upload(len).await;
        }
        if let Some(utp) = self.mixed_class() {
            let _ = session
                .peer_policy
                .mixed
                .limits(utp)
                .prepare_for_upload(len)
                .await;
        }
    }

    fn take_upload_request(&self, chunk: &ChunkInfo) -> bool {
        let queued = self.uploads.lock().take(chunk.absolute_index);
        // The peer may have been choked while the request waited.
        queued && self.not_choking()
    }

    fn on_metadata_piece_dequeued(&self) {
        let _ = self.queued_metadata_pieces.try_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |n| n.checked_sub(1),
        );
    }

    fn on_hash_reply_dequeued(&self) {
        let _ = self.queued_hash_replies.try_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |n| n.checked_sub(1),
        );
    }

    fn on_extended_handshake(&self, hs: &ExtendedHandshake<ByteBuf>) -> anyhow::Result<()> {
        if let Some(client_name) = hs.v.as_ref().and_then(format_peer_client_name) {
            self.state
                .peers
                .with_live_mut(self.addr, "update peer client name", |live| {
                    live.client_name = Some(client_name);
                });
        }

        if let Some(reqq) = hs.reqq.and_then(|reqq| usize::try_from(reqq).ok())
            && reqq > 0
        {
            let request_window = reqq.min(self.state.max_request_window());
            let mut flow = self.lock_flow_control("update request window");
            if flow.request_window != request_window {
                debug!(
                    reqq,
                    request_window, "updated peer request window from extended handshake"
                );
                flow.request_window = request_window;
                self.notify_request_slots_changed();
            }
        }

        // A peer may repeat its extended handshake: spawn the PEX sender
        // only once per connection.
        if !self.state.metadata.info.info().private
            && hs.m.ut_pex.is_some()
            && !self.pex.task_spawned.swap(true, Ordering::Relaxed)
        {
            spawn_with_cancel(
                debug_span!(
                    parent: self.state.shared.span.clone(),
                    "sending_pex_to_peer",
                    peer = ?self.addr,
                ),
                format!(
                    "[{}][addr={}]sending_pex_to_peer",
                    self.state.shared.id, self.addr
                ),
                self.cancel_token.clone(),
                self.state
                    .clone()
                    .task_send_pex_to_peer(self.addr, self.tx.clone()),
            );
        }
        // Lets update outgoing Socket address for incoming connection
        if self.incoming
            && let Some(port) = hs.port()
        {
            // Only the listen port is taken from the peer: the address is the
            // one it connected from, so a peer cannot make us connect to an
            // arbitrary third-party IP later.
            let outgoing_addr = SocketAddr::new(self.addr.ip(), port);
            self.state
                .peers
                .with_peer_mut(self.addr, "update outgoing addr", |peer| {
                    peer.outgoing_address = Some(outgoing_addr)
                });
        }

        // The address the peer sees us at (BEP 10 `yourip`) is a vote for
        // our external address.
        if let Some(yourip) = hs.yourip
            && !crate::i2p::is_i2p_addr(self.addr)
            && let Some(session) = self.state.shared.session.upgrade()
        {
            session
                .external_ip
                .cast_vote(yourip.0, IpVoteSource::Peer(self.addr.ip()));
        }

        Ok(())
    }

    /// Pure v2, and hybrid torrents on every connection: on one made with
    /// the v1 hash the bit offers the upgrade to v2 (libtorrent does the
    /// same; a peer that knows the torrent as hybrid answers with the v2
    /// hash).
    fn handshake_v2(&self) -> bool {
        self.state.metadata.info.is_pure_v2() || self.state.shared.info_hash_alt.is_some()
    }

    fn accepts_other_info_hash(&self, info_hash: Id20) -> bool {
        // Our v1 connection upgraded (outgoing: the peer answered with the
        // v2 hash), or (incoming) the peer's v1 hash we upgraded.
        self.state.shared.info_hash_alt.is_some()
            && (Some(info_hash) == self.state.shared.info_hash_alt
                || info_hash == self.state.shared.info_hash)
    }

    fn should_send_bitfield(&self) -> bool {
        if self.state.torrent().options.disable_upload() {
            return false;
        }

        // Super seeding: reveal pieces one by one instead.
        if self
            .state
            .peers
            .with_live(self.addr, |l| l.super_seeding)
            .unwrap_or(false)
        {
            self.state.superseed_check(self.addr);
            return false;
        }

        self.state.get_approx_have_bytes() > 0
    }

    fn should_transmit_have(&self, _id: ValidPieceIndex) -> bool {
        if self.state.shared.options.disable_upload() {
            return false;
        }
        // Skip "have" only for peers with the whole torrent. A peer that
        // merely has this piece still needs it: a super-seeding seed (BEP 16)
        // reveals its pieces one by one and waits for our "have" to reveal
        // the next one.
        let seed = self
            .state
            .peers
            .with_live(self.addr, |l| l.seeder)
            .unwrap_or(true);
        !seed
    }

    fn update_my_extended_handshake(
        &self,
        handshake: &mut ExtendedHandshake<ByteBuf>,
    ) -> anyhow::Result<()> {
        let info_bytes = &self.state.metadata.info_bytes;
        if !info_bytes.is_empty()
            && let Ok(len) = info_bytes.len().try_into()
        {
            handshake.metadata_size = Some(len);
        }
        handshake.reqq = Some(MAX_UPLOAD_REQUESTS);
        // No PeX for private torrents (BEP 27) or with PeX off for the
        // torrent: don't offer it (session-wide PeX is handled by the
        // connection).
        if !self.state.pex_enabled() {
            handshake.m.ut_pex = None;
        }

        Ok(())
    }

    fn client_name_and_version(&self) -> &str {
        &self.client_name_and_version
    }
}

impl PeerHandler {
    fn on_peer_died(self, error: Option<crate::Error>) -> crate::Result<()> {
        self.state.v2_hashes.on_peer_gone(self.addr);
        let peers = &self.state.peers;
        let handle = self.addr;
        let mut pe = match peers.states.get_mut(&handle) {
            Some(peer) => TimedExistence::new(peer, "on_peer_died"),
            None => {
                warn!(
                    id = self.state.shared.id,
                    info_hash = ?self.state.shared.info_hash,
                    addr=?handle,
                    "bug: peer not found in table. Forgetting it forever"
                );
                return Ok(());
            }
        };
        let prev = pe.value_mut().take_state(peers);

        match prev {
            PeerState::Connecting(_) => {}
            PeerState::Live(live) => {
                let mut g = self.state.lock_write("mark_chunk_requests_canceled");

                // Release all pieces owned by this peer (fixes the bug where pieces
                // could be in both queue_pieces AND inflight_pieces after peer death)
                let released = g.get_pieces_mut()?.release_pieces_owned_by(self.addr);
                if released > 0 {
                    trace!(
                        "peer dead, released {} in-flight pieces back to queue",
                        released
                    );
                }

                // Also handle any active chunk-level inflight requests.
                let mut had_inflight = false;
                for req in live.inflight_requests() {
                    had_inflight = true;
                    trace!(
                        "peer dead, marking chunk request cancelled, index={}, chunk={}",
                        req.piece_index.get(),
                        req.chunk_index
                    );
                }

                if released > 0 || had_inflight {
                    self.state.new_pieces_notify.notify_waiters();
                }
            }
            PeerState::NotNeeded => {
                // Restore it as std::mem::take() replaced it above.
                pe.value_mut().set_state(PeerState::NotNeeded, peers);
                return Ok(());
            }
            s @ PeerState::Queued | s @ PeerState::Dead => {
                warn!(
                    id = self.state.shared.id,
                    info_hash = ?self.state.shared.info_hash,
                    addr = ?handle,
                    "bug: peer was in a wrong state {s:?}, ignoring it forever"
                );
                // Prevent deadlocks.
                drop(pe);
                self.state.peers.drop_peer(handle);
                return Ok(());
            }
        };

        if pe.value().disconnect_requested {
            trace!("peer was disconnected on request, not re-queueing");
            pe.value_mut().disconnect_requested = false;
            pe.value_mut().set_state(PeerState::NotNeeded, peers);
            return Ok(());
        }

        let _error = match error {
            Some(e) => e,
            None => {
                trace!("peer died without errors, not re-queueing");
                pe.value_mut().set_state(PeerState::NotNeeded, peers);
                return Ok(());
            }
        };

        self.counters.errors.fetch_add(1, Ordering::Relaxed);

        if self.state.is_finished_and_no_active_streams() {
            debug!("torrent finished, not re-queueing");
            pe.value_mut().set_state(PeerState::NotNeeded, peers);
            return Ok(());
        }

        pe.value_mut().set_state(PeerState::Dead, peers);

        if self.incoming {
            // do not retry incoming peers
            debug!(
                peer = handle.to_string(),
                "incoming peer died, not re-queueing"
            );
            return Ok(());
        }

        let backoff = pe.value_mut().stats.backoff.next();

        // Prevent deadlocks.
        drop(pe);

        if let Some(dur) = backoff {
            if cfg!(feature = "_disable_reconnect_test") {
                return Ok(());
            }
            self.state.clone().spawn(
                debug_span!(
                    parent: self.state.shared.span.clone(),
                    "wait_for_peer",
                    peer = ?handle,
                    duration = format!("{dur:?}")
                ),
                format!("[{}][addr={}]wait_for_peer", self.state.shared.id, handle),
                async move {
                    trace!("waiting to reconnect again");
                    tokio::time::sleep(dur).await;
                    trace!("finished waiting");
                    let should_requeue = self
                        .state
                        .peers
                        .with_peer_mut(handle, "dead_to_queued", |peer| {
                            match peer.get_state() {
                                PeerState::Dead => {
                                    peer.set_state(PeerState::Queued, &self.state.peers);
                                    true
                                }
                                // Peer reconnected (e.g. via incoming connection) while we were
                                // waiting. No need to queue - it's already connected or queued.
                                PeerState::Live(_)
                                | PeerState::Connecting(_)
                                | PeerState::Queued => {
                                    trace!(
                                        state = peer.get_state().name(),
                                        "peer is no longer dead, skipping requeue"
                                    );
                                    false
                                }
                                // Don't need this peer anymore.
                                PeerState::NotNeeded => false,
                            }
                        })
                        .unwrap_or(false);
                    if should_requeue {
                        self.state
                            .peer_queue_tx
                            .send(handle)
                            .ok()
                            .ok_or(Error::TorrentIsNotLive)?;
                    }
                    Ok::<_, Error>(())
                },
            );
        } else {
            debug!("dropping peer, backoff exhausted");
            self.state.peers.drop_peer(handle);
        };
        Ok(())
    }

    /// Acquire a piece for this peer: try steal (10x) → reserve → steal (3x).
    ///
    /// Returns the piece index to download, or None if no pieces are available.
    fn acquire_next_piece(&self) -> crate::Result<Option<ValidPieceIndex>> {
        if self.is_choked() {
            debug!("we are choked, can't acquire piece");
            return Ok(None);
        }

        // Steal info to process after releasing the peer lock
        let mut steal_info: Option<(SocketAddr, ValidPieceIndex)> = None;

        let result = self
            .state
            .peers
            .with_live_mut(self.addr, "acquire_next_piece", |live| {
                let mut g = self.state.lock_write("acquire_next_piece");

                let bf = &live.bitfield;
                // Extract references to disjoint fields
                let TorrentStateLocked {
                    pieces,
                    piece_priorities,
                    v2_blocked,
                    ..
                } = &mut **g;
                let pieces = pieces.as_mut().ok_or(Error::ChunkTrackerEmpty)?;
                let result = pieces.acquire_piece(AcquireRequest {
                    peer: self.addr,
                    peer_avg_time: self.counters.average_piece_download_time(),
                    priority_pieces: self.state.streams.iter_next_pieces(&self.state.lengths),
                    piece_priorities,
                    availability: |p| self.state.peers.availability.get(p),
                    sequential: self.state.shared.runtime.sequential(),
                    seed: self.state.picker_seed,
                    // Pieces that can't be verified yet are never requested.
                    peer_has_piece: |p| {
                        bf.get(p.get() as usize).map(|v| *v) == Some(true)
                            && !is_blocked(v2_blocked.as_deref(), p)
                    },
                    can_steal: |p| {
                        self.state
                            .piece_lock(p.get_usize())
                            .try_write()
                            .is_some()
                    },
                });

                match result {
                    AcquireResult::Reserved(piece) => {
                        trace!("reserved piece {}", piece);
                        Ok(Some(piece))
                    }
                    AcquireResult::Stolen { piece, from_peer } => {
                        debug!("stole piece {} from {}", piece, from_peer);
                        // Store steal info to process after releasing peer lock to avoid deadlock
                        steal_info = Some((from_peer, piece));
                        Ok(Some(piece))
                    }
                    AcquireResult::NoneAvailable => Ok(None),
                }
            })
            .transpose()
            .map(|r| r.flatten());

        // Process steal notification outside the peer lock to avoid deadlock
        if let Some((from_peer, piece)) = steal_info {
            self.state.peers.on_steal(from_peer, self.addr, piece);
        }

        result
    }

    fn on_download_request(&self, request: Request) -> anyhow::Result<()> {
        if self.state.torrent().options.disable_upload() {
            anyhow::bail!("upload disabled, but peer requested a piece")
        }

        let piece_index = match self.state.lengths.validate_piece_index(request.index) {
            Some(p) => p,
            None => {
                anyhow::bail!(
                    "received {:?}, but it is not a valid chunk request (piece index is invalid). Ignoring.",
                    request
                );
            }
        };

        let chunk_info = match self.requested_chunk(piece_index, request.begin, request.length) {
            Some(d) => d,
            None => {
                anyhow::bail!(
                    "received {:?}, but it is not a valid chunk request (chunk data is invalid). Ignoring.",
                    request
                );
            }
        };

        // Requests from choked peers (e.g. sent before our choke reached them)
        // are dropped, as are requests for pieces not revealed while super seeding.
        let allowed = self
            .state
            .peers
            .with_live(self.addr, |l| {
                !l.am_choking && superseed_allows(l, piece_index)
            })
            .unwrap_or(false);
        if !allowed {
            trace!(?request, "ignoring request from a choked peer");
            return Ok(());
        }

        match self.uploads.lock().push(chunk_info.absolute_index) {
            PushResult::Queued => {}
            PushResult::Duplicate => return Ok(()),
            PushResult::Dropped => {
                trace!(?request, "dropping request: too many outstanding requests");
                return Ok(());
            }
            PushResult::Abusive => {
                bail!("peer keeps sending more than {MAX_UPLOAD_REQUESTS} outstanding requests")
            }
        }

        if !self
            .state
            .lock_read("is_chunk_ready_to_upload")
            .get_chunks()?
            .is_chunk_ready_to_upload(&chunk_info)
        {
            anyhow::bail!(
                "got request for a chunk that is not ready to upload. chunk {:?}",
                chunk_info
            );
        }

        // The writer waits for the upload limits right before sending.
        self.tx
            .send(WriterRequest::ReadChunkRequest(chunk_info))
            .ok()
            .ok_or(Error::PeerTaskDead)?;
        Ok(())
    }

    fn on_cancel_request(&self, request: Request) {
        let Some(piece_index) = self.state.lengths.validate_piece_index(request.index) else {
            return;
        };
        if let Some(chunk) = self.requested_chunk(piece_index, request.begin, request.length) {
            self.uploads.lock().remove(chunk.absolute_index);
        }
    }

    /// The chunk a peer's request (or cancel) is for. In a pure v2 torrent
    /// libtorrent asks for a block cut at the end of a file's data, without
    /// the padding that follows (`piece_size2`): such a shorter request for
    /// the last block of a piece's data is accepted and served as asked.
    fn requested_chunk(
        &self,
        piece_index: ValidPieceIndex,
        begin: u32,
        length: u32,
    ) -> Option<ChunkInfo> {
        let lengths = &self.state.lengths;
        if let Some(c) = lengths.chunk_info_from_received_data(piece_index, begin, length) {
            return Some(c);
        }
        let data_len = self
            .state
            .metadata
            .info
            .v2_layout()?
            .piece_data_len(piece_index.get())?;
        let full = lengths.chunk_size(piece_index, begin / CHUNK_SIZE)?;
        let chunk = lengths.chunk_info_from_received_data(piece_index, begin, full)?;
        (length > 0 && length < full && u64::from(begin) + u64::from(length) == data_len)
            .then_some(ChunkInfo {
                size: length,
                ..chunk
            })
    }

    // Whether the session rate limits apply to this peer (uTP and LAN peers
    // can be exempted).
    fn session_limits_apply(&self) -> bool {
        match self.state.shared.session.upgrade() {
            Some(s) => s
                .peer_policy
                .session_limits_apply(self.is_utp.load(Ordering::Relaxed), self.is_lan),
            None => true,
        }
    }

    // `Some(is_utp)` when the mixed mode throttles apply to this peer.
    fn mixed_class(&self) -> Option<bool> {
        (!self.is_lan).then(|| self.is_utp.load(Ordering::Relaxed))
    }

    // Wait for the download limits that apply to this peer (torrent, session,
    // mixed mode class) to allow `len` bytes of piece data.
    async fn throttle_download(&self, len: usize) -> crate::Result<()> {
        let Some(len) = u32::try_from(len).ok().and_then(NonZeroU32::new) else {
            return Ok(());
        };
        self.state
            .shared
            .runtime
            .ratelimits
            .prepare_for_download(len)
            .await?;
        let Some(session) = self.state.shared.session.upgrade() else {
            return Ok(());
        };
        if self.session_limits_apply() {
            session.ratelimits.prepare_for_download(len).await?;
        }
        if let Some(utp) = self.mixed_class() {
            session
                .peer_policy
                .mixed
                .limits(utp)
                .prepare_for_download(len)
                .await?;
        }
        Ok(())
    }

    fn not_choking(&self) -> bool {
        self.state
            .peers
            .with_live(self.addr, |l| !l.am_choking)
            .unwrap_or(false)
    }

    // The upload request is still queued and the peer is not choked.
    fn upload_wanted(&self, chunk: &ChunkInfo) -> bool {
        self.uploads.lock().contains(chunk.absolute_index) && self.not_choking()
    }

    // Whether any download limit applies to this peer.
    fn download_limited(&self) -> bool {
        if self
            .state
            .shared
            .runtime
            .ratelimits
            .get_download_bps()
            .is_some()
        {
            return true;
        }
        let Some(session) = self.state.shared.session.upgrade() else {
            return false;
        };
        (self.session_limits_apply() && session.ratelimits.get_download_bps().is_some())
            || self.mixed_class().is_some_and(|utp| {
                session
                    .peer_policy
                    .mixed
                    .limits(utp)
                    .get_download_bps()
                    .is_some()
            })
    }

    fn on_have(&self, have: u32) {
        let total_pieces = self.state.lengths.total_pieces() as usize;
        self.state
            .peers
            .with_live_mut(self.addr, "on_have", |live| {
                // The bitfield is allocated on the first "have": some clients start empty so
                // they never send bitfields.
                if !self
                    .state
                    .peers
                    .mark_have(live, have as usize, total_pieces)
                {
                    warn!(
                        id = self.state.shared.id,
                        info_hash = ?self.state.shared.info_hash,
                        addr = ?self.addr,
                        "received have {} out of range",
                        have
                    );
                    return;
                }
                trace!("updated bitfield with have={}", have);
                if live.seeder {
                    debug!("peer has full torrent");
                }
            });
        if let Some(piece) = self.state.lengths.validate_piece_index(have) {
            self.state.superseed_on_have(self.addr, piece);
            // A requester waiting for pieces may be able to get this one now.
            let needed = self
                .state
                .lock_read("on_have_needed")
                .get_chunks()
                .is_ok_and(|c| c.queue_pieces().get(piece.get_usize()).is_some_and(|b| *b));
            if needed {
                self.state.new_pieces_notify.notify_waiters();
            }
        }
        self.on_bitfield_notify.notify_waiters();
    }

    fn on_bitfield(&self, bitfield: ByteBufOwned) -> anyhow::Result<()> {
        if bitfield.as_ref().len() != self.state.lengths.piece_bitfield_bytes() {
            anyhow::bail!(
                "dropping peer as its bitfield has unexpected size. Got {}, expected {}",
                bitfield.as_ref().len(),
                self.state.lengths.piece_bitfield_bytes(),
            );
        }
        let bf = BF::from_boxed_slice(bitfield.0.to_vec().into_boxed_slice());
        if let Some(true) = bf
            .get(..self.state.lengths.total_pieces() as usize)
            .map(|s| s.all())
        {
            debug!("peer has full torrent");
        }
        self.state
            .peers
            .update_bitfield(self.addr, bf, self.state.lengths.total_pieces() as usize);
        self.state.superseed_check(self.addr);
        self.on_bitfield_notify.notify_waiters();
        Ok(())
    }

    async fn wait_for_any_notify(&self, notify: &Notify, check: impl Fn() -> bool) {
        loop {
            // To remove possibility of races, we first grab a token, then check
            // if we need it, and only if so, await.
            let notified = notify.notified();
            if check() {
                return;
            }
            notified.await;
        }
    }

    async fn wait_for_bitfield(&self) {
        self.wait_for_any_notify(&self.on_bitfield_notify, || {
            self.state
                .peers
                .with_live(self.addr, |live| !live.bitfield.is_empty())
                .unwrap_or_default()
        })
        .await;
    }

    async fn wait_for_request_slot(&self) {
        loop {
            let Some(notify) = self.request_slots_changed() else {
                return;
            };
            let notified = notify.notified();
            if self.can_send_request() {
                return;
            }
            notified.await;
        }
    }

    // The job of this is to request chunks and also to keep peer alive.
    // The moment this ends, the peer is disconnected.
    async fn task_peer_chunk_requester(&self) -> crate::Result<()> {
        let handle = self.addr;
        self.wait_for_bitfield().await;

        let mut update_interest = {
            let mut current = false;
            move |h: &PeerHandler, new_value: bool| -> crate::Result<()> {
                if new_value != current {
                    h.tx.send(if new_value {
                        WriterRequest::Message(Message::Interested)
                    } else {
                        WriterRequest::Message(Message::NotInterested)
                    })
                    .ok()
                    .ok_or(Error::PeerTaskDead)?;
                    current = new_value;
                    h.state
                        .peers
                        .with_live_mut(h.addr, "am_interested", |l| l.am_interested = new_value);
                }
                Ok(())
            }
        };

        loop {
            // Woken by this peer's interest / have / bitfield messages while idle.
            let peer_changed = self.on_bitfield_notify.notified();
            tokio::pin!(peer_changed);
            peer_changed.as_mut().enable();
            // If we have full torrent, we don't need to request more pieces.
            // However we might still need to seed them to the peer.
            if self.state.is_finished_and_no_active_streams() {
                update_interest(self, false)?;
                if self
                    .state
                    .peers
                    .is_peer_not_interested_and_has_full_torrent(
                        self.addr,
                        self.state.lengths.total_pieces() as usize,
                    )
                {
                    debug!("nothing left to do, neither of us is interested, disconnecting peer");
                    self.tx
                        .send(WriterRequest::Disconnect(Ok(())))
                        .ok()
                        .ok_or(Error::PeerTaskDead)?;
                    // wait until the receiver gets the message so that it doesn't finish with an error.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    return Ok(());
                } else {
                    // Wait for a change in this peer's interest or pieces, or in our own
                    // pieces/priorities/streams; the timeout is only a safety net for
                    // changes that do not notify (a 5 s poll per peer was a wakeup storm
                    // with thousands of seeding connections).
                    let state_changed = self.state.new_pieces_notify.notified();
                    let _ = tokio::time::timeout(Duration::from_secs(30), async {
                        tokio::select! {
                            _ = &mut peer_changed => {}
                            _ = state_changed => {}
                        }
                    })
                    .await;
                    continue;
                }
            }

            update_interest(self, true)?;
            aframe!(self.wait_for_request_slot()).await;

            // Acquire a piece using the strategy: try steal (10x) → reserve → steal (3x).
            let new_piece_notify = self.state.new_pieces_notify.notified();
            let next = match self.acquire_next_piece()? {
                Some(next) => next,
                None => {
                    debug!("no pieces to request");
                    match aframe!(tokio::time::timeout(
                        // Half of default rw timeout not to race with it.
                        Duration::from_secs(5),
                        new_piece_notify
                    ))
                    .await
                    {
                        Ok(()) => debug!("woken up, new pieces might be available"),
                        Err(_) => debug!("woken up by sleep timer"),
                    }
                    continue;
                }
            };

            for chunk in self.state.lengths.iter_chunk_infos(next) {
                let request = Request {
                    index: next.get(),
                    begin: chunk.offset,
                    length: chunk.size,
                };

                // Download limits throttle the reads of piece data (see
                // `throttle_download`), and the request window shrinks with
                // the peer's rate while a limit applies, so that requests
                // made before a limit change cannot arrive at full speed.
                aframe!(self.wait_for_request_slot()).await;

                match self
                    .state
                    .peers
                    .with_live_mut(handle, "add chunk request", |live| {
                        live.add_inflight_request(chunk)
                    }) {
                    Some(true) => {}
                    Some(false) => {
                        // This request was already in-flight for this peer for this chunk.
                        // This might happen in theory, but not very likely.
                        //
                        // Example:
                        // someone stole a piece from us, and then died, the piece became "needed" again, and we reserved it
                        // all before the piece request was processed by us.
                        warn!(
                            id = self.state.shared.id,
                            info_hash = ?self.state.shared.info_hash,
                            addr = ?self.addr,
                            "we already requested {:?} previously",
                            chunk
                        );
                        continue;
                    }
                    // peer died
                    None => return Ok(()),
                };

                if self
                    .tx
                    .send(WriterRequest::Message(Message::Request(request)))
                    .is_err()
                {
                    return Ok(());
                }
            }
        }
    }

    fn on_i_am_choked(&self) {
        self.lock_flow_control("i_am_choked = true").i_am_choked = true;
        self.state
            .peers
            .with_live_mut(self.addr, "peer_choking", |l| l.peer_choking = true);
        self.notify_request_slots_changed();
    }

    fn on_peer_interested(&self) {
        trace!("peer is interested");
        self.state.peers.mark_peer_interested(self.addr, true);
        self.state.maybe_unchoke_interested(self.addr);
        self.on_bitfield_notify.notify_waiters();
    }

    fn on_peer_not_interested(&self) {
        trace!("peer is not interested");
        self.state.peers.mark_peer_interested(self.addr, false);
        self.state.choke_not_interested(self.addr);
        self.on_bitfield_notify.notify_waiters();
    }

    fn on_i_am_unchoked(&self) {
        trace!("we are unchoked");
        self.lock_flow_control("i_am_choked = false").i_am_choked = false;
        self.state
            .peers
            .with_live_mut(self.addr, "peer_choking", |l| l.peer_choking = false);
        self.notify_request_slots_changed();
    }

    async fn on_received_piece(&self, piece: Piece<ByteBuf<'_>>) -> anyhow::Result<()> {
        let piece_index = self
            .state
            .lengths
            .validate_piece_index(piece.index)
            .with_context(|| format!("peer sent an invalid piece {}", piece.index))?;
        let chunk_info = match self.state.lengths.chunk_info_from_received_data(
            piece_index,
            piece.begin,
            piece.len().try_into().context("bug")?,
        ) {
            Some(i) => i,
            None => {
                anyhow::bail!("peer sent us an invalid piece {:?}", piece,);
            }
        };

        // Peer chunk/byte counters.
        self.counters
            .fetched_bytes
            .fetch_add(piece.len() as u64, Ordering::Relaxed);
        self.counters.fetched_chunks.fetch_add(1, Ordering::Relaxed);

        let should_process = self
            .state
            .peers
            .with_live_mut(self.addr, "inflight_requests.remove", |h| {
                match h.remove_inflight_request(&chunk_info) {
                    RemoveInflightRequestResult::Expected => {
                        h.last_data_received = std::time::Instant::now();
                        Ok(true)
                    }
                    RemoveInflightRequestResult::LateCanceled => {
                        trace!(?piece, "peer sent us a chunk we did not ask for");
                        Ok(false)
                    }
                    RemoveInflightRequestResult::Unexpected => anyhow::bail!(
                        "peer sent us a piece we did not ask. Inflight requests: {:?}. Got: {:?}",
                        h.inflight_requests_debug(),
                        piece,
                    ),
                }
            })
            .context("peer not found")??;

        if !should_process {
            self.state.add_wasted(0, piece.len() as u64);
            return Ok(());
        }

        // This one is used to calculate download speed.
        self.state
            .stats
            .fetched_bytes
            .fetch_add(piece.len() as u64, Ordering::Relaxed);
        self.state
            .session_stats
            .counters
            .fetched_bytes
            .fetch_add(piece.len() as u64, Ordering::Relaxed);

        fn write_to_disk(
            state: &TorrentStateLive,
            addr: PeerHandle,
            counters: &AtomicPeerCounters,
            piece: &Piece<ByteBuf<'_>>,
            chunk_info: &ChunkInfo,
        ) -> anyhow::Result<()> {
            let index = piece.index;

            // If someone stole the piece by now, ignore it.
            // However if they didn't, don't let them steal it while we are writing.
            // So that by the time we are done writing AND if it was the last piece,
            // we can actually checksum etc.
            // Otherwise it might get into some weird state.
            let ppl_guard = {
                let g = state.lock_read("check_steal");

                let ppl = Some(state.piece_lock(piece.index as usize).read());

                match g.get_pieces()?.get_inflight(chunk_info.piece_index) {
                    Some(inflight) if inflight.peer == addr => {}
                    Some(inflight) => {
                        debug!(
                            "in-flight piece {} was stolen by {}, ignoring",
                            chunk_info.piece_index, inflight.peer
                        );
                        state.add_wasted(0, piece.len() as u64);
                        return Ok(());
                    }
                    None => {
                        debug!(
                            "in-flight piece {} not found. it was probably completed by someone else",
                            chunk_info.piece_index
                        );
                        state.add_wasted(0, piece.len() as u64);
                        return Ok(());
                    }
                };

                ppl
            };

            // While we hold per piece lock, noone can steal it.
            // So we can proceed writing knowing that the piece is ours now and will still be by the time
            // the write is finished.
            //

            if !cfg!(feature = "_disable_disk_write_net_benchmark") {
                match state.file_ops().write_chunk(addr, piece, chunk_info) {
                    Ok(()) => {}
                    Err(e) => {
                        error!(
                            id = state.shared.id,
                            info_hash = ?state.shared.info_hash,
                            "FATAL: error writing chunk to disk: {e:#}"
                        );
                        return state.on_fatal_error(e);
                    }
                };
            }

            let full_piece_download_time = {
                let mut g = state.lock_write("mark_chunk_downloaded");
                let chunk_marking_result = g.get_pieces_mut()?.mark_chunk_downloaded(piece);
                trace!(?piece, chunk_marking_result=?chunk_marking_result);

                match chunk_marking_result {
                    Some(ChunkMarkingResult::Completed) => {
                        trace!("piece={} done, will write and checksum", piece.index);
                        // Remove from inflight to prevent others from stealing it during hash check.
                        g.get_pieces_mut()?.take_inflight(chunk_info.piece_index)
                    }
                    Some(ChunkMarkingResult::PreviouslyCompleted) => {
                        // TODO: we might need to send cancellations here.
                        debug!("piece={} was done by someone else, ignoring", piece.index);
                        state.add_wasted(0, piece.len() as u64);
                        return Ok(());
                    }
                    Some(ChunkMarkingResult::NotCompleted) => None,
                    None => {
                        anyhow::bail!(
                            "bogus data received: {:?}, cannot map this to a chunk, dropping peer",
                            piece
                        );
                    }
                }
            };

            // We don't care about per piece lock anymore, as it's removed from inflight pieces.
            // It shouldn't impact perf anyway, but dropping just in case.
            drop(ppl_guard);

            let full_piece_download_time = match full_piece_download_time {
                Some(t) => t,
                None => return Ok(()),
            };

            match state
                .file_ops()
                .check_piece(chunk_info.piece_index)
                .with_context(|| format!("error checking piece={index}"))?
            {
                true => {
                    {
                        let mut g = state.lock_write("mark_piece_downloaded");
                        g.get_pieces_mut()?
                            .mark_piece_hash_ok(chunk_info.piece_index, &state.metadata.file_infos);
                    }

                    // Global piece counters.
                    let piece_len = state.lengths.piece_length(chunk_info.piece_index) as u64;
                    state
                        .stats
                        .downloaded_and_checked_bytes
                        // This counter is used to compute "is_finished", so using
                        // stronger ordering.
                        .fetch_add(piece_len, Ordering::Release);
                    state
                        .stats
                        .downloaded_and_checked_pieces
                        // This counter is used to compute "is_finished", so using
                        // stronger ordering.
                        .fetch_add(1, Ordering::Release);
                    state
                        .stats
                        .have_bytes
                        .fetch_add(piece_len, Ordering::Relaxed);
                    #[allow(clippy::cast_possible_truncation)]
                    state.stats.total_piece_download_ms.fetch_add(
                        full_piece_download_time.as_millis() as u64,
                        Ordering::Relaxed,
                    );

                    // Per-peer piece counters.
                    counters.on_piece_completed(piece_len, full_piece_download_time);
                    state.peers.reset_peer_backoff(addr);

                    trace!(piece = index, "successfully downloaded and verified");

                    state.on_piece_completed(chunk_info.piece_index)?;

                    state.transmit_haves(chunk_info.piece_index);
                }
                false => {
                    warn!(
                        id = state.shared.id,
                        info_hash = ?state.shared.info_hash,
                        ?addr,
                        "checksum for piece={} did not validate. disconnecting peer.", index
                    );
                    state
                        .lock_write("mark_piece_broken")
                        .get_pieces_mut()?
                        .mark_piece_hash_failed(chunk_info.piece_index);
                    state.add_wasted(
                        u64::from(state.lengths.piece_length(chunk_info.piece_index)),
                        0,
                    );
                    state.new_pieces_notify.notify_waiters();
                    anyhow::bail!("i am probably a bogus peer. dying.")
                }
            };
            Ok(())
        }

        self.state
            .shared
            .spawner
            .block_in_place_with_semaphore(|| {
                write_to_disk(&self.state, self.addr, &self.counters, &piece, &chunk_info)
            })
            .await
            .with_context(|| format!("error processing received chunk {chunk_info:?}"))?;

        Ok(())
    }

    /// BEP 52 `hashes` / `hash reject`: only replies to the layer fetch of a
    /// hybrid torrent are used (verified against the pieces roots); others
    /// are ignored.
    fn on_hashes_reply(&self, req: &HashRequest, data: Option<&[u8]>) -> anyhow::Result<()> {
        let progress = self
            .state
            .v2_hashes
            .on_fetch_reply(&self.state.v2_ctx(), self.addr, req, data)
            .context("BEP 52 hashes")?;
        for r in progress.requests {
            self.tx
                .send(WriterRequest::Message(Message::HashRequest(r)))?;
        }
        if !progress.known_spans.is_empty() {
            debug!(
                id = self.state.shared.id,
                files = progress.known_spans.len(),
                "fetched missing piece layers from {}",
                self.addr
            );
            self.state.on_v2_layers_known(progress.known_spans);
        }
        if let Some(layers) = progress.done {
            debug!(
                id = self.state.shared.id,
                "fetched the piece layers of a hybrid torrent from {}", self.addr
            );
            if self.state.shared.v2_fetched_layers.set(layers).is_ok() {
                self.state
                    .shared
                    .v2_layers_unsaved
                    .store(true, Ordering::Release);
            }
        }
        Ok(())
    }

    /// BEP 52: answer with piece-layer hashes and their proof, or reject.
    /// Replies are capped per peer: requests beyond are dropped.
    fn on_hash_request(&self, req: HashRequest) -> anyhow::Result<()> {
        if self
            .queued_hash_replies
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < MAX_QUEUED_HASH_REPLIES).then_some(n + 1)
            })
            .is_err()
        {
            trace!("ignoring hash request: too many queued");
            return Ok(());
        }
        let reply = self.state.v2_hashes.reply(&self.state.v2_ctx(), &req);
        self.tx.send(WriterRequest::V2Hashes(req, reply))?;
        Ok(())
    }

    fn send_metadata_piece(&self, piece_id: u32) -> anyhow::Result<()> {
        let data = &self.state.metadata.info_bytes;
        let metadata_size = data.len();
        if metadata_size == 0 {
            anyhow::bail!("peer requested for info metadata but we don't have it")
        }
        let Some(range) = metadata_piece_range(metadata_size, piece_id) else {
            bail!("piece out of bounds")
        };
        let total_size: u32 = data
            .len()
            .try_into()
            .context("can't send metadata: len doesn't fit into u32")?;
        let data = data.slice(range);

        // Requests are cheap to send and the answers are 16 KiB: don't queue
        // more than a few for a peer that isn't reading them.
        if self
            .queued_metadata_pieces
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < MAX_QUEUED_METADATA_PIECES).then_some(n + 1)
            })
            .is_err()
        {
            trace!(piece_id, "ignoring ut_metadata request: too many queued");
            return Ok(());
        }

        self.tx
            .send(WriterRequest::UtMetadata(UtMetadata::Data(
                UtMetadataData::from_bytes(piece_id, total_size, data.into()),
            )))
            .context("error sending UtMetadata: channel closed")?;
        Ok(())
    }

    fn on_pex_message(&self, msg: UtPex<ByteBuf<'_>>) {
        {
            let now = Instant::now();
            let mut last = self.pex.last_message.lock();
            if last.is_some_and(|t| now.saturating_duration_since(t) < PEX_MIN_INTERVAL) {
                debug!(peer = ?self.addr, "ignoring PEX message: sent too often");
                return;
            }
            *last = Some(now);
        }
        // Dropped peers are gone: never connect to them.
        pex_candidates(msg.added_peers().map(|p| p.addr), self.addr)
            .into_iter()
            .for_each(|addr| {
                self.state
                    .add_peer_with_source(addr, PeerSource::Pex)
                    .map_err(|error| {
                        warn!(
                            id = self.state.shared.id,
                            info_hash = ?self.state.shared.info_hash,
                            peer = ?addr,
                            "failed to add peer: {error:#}"
                        );
                        error
                    })
                    .ok();
            });
    }

    fn is_choked(&self) -> bool {
        self.lock_flow_control("is_choked").i_am_choked
    }

    fn can_send_request(&self) -> bool {
        let (i_am_choked, request_window) = {
            let flow = self.lock_flow_control("can_send_request");
            (flow.i_am_choked, flow.request_window)
        };

        if i_am_choked {
            return false;
        }

        let Some((requested, down_bps)) = self.state.peers.with_live(self.addr, |live| {
            (live.requested_inflight_count(), live.rates.down_bps)
        }) else {
            return false;
        };
        let window = if self.download_limited() {
            limited_request_window(down_bps, request_window)
        } else {
            request_window
        };
        requested < window
    }

    fn lock_flow_control(
        &self,
        reason: &'static str,
    ) -> TimedExistence<MutexGuard<'_, PeerFlowControl>> {
        TimedExistence::new(timeit(reason, || self.flow_control.lock()), reason)
    }

    fn request_slots_changed(&self) -> Option<Arc<Notify>> {
        self.state
            .peers
            .with_live(self.addr, |live| live.request_slots_changed())
    }

    fn notify_request_slots_changed(&self) {
        if let Some(notify) = self.request_slots_changed() {
            notify.notify_waiters();
        }
    }
}

fn format_peer_client_name(value: &ByteBuf<'_>) -> Option<String> {
    let client_name = String::from_utf8_lossy(value.as_ref()).trim().to_string();
    if client_name.is_empty() {
        return None;
    }

    Some(client_name)
}

/// Byte range of ut_metadata piece `piece_id` in metadata of `metadata_size`
/// bytes, or `None` when the piece does not exist. Peers choose `piece_id`,
/// so this must never panic (the process aborts on panic).
fn metadata_piece_range(metadata_size: usize, piece_id: u32) -> Option<std::ops::Range<usize>> {
    let offset = (piece_id as usize).checked_mul(CHUNK_SIZE as usize)?;
    if offset >= metadata_size {
        return None;
    }
    Some(offset..(offset + CHUNK_SIZE as usize).min(metadata_size))
}

#[cfg(test)]
mod request_window_tests {
    use super::limited_request_window;

    #[test]
    fn three_seconds_of_the_rate_within_bounds() {
        // A new or stalled peer still gets a couple of requests.
        assert_eq!(limited_request_window(0, 128), 2);
        assert_eq!(limited_request_window(1000, 128), 2);
        // 800 KiB/s: 3 s = 150 blocks, capped by the normal window.
        assert_eq!(limited_request_window(800 * 1024, 128), 128);
        assert_eq!(limited_request_window(800 * 1024, 250), 150);
        // 100 KiB/s: 300 KiB = 18.75 blocks, rounded up.
        assert_eq!(limited_request_window(100 * 1024, 128), 19);
        // A peer that allows a single request.
        assert_eq!(limited_request_window(100 * 1024, 1), 1);
    }
}

#[cfg(test)]
mod metadata_piece_tests {
    use super::metadata_piece_range;

    #[test]
    fn out_of_range_metadata_pieces_are_refused() {
        // 20000 bytes = 2 pieces; piece 2 used to slice 32768..20000 and panic.
        assert_eq!(metadata_piece_range(20000, 0), Some(0..16384));
        assert_eq!(metadata_piece_range(20000, 1), Some(16384..20000));
        assert_eq!(metadata_piece_range(20000, 2), None);
        assert_eq!(metadata_piece_range(20000, u32::MAX), None);
        assert_eq!(metadata_piece_range(32768, 1), Some(16384..32768));
        assert_eq!(metadata_piece_range(32768, 2), None);
        assert_eq!(metadata_piece_range(0, 0), None);
    }
}
