use std::{
    net::{Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering},
    },
    task::Poll,
    time::{Duration, Instant},
};

use crate::{
    DHT_ANNOUNCE_INTERVAL, Error, INACTIVITY_TIMEOUT, LookupPacer, REQUERY_INTERVAL,
    RESPONSE_TIMEOUT,
    bprotocol::{
        self, AnnouncePeer, CompactNodeInfo, CompactNodeInfoOwned, ErrorDescription,
        FindNodeRequest, GetPeersRequest, Message, MessageKind, Node, PingRequest, Response, Want,
    },
    peer_store::PeerStore,
    routing_table::{InsertResult, NodeStatus, RoutingTable},
};
use backon::{ExponentialBuilder, Retryable};
use bencode::ByteBufOwned;
use dashmap::DashMap;
use futures::{FutureExt, Stream, StreamExt, future::BoxFuture, stream::FuturesUnordered};

use leaky_bucket::RateLimiter;
use librqbit_core::{
    compact_ip::{CompactSerialize, CompactSerializeFixedLen},
    crate_version,
    hash_id::Id20,
    peer_id::generate_azereus_style,
    spawn_utils::{spawn, spawn_with_cancel},
};
use librqbit_dualstack_sockets::{BindDevice, UdpSocket};
use parking_lot::{Mutex, RwLock};

use serde::Serialize;
use tokio::sync::mpsc::{Sender, UnboundedReceiver, UnboundedSender, channel, unbounded_channel};

use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, debug_span, info, trace, warn};

/// How often an empty routing table is noticed and bootstrapped again.
const REBOOTSTRAP_CHECK_INTERVAL: Duration = Duration::from_secs(30);

fn now() -> Instant {
    Instant::now()
}

#[derive(Debug, Serialize)]
pub struct DhtStats {
    #[serde(serialize_with = "crate::utils::serialize_id20")]
    pub id: Id20,
    pub outstanding_requests: usize,
    pub routing_table_size: usize,
    pub routing_table_size_v6: usize,
}

/// Torrent lookup rounds allowed to run at the same time.
const MAX_CONCURRENT_LOOKUPS: usize = 32;
/// Same, for the first round of each torrent.
const MAX_CONCURRENT_FIRST_LOOKUPS: usize = 8;
/// Requests one lookup round keeps in flight (libtorrent's
/// `search_branching` is 5; ours also bounds the waiters on the rate limiter).
const MAX_INFLIGHT_PER_LOOKUP: usize = 8;
/// Nodes taken from one response (the closest to the target). A response
/// can carry hundreds of compact nodes; a hostile node would otherwise make
/// every reply fan out into hundreds of new requests at each depth.
const MAX_NODES_PER_RESPONSE: usize = 16;
/// Requests one lookup (one get_peers round, one find_node) may send. A
/// converging lookup normally stops after 20 to 60 (see `LOOKUP_K`); this is
/// only the safety net against networks that keep handing out closer nodes.
const MAX_REQUESTS_PER_LOOKUP: usize = 100;
/// A lookup is done when the `LOOKUP_K` closest nodes it knows have all
/// answered (or failed) and none of the nodes in flight is closer: the
/// Kademlia bucket size, libtorrent's traversal `m_node.m_table.bucket_size()`.
/// Nodes farther away than the K-th closest responder are never asked.
const LOOKUP_K: usize = 8;
/// Routing table nodes a lookup starts from (libtorrent seeds its traversal
/// with twice the bucket size). Only the closest are asked unless some fail.
const LOOKUP_SEEDS: usize = 2 * LOOKUP_K;
/// Hops away from the routing table a lookup may go. Rounds end by
/// converging long before this; it bounds walks through hostile nodes.
const LOOKUP_MAX_DEPTH: usize = 8;

/// Time between two routing table pings of one address family.
const PING_SPACING: Duration = Duration::from_millis(200);

/// Default session-wide cap on outgoing DHT queries (get_peers, find_node,
/// ping, announce_peer), shared by all lookups and the routing table upkeep.
/// libtorrent limits all DHT traffic to `dht_upload_rate_limit` = 8000 B/s.
/// A query is about 100 bytes of payload plus 28 bytes of UDP/IP headers;
/// libtorrent's budget also covers its (larger, 250 to 450 byte) replies, so
/// it leaves room for roughly 30 queries a second. Our replies are limited
/// per source instead (`query_limit`). A converged lookup sends 20 to 60
/// queries, so steady seeding (one round per torrent per 15 min) needs well
/// under 1 query/s; the cap only spreads out bursts such as many torrents
/// starting at once, which used to reach hundreds of distinct nodes (and as
/// many NAT mappings on the user's router) within seconds.
const DEFAULT_QUERIES_PER_SECOND: usize = 32;
/// Burst the limiter allows on top of the rate: one or two lookups at full
/// speed.
const QUERY_BURST_SECONDS: usize = 2;

struct OutstandingRequest {
    done: tokio::sync::oneshot::Sender<crate::Result<ResponseOrError>>,
}

pub struct WorkerSendRequest {
    // If this is set, we are tracking the response in inflight_by_transaction_id
    our_tid: Option<u16>,
    message: Message<ByteBufOwned>,
    addr: SocketAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LookupNodeState {
    /// Known, not asked yet.
    Candidate,
    InFlight,
    Responded,
    /// Timed out, failed to send or answered with an error.
    Failed,
}

/// A node one lookup round has heard of.
#[derive(Debug)]
struct LookupNode {
    id: Id20,
    addr: SocketAddr,
    depth: u8,
    state: LookupNodeState,
    /// get_peers: the token to announce with.
    token: Option<ByteBufOwned>,
}

fn make_rate_limiter() -> RateLimiter {
    let qps = std::env::var("DHT_QUERIES_PER_SECOND")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_QUERIES_PER_SECOND);
    make_rate_limiter_with(qps)
}

fn make_rate_limiter_with(queries_per_second: usize) -> RateLimiter {
    let qps = queries_per_second.max(1);
    let burst = qps * QUERY_BURST_SECONDS;
    RateLimiter::builder()
        .initial(burst)
        .max(burst)
        .interval(Duration::from_secs(1) / qps as u32)
        .refill(1)
        // First come, first served: a lookup is not starved by others.
        .fair(true)
        .build()
}

trait RecursiveRequestCallbacks: Sized + Send + Sync + 'static {
    fn on_request_start(&self, req: &RecursiveRequest<Self>, target_node: Id20, addr: SocketAddr);
    fn on_request_end(
        &self,
        req: &RecursiveRequest<Self>,
        target_node: Id20,
        addr: SocketAddr,
        resp: &crate::Result<ResponseOrError>,
    );
}

struct RecursiveRequestCallbacksGetPeers {
    // Id20::from_str("0000ffffffffffffffffffffffffffffffffffff").unwrap()
    min_distance_to_announce: Id20,
    announce_port: Option<u16>,
    // Announce in the current round (see `DHT_ANNOUNCE_INTERVAL`).
    announce_now: AtomicBool,
}

impl RecursiveRequestCallbacks for RecursiveRequestCallbacksGetPeers {
    fn on_request_start(&self, _: &RecursiveRequest<Self>, _: Id20, _: SocketAddr) {}

    fn on_request_end(
        &self,
        req: &RecursiveRequest<Self>,
        target_node: Id20,
        addr: SocketAddr,
        resp: &crate::Result<ResponseOrError>,
    ) {
        // Like libtorrent, every node that answers a lookup is a routing
        // table candidate, and an answer keeps its entry (and bucket) fresh,
        // which spares pings and bucket refresh lookups.
        let mut rt = req.dht.get_table_for_addr(addr).write();
        match resp {
            Ok(ResponseOrError::Response(_)) => {
                if !matches!(rt.add_node(target_node, addr), InsertResult::Ignored) {
                    rt.mark_response(&target_node, now());
                }
            }
            _ => {
                rt.mark_error(&target_node);
            }
        }
    }
}

struct RecursiveRequestCallbacksFindNodes {}
impl RecursiveRequestCallbacks for RecursiveRequestCallbacksFindNodes {
    fn on_request_start(&self, req: &RecursiveRequest<Self>, target_node: Id20, addr: SocketAddr) {
        let mut rt = req.dht.get_table_for_addr(addr).write();
        match rt.add_node(target_node, addr) {
            InsertResult::WasExisting | InsertResult::ReplacedBad(_) | InsertResult::Added => {
                rt.mark_outgoing_request(&target_node, now());
            }
            InsertResult::Ignored => {}
        }
    }

    fn on_request_end(
        &self,
        req: &RecursiveRequest<Self>,
        target_node: Id20,
        addr: SocketAddr,
        resp: &crate::Result<ResponseOrError>,
    ) {
        let mut table = req.dht.get_table_for_addr(addr).write();
        if resp.is_ok() {
            table.mark_response(&target_node, now());
        } else {
            table.mark_error(&target_node);
        }
    }
}

/// One iterative lookup (a get_peers round or a find_node), run like
/// libtorrent's traversal: nodes are asked closest first, and only while
/// they are closer to the target than the `LOOKUP_K`-th closest node that
/// answered or is being asked. The round ends when nothing is in flight and
/// no candidate is left inside that window.
struct RecursiveRequest<C: RecursiveRequestCallbacks> {
    max_depth: usize,
    useful_nodes_limit: usize,
    info_hash: Id20,
    request: Request,
    dht: Arc<DhtState>,
    // Sorted by distance to `info_hash`.
    useful_nodes: Mutex<Vec<LookupNode>>,
    peer_tx: tokio::sync::mpsc::UnboundedSender<SocketAddr>,
    // Requests sent in the current round (capped by MAX_REQUESTS_PER_LOOKUP).
    queued_requests: AtomicUsize,
    callbacks: C,
}

pub struct RequestPeersStream {
    rx: tokio::sync::mpsc::UnboundedReceiver<SocketAddr>,
    cancel_join_handle_v4: tokio::task::JoinHandle<()>,
    cancel_join_handle_v6: tokio::task::JoinHandle<()>,
}

impl RequestPeersStream {
    fn new(
        dht: Arc<DhtState>,
        info_hash: Id20,
        announce_port: Option<u16>,
        pacer: Arc<LookupPacer>,
    ) -> Self {
        let (peer_tx, peer_rx) = unbounded_channel();
        let make = |is_v4: bool, dht: Arc<DhtState>, peer_tx: UnboundedSender<SocketAddr>| {
            let rp = Arc::new(RecursiveRequest {
                max_depth: LOOKUP_MAX_DEPTH,
                info_hash,
                useful_nodes_limit: 128,
                request: Request::GetPeers(info_hash),
                dht,
                useful_nodes: Mutex::new(Vec::new()),
                peer_tx,
                queued_requests: AtomicUsize::new(0),
                callbacks: RecursiveRequestCallbacksGetPeers {
                    min_distance_to_announce: Id20::from_str(
                        "0000ffffffffffffffffffffffffffffffffffff",
                    )
                    .unwrap(),
                    announce_port,
                    announce_now: AtomicBool::new(false),
                },
            });
            rp.request_peers_forever(is_v4, pacer.clone())
        };

        let v4 = make(true, dht.clone(), peer_tx.clone());
        let v6 = make(false, dht, peer_tx);

        Self {
            rx: peer_rx,
            cancel_join_handle_v4: v4,
            cancel_join_handle_v6: v6,
        }
    }
}

impl Drop for RequestPeersStream {
    fn drop(&mut self) {
        self.cancel_join_handle_v4.abort();
        self.cancel_join_handle_v6.abort();
    }
}

impl Stream for RequestPeersStream {
    type Item = SocketAddr;

    #[inline(never)]
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

impl RecursiveRequest<RecursiveRequestCallbacksFindNodes> {
    /// A find_node lookup for `target`, starting from routing table nodes
    /// (`known`) and hosts known only by address (`addrs`, bootstrap).
    async fn find_node_for_routing_table(
        dht: Arc<DhtState>,
        target: Id20,
        known: impl Iterator<Item = (Id20, SocketAddr)>,
        addrs: impl Iterator<Item = SocketAddr>,
    ) -> crate::Result<()> {
        let req = RecursiveRequest {
            max_depth: LOOKUP_MAX_DEPTH,
            info_hash: target,
            request: Request::FindNode(target),
            dht,
            useful_nodes_limit: 32,
            useful_nodes: Mutex::new(Vec::new()),
            peer_tx: unbounded_channel().0,
            queued_requests: AtomicUsize::new(0),
            callbacks: RecursiveRequestCallbacksFindNodes {},
        };
        for (id, addr) in known {
            req.add_candidate(id, addr, 0);
        }
        let addrs: Vec<SocketAddr> = addrs.collect();
        let initial_addrs = addrs.len();
        let (successes, errors) = req
            .run_round(addrs)
            .instrument(debug_span!("find_node", target = format!("{target:?}")))
            .await;
        if successes == 0 {
            return Err(Error::NoSuccessfulLookups { errors });
        }
        debug!(
            "finished, successes = {successes}, errors = {errors}, initial_addrs = {initial_addrs}"
        );
        Ok(())
    }
}

impl RecursiveRequest<RecursiveRequestCallbacksGetPeers> {
    fn request_peers_forever(
        self: &Arc<Self>,
        is_v4: bool,
        pacer: Arc<LookupPacer>,
    ) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        spawn::<crate::Error>(
            debug_span!(parent: None, "get_peers", is_v4, info_hash = format!("{:?}", self.info_hash)),
            "get_peers",
            async move {
                let this = &this;
                let mut iteration = 0;
                let mut empty_rounds = 0u32;
                let mut wake_rx = pacer.subscribe();
                let mut forced_seen = *wake_rx.borrow();
                let mut forced = false;
                // Announcing ourselves follows libtorrent's cadence even when
                // the pacer asks for peers more often. The margin keeps a
                // jittered 15 min round from skipping its announce.
                let announce_gap = DHT_ANNOUNCE_INTERVAL * 9 / 10;
                let mut last_announce: Option<tokio::time::Instant> = None;
                // A lookup only occupies memory while one of its rounds is
                // running; rounds of all torrents share `lookup_slots`, so a
                // client with thousands of torrents does not run thousands of
                // iterative lookups (and their request futures) at once.
                loop {
                    trace!("iteration {}", iteration);
                    // A torrent's first round has its own slots, so a torrent
                    // added while many others are cycling is not queued
                    // behind all of them.
                    let slots = if iteration == 0 {
                        &this.dht.first_lookup_slots
                    } else {
                        &this.dht.lookup_slots
                    };
                    let permit = slots.acquire().await.ok();
                    let announce =
                        forced || last_announce.is_none_or(|t| t.elapsed() >= announce_gap);
                    this.callbacks
                        .announce_now
                        .store(announce, Ordering::Relaxed);
                    // `Err`: a fixed retry delay; `Ok`: paced, with an
                    // optional cap.
                    let n = this.get_peers_root(is_v4);
                    let next: Result<Option<Duration>, Duration> = if n == 0 {
                        // Nothing to ask yet (routing table still empty, or no
                        // IPv6 nodes at all): retry soon, then back off.
                        empty_rounds = empty_rounds.saturating_add(1);
                        Err(Duration::from_secs(1 << (empty_rounds / 3).min(3)))
                    } else {
                        empty_rounds = 0;
                        let (ok, errors) = this.run_round(Vec::new()).await;
                        debug!(
                            ok,
                            errors,
                            sent = this.queued_requests.load(Ordering::Relaxed),
                            "get_peers round done"
                        );
                        if announce && this.callbacks.announce_port.is_some() {
                            this.announce_to_closest().await;
                            last_announce = Some(tokio::time::Instant::now());
                        }
                        // Few nodes near the hash: the routing table is
                        // still thin, look again sooner.
                        Ok((n < 8).then(|| REQUERY_INTERVAL / 8 * (n as u32)))
                    };
                    drop(permit);
                    // Idle until the next round: give the scratch state back.
                    *this.useful_nodes.lock() = Vec::new();
                    this.queued_requests.store(0, Ordering::Relaxed);
                    forced = match next {
                        Err(d) => {
                            tokio::time::sleep(d).await;
                            false
                        }
                        Ok(cap) => {
                            let round_end = tokio::time::Instant::now();
                            pacer
                                .wait_next_round(&mut wake_rx, &mut forced_seen, round_end, cap)
                                .await
                        }
                    };
                    iteration += 1;
                }
            },
        )
    }

    /// Seeds the round with the routing table nodes closest to the hash;
    /// returns how many.
    fn get_peers_root(&self, is_v4: bool) -> usize {
        let table = if is_v4 {
            &self.dht.routing_table_v4
        } else {
            &self.dht.routing_table_v6
        };
        let seeds: Vec<(Id20, SocketAddr)> = table
            .read()
            .sorted_by_distance_from(self.info_hash, now())
            .iter()
            .map(|n| (n.id(), n.addr()))
            .take(LOOKUP_SEEDS)
            .collect();
        let count = seeds.len();
        for (id, addr) in seeds {
            self.add_candidate(id, addr, 0);
        }
        count
    }

    /// `announce_peer` to the `LOOKUP_K` closest nodes that answered with a
    /// token (libtorrent announces to the 8 closest), if they are near
    /// enough to the hash to be storing it.
    async fn announce_to_closest(&self) {
        let Some(port) = self.callbacks.announce_port else {
            return;
        };
        let targets: Vec<(SocketAddr, ByteBufOwned)> = self
            .useful_nodes
            .lock()
            .iter()
            .filter(|n| n.state == LookupNodeState::Responded)
            .filter_map(|n| Some((n.id, n.addr, n.token.clone()?)))
            .take(LOOKUP_K)
            .filter(|(id, _, _)| {
                self.info_hash.distance(id) <= self.callbacks.min_distance_to_announce
            })
            .map(|(_, addr, token)| (addr, token))
            .collect();
        for (addr, token) in targets {
            self.dht.rate_limiter.acquire_one().await;
            let (tid, message) = self.dht.create_request(
                Request::Announce {
                    info_hash: self.info_hash,
                    token,
                    port,
                },
                addr,
            );
            let _ = self.dht.worker_sender.send(WorkerSendRequest {
                our_tid: Some(tid),
                message,
                addr,
            });
        }
    }
}

impl<C: RecursiveRequestCallbacks> RecursiveRequest<C> {
    /// Runs the lookup until it converges (see [`RecursiveRequest`]).
    /// `addrs`: hosts without a known id (bootstrap), asked first; they do
    /// not count towards the closest nodes. Returns (answers, failures).
    async fn run_round(&self, addrs: Vec<SocketAddr>) -> (usize, usize) {
        let request_one = |id: Option<Id20>, addr: SocketAddr, depth: usize| {
            self.request_one(id, addr, depth)
                .instrument(debug_span!("addr", addr = addr.to_string()))
        };
        let mut futs = FuturesUnordered::new();
        for addr in addrs {
            if self.queued_requests.fetch_add(1, Ordering::Relaxed) < MAX_REQUESTS_PER_LOOKUP {
                futs.push(request_one(None, addr, 0));
            }
        }
        let (mut ok, mut errors) = (0, 0);
        loop {
            while futs.len() < MAX_INFLIGHT_PER_LOOKUP {
                match self.next_request() {
                    Some((id, addr, depth)) => futs.push(request_one(Some(id), addr, depth)),
                    None => break,
                }
            }
            match futs.next().await {
                Some(Ok(())) => ok += 1,
                Some(Err(e)) => {
                    debug!("error: {e:#}");
                    errors += 1;
                }
                None => break,
            }
        }
        (ok, errors)
    }

    /// The closest candidate inside the window of the `LOOKUP_K` closest
    /// nodes that answered or are being asked (failed nodes do not count),
    /// marked in flight; `None` if the window is full or the budget used up.
    fn next_request(&self) -> Option<(Id20, SocketAddr, usize)> {
        let mut nodes = self.useful_nodes.lock();
        let mut alive_or_pending = 0;
        for n in nodes.iter_mut() {
            match n.state {
                LookupNodeState::Failed => {}
                LookupNodeState::Responded | LookupNodeState::InFlight => {
                    alive_or_pending += 1;
                    if alive_or_pending >= LOOKUP_K {
                        return None;
                    }
                }
                LookupNodeState::Candidate => {
                    if self.queued_requests.fetch_add(1, Ordering::Relaxed)
                        >= MAX_REQUESTS_PER_LOOKUP
                    {
                        trace!("lookup request budget used up");
                        return None;
                    }
                    n.state = LookupNodeState::InFlight;
                    return Some((n.id, n.addr, n.depth as usize));
                }
            }
        }
        None
    }

    async fn request_one(
        &self,
        id: Option<Id20>,
        addr: SocketAddr,
        depth: usize,
    ) -> crate::Result<()> {
        if let Some(id) = id {
            self.callbacks.on_request_start(self, id, addr);
        }

        let response = self.dht.request(self.request.clone(), addr).await;
        self.mark_node_done(addr, &response);
        // Nodes we only know by address (bootstrap hosts) identify themselves
        // in the reply; a node that answered is a routing table candidate
        // like any other, even if it hands out no other nodes.
        let id = match (id, &response) {
            (None, Ok(ResponseOrError::Response(r))) => {
                self.callbacks.on_request_start(self, r.id, addr);
                Some(r.id)
            }
            (id, _) => id,
        };
        if let Some(id) = id {
            self.callbacks.on_request_end(self, id, addr, &response);
        }

        let response = match response {
            Ok(ResponseOrError::Response(r)) => r,
            Ok(ResponseOrError::Error(e)) => {
                debug!("error response: {e:?}");
                return Err(Error::ErrorResponse);
            }
            Err(e) => return Err(e),
        };

        if let Some(peers) = response.values {
            for peer in peers {
                if !acceptable_remote_addr(peer.0, addr) {
                    continue;
                }
                self.peer_tx.send(peer.0).ok().ok_or(Error::ReceiverDead)?;
            }
        }

        let mut nodes: Vec<_> = response
            .nodes
            .iter()
            .flat_map(|n| n.iter().map(|n| n.as_socketaddr()))
            .chain(
                response
                    .nodes6
                    .iter()
                    .flat_map(|n| n.iter().map(|n| n.as_socketaddr())),
            )
            .filter(|node| {
                addr.is_ipv4() == node.addr.is_ipv4() && acceptable_remote_addr(node.addr, addr)
            })
            .collect();
        if nodes.len() > MAX_NODES_PER_RESPONSE {
            nodes.sort_by_key(|n| n.id.distance(&self.info_hash));
            nodes.truncate(MAX_NODES_PER_RESPONSE);
        }

        for node in nodes {
            let added = self.add_candidate(node.id, node.addr, depth + 1);
            trace!(
                "added={}, id={:?}, addr={}, depth={}/{}",
                added,
                node.id,
                node.addr,
                depth + 1,
                self.max_depth
            );
        }
        Ok(())
    }

    /// Records the outcome of a request to a node of this round.
    fn mark_node_done(&self, addr: SocketAddr, response: &crate::Result<ResponseOrError>) {
        if let Some(node) = self.useful_nodes.lock().iter_mut().find(|n| n.addr == addr) {
            match response {
                Ok(ResponseOrError::Response(r)) => {
                    node.state = LookupNodeState::Responded;
                    node.token = r.token.clone();
                }
                Ok(ResponseOrError::Error(_)) | Err(_) => node.state = LookupNodeState::Failed,
            }
        }
    }

    /// Adds a node heard of at `depth` hops from the routing table as a
    /// candidate of this round. False if it is known already, too deep, us,
    /// on a host the round already has, or farther than the
    /// `useful_nodes_limit` closest nodes.
    fn add_candidate(&self, node_id: Id20, addr: SocketAddr, depth: usize) -> bool {
        if depth >= self.max_depth || node_id == self.dht.id {
            return false;
        }
        let mut nodes = self.useful_nodes.lock();
        if nodes.iter().any(|n| n.id == node_id) {
            return false;
        }
        // One node per IP address and lookup: a host handing out many ids
        // for itself (or for one victim address) gets one request.
        // (Loopback is keyed by port: only reachable from a loopback node, and
        // local test swarms share one address.)
        let same_host = |n: &LookupNode| {
            if addr.ip().is_loopback() {
                n.addr == addr
            } else {
                n.addr.ip() == addr.ip()
            }
        };
        if nodes.iter().any(same_host) {
            return false;
        }
        let distance = node_id.distance(&self.info_hash);
        let pos = nodes.partition_point(|n| n.id.distance(&self.info_hash) < distance);
        if pos >= self.useful_nodes_limit {
            return false;
        }
        if nodes.capacity() == 0 {
            // One allocation of the final size instead of doubling past it.
            nodes.reserve_exact(self.useful_nodes_limit + 1);
        }
        nodes.insert(
            pos,
            LookupNode {
                id: node_id,
                addr,
                depth: depth as u8,
                state: LookupNodeState::Candidate,
                token: None,
            },
        );
        // The farthest node is beyond the window anyway; if it is still in
        // flight, its answer is simply not recorded.
        nodes.truncate(self.useful_nodes_limit);
        true
    }
}

pub struct DhtState {
    id: Id20,
    next_transaction_id: AtomicU16,

    // Created requests: (transaction_id, addr) => Requests.
    // If we get a response, it gets removed from here.
    inflight_by_transaction_id: DashMap<(u16, SocketAddr), OutstandingRequest>,

    routing_table_v4: RwLock<RoutingTable>,
    routing_table_v6: RwLock<RoutingTable>,
    listen_addr: SocketAddr,

    // Sending requests to the worker.
    rate_limiter: RateLimiter,
    // This is to send raw messages
    worker_sender: UnboundedSender<WorkerSendRequest>,

    cancellation_token: CancellationToken,

    pub(crate) peer_store: PeerStore,

    // Bounds how many get_peers lookup rounds run concurrently.
    lookup_slots: tokio::sync::Semaphore,
    first_lookup_slots: tokio::sync::Semaphore,

    // Incoming queries answered per source (reflection limit).
    query_limiter: parking_lot::Mutex<crate::query_limit::QueryLimiter>,
}

impl DhtState {
    fn new_internal(
        id: Id20,
        sender: UnboundedSender<WorkerSendRequest>,
        routing_table_v4: Option<RoutingTable>,
        routing_table_v6: Option<RoutingTable>,
        listen_addr: SocketAddr,
        peer_store: PeerStore,
        cancellation_token: CancellationToken,
    ) -> Self {
        let routing_table_v4 = routing_table_v4.unwrap_or_else(|| RoutingTable::new(id, None));
        let routing_table_v6 = routing_table_v6.unwrap_or_else(|| RoutingTable::new(id, None));
        Self {
            id,
            next_transaction_id: AtomicU16::new(0),
            inflight_by_transaction_id: Default::default(),
            routing_table_v4: RwLock::new(routing_table_v4),
            routing_table_v6: RwLock::new(routing_table_v6),
            worker_sender: sender,
            listen_addr,
            rate_limiter: make_rate_limiter(),
            peer_store,
            cancellation_token,
            lookup_slots: tokio::sync::Semaphore::new(MAX_CONCURRENT_LOOKUPS),
            first_lookup_slots: tokio::sync::Semaphore::new(MAX_CONCURRENT_FIRST_LOOKUPS),
            query_limiter: parking_lot::Mutex::new(crate::query_limit::QueryLimiter::new(
                Instant::now(),
            )),
        }
    }

    async fn request(&self, request: Request, addr: SocketAddr) -> crate::Result<ResponseOrError> {
        // Wait for the session-wide query budget. The wait is not bounded by
        // a timeout: giving up would count as the node failing (and mark
        // routing table nodes bad). The waiters are bounded instead: every
        // caller keeps a fixed number of requests in flight (8 per lookup,
        // 40 lookup rounds, pings paced, refreshes one per stale bucket).
        self.rate_limiter.acquire_one().await;
        let (tid, message) = self.create_request(request, addr);
        let key = (tid, addr);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.inflight_by_transaction_id
            .insert(key, OutstandingRequest { done: tx });
        trace!("sending {message:?}");
        match self.worker_sender.send(WorkerSendRequest {
            our_tid: Some(tid),
            message,
            addr,
        }) {
            Ok(_) => {}
            Err(_) => {
                self.inflight_by_transaction_id.remove(&key);
                return Err(Error::DhtDead);
            }
        };
        match tokio::time::timeout(RESPONSE_TIMEOUT, rx).await {
            Ok(Ok(r)) => r.map(|r| {
                trace!("received {r:?}");
                r
            }),
            Ok(Err(_)) => {
                self.inflight_by_transaction_id.remove(&key);
                Err(Error::DhtDead)
            }
            Err(_) => {
                self.inflight_by_transaction_id.remove(&key);
                Err(Error::ResponseTimeout(RESPONSE_TIMEOUT))
            }
        }
    }

    fn create_request(&self, request: Request, addr: SocketAddr) -> (u16, Message<ByteBufOwned>) {
        let transaction_id = self.next_transaction_id.fetch_add(1, Ordering::Relaxed);
        let transaction_id_buf = [(transaction_id >> 8) as u8, (transaction_id & 0xff) as u8];

        let want = if addr.is_ipv6() {
            Some(Want::V6)
        } else {
            Some(Want::V4)
        };

        let message = match request {
            Request::GetPeers(info_hash) => Message {
                transaction_id: ByteBufOwned::from(transaction_id_buf.as_ref()),
                version: None,
                ip: None,
                kind: MessageKind::GetPeersRequest(GetPeersRequest {
                    id: self.id,
                    info_hash,
                    want,
                }),
            },
            Request::FindNode(target) => Message {
                transaction_id: ByteBufOwned::from(transaction_id_buf.as_ref()),
                version: None,
                ip: None,
                kind: MessageKind::FindNodeRequest(FindNodeRequest {
                    id: self.id,
                    target,
                    want,
                }),
            },
            Request::Ping => Message {
                transaction_id: ByteBufOwned::from(transaction_id_buf.as_ref()),
                version: None,
                ip: None,
                kind: MessageKind::PingRequest(PingRequest { id: self.id }),
            },
            Request::Announce {
                info_hash,
                token,
                port,
            } => Message {
                kind: MessageKind::AnnouncePeer(AnnouncePeer {
                    id: self.id,
                    implied_port: 0,
                    info_hash,
                    port,
                    token,
                }),
                transaction_id: ByteBufOwned::from(transaction_id_buf.as_ref()),
                version: None,
                ip: None,
            },
        };
        (transaction_id, message)
    }

    fn generate_compact_nodes_both(
        &self,
        target: Id20,
        want: Want,
    ) -> (
        Option<CompactNodeInfoOwned<SocketAddrV4>>,
        Option<CompactNodeInfoOwned<SocketAddrV6>>,
    ) {
        let now = now();
        match want {
            Want::V4 => (
                Some(self.generate_compact_nodes(target, &self.routing_table_v4.read(), now)),
                None,
            ),
            Want::V6 => (
                None,
                Some(self.generate_compact_nodes(target, &self.routing_table_v6.read(), now)),
            ),
            Want::Both => (
                Some(self.generate_compact_nodes(target, &self.routing_table_v4.read(), now)),
                Some(self.generate_compact_nodes(target, &self.routing_table_v6.read(), now)),
            ),
            Want::None => (None, None),
        }
    }

    fn get_table_for_addr(&self, addr: SocketAddr) -> &RwLock<RoutingTable> {
        if addr.is_ipv4() {
            &self.routing_table_v4
        } else {
            &self.routing_table_v6
        }
    }

    fn generate_compact_nodes<A>(
        &self,
        target: Id20,
        table: &RoutingTable,
        now: Instant,
    ) -> CompactNodeInfo<ByteBufOwned, A>
    where
        A: CompactSerialize + CompactSerializeFixedLen + FromSocketAddr,
        Node<A>: CompactSerialize + CompactSerializeFixedLen,
    {
        let it = table
            .sorted_by_distance_from(target, now)
            .into_iter()
            .filter_map(|r| {
                Some(Node {
                    id: r.id(),
                    addr: A::from_socket_addr(r.addr())?,
                })
            })
            .take(8);
        CompactNodeInfo::new_from_iter(it)
    }

    fn on_received_message(
        self: &Arc<Self>,
        msg: Message<ByteBufOwned>,
        addr: SocketAddr,
    ) -> crate::Result<()> {
        match &msg.kind {
            // If it's a response to a request we made, find the request task, notify it with the response,
            // and let it handle it.
            MessageKind::Error(_) | MessageKind::Response(_) => {
                let tid = msg
                    .get_our_transaction_id()
                    .ok_or(Error::BadTransactionId)?;
                let request = match self
                    .inflight_by_transaction_id
                    .remove(&(tid, addr))
                    .map(|(_, v)| v)
                {
                    Some(req) => req,
                    None => {
                        trace!(?msg, "outstanding request not found");
                        return Err(Error::RequestNotFound);
                    }
                };

                let response_or_error = match msg.kind {
                    MessageKind::Error(e) => ResponseOrError::Error(e),
                    MessageKind::Response(r) => ResponseOrError::Response(r),
                    _ => unreachable!(),
                };
                match request.done.send(Ok(response_or_error)) {
                    Ok(_) => {}
                    Err(e) => {
                        debug!(
                            "received response, but the receiver task is closed: {:?}",
                            e
                        );
                    }
                }
                return Ok(());
            }
            _ => {}
        };

        trace!("received query from {addr}: {msg:?}");

        if !self.query_limiter.lock().allow(addr.ip(), Instant::now()) {
            trace!(?addr, "dropping query: rate limit for this source");
            return Ok(());
        }

        match &msg.kind {
            // Otherwise, respond to a query.
            MessageKind::PingRequest(req) => {
                let message = Message {
                    transaction_id: msg.transaction_id,
                    version: None,
                    ip: Some(addr),
                    kind: MessageKind::Response(bprotocol::Response {
                        id: self.id,
                        ..Default::default()
                    }),
                };
                self.get_table_for_addr(addr)
                    .write()
                    .mark_last_query(&req.id, now());
                self.worker_sender
                    .send(WorkerSendRequest {
                        our_tid: None,
                        message,
                        addr,
                    })
                    .ok()
                    .ok_or(Error::DhtDead)?;
                Ok(())
            }
            MessageKind::AnnouncePeer(ann) => {
                self.get_table_for_addr(addr)
                    .write()
                    .mark_last_query(&ann.id, now());
                let added = self.peer_store.store_peer(ann, addr);
                trace!("{addr}: added_peer={added}, announce={ann:?}");
                let message = Message {
                    transaction_id: msg.transaction_id,
                    version: None,
                    ip: Some(addr),
                    kind: MessageKind::Response(bprotocol::Response {
                        id: self.id,
                        ..Default::default()
                    }),
                };
                self.worker_sender
                    .send(WorkerSendRequest {
                        our_tid: None,
                        message,
                        addr,
                    })
                    .ok()
                    .ok_or(Error::DhtDead)?;
                Ok(())
            }
            MessageKind::GetPeersRequest(req) => {
                let want = req
                    .want
                    .unwrap_or(if addr.is_ipv6() { Want::V6 } else { Want::V4 });
                let (nodes, nodes6) = self.generate_compact_nodes_both(req.info_hash, want);
                let compact_peer_info = self.peer_store.get_for_info_hash(req.info_hash, want);
                self.get_table_for_addr(addr)
                    .write()
                    .mark_last_query(&req.id, now());
                let message = Message {
                    transaction_id: msg.transaction_id,
                    version: None,
                    ip: Some(addr),
                    kind: MessageKind::Response(bprotocol::Response {
                        id: self.id,
                        nodes,
                        nodes6,
                        values: Some(compact_peer_info),
                        token: Some(ByteBufOwned::from(
                            &self.peer_store.gen_token_for(req.id, addr)[..],
                        )),
                    }),
                };
                self.worker_sender
                    .send(WorkerSendRequest {
                        our_tid: None,
                        message,
                        addr,
                    })
                    .ok()
                    .ok_or(Error::DhtDead)?;
                Ok(())
            }
            MessageKind::FindNodeRequest(req) => {
                let want = req
                    .want
                    .unwrap_or(if addr.is_ipv6() { Want::V6 } else { Want::V4 });
                let (nodes, nodes6) = self.generate_compact_nodes_both(req.target, want);
                self.get_table_for_addr(addr)
                    .write()
                    .mark_last_query(&req.id, now());
                let message = Message {
                    transaction_id: msg.transaction_id,
                    version: None,
                    ip: Some(addr),
                    kind: MessageKind::Response(bprotocol::Response {
                        id: self.id,
                        nodes,
                        nodes6,
                        ..Default::default()
                    }),
                };
                self.worker_sender
                    .send(WorkerSendRequest {
                        our_tid: None,
                        message,
                        addr,
                    })
                    .ok()
                    .ok_or(Error::DhtDead)?;
                Ok(())
            }
            _ => unreachable!(),
        }
    }

    pub fn get_stats(&self) -> DhtStats {
        DhtStats {
            id: self.id,
            outstanding_requests: self.inflight_by_transaction_id.len(),
            routing_table_size: self.routing_table_v4.read().len(),
            routing_table_size_v6: self.routing_table_v6.read().len(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Request {
    GetPeers(Id20),
    FindNode(Id20),
    Announce {
        info_hash: Id20,
        token: ByteBufOwned,
        port: u16,
    },
    Ping,
}

enum ResponseOrError {
    Response(Response<ByteBufOwned>),
    Error(ErrorDescription<ByteBufOwned>),
}

impl core::fmt::Debug for ResponseOrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Response(r) => write!(f, "{r:?}"),
            Self::Error(e) => write!(f, "{e:?}"),
        }
    }
}

struct DhtWorker {
    socket: UdpSocket,
    dht: Arc<DhtState>,
}

// Windows fails recv_from on a UDP socket with these for a single datagram, and the socket keeps
// working afterwards:
// - WSAEMSGSIZE: the datagram was longer than the buffer and got truncated.
// - WSAENETRESET: an earlier send_to got an ICMP time exceeded back.
// - WSAECONNRESET: an earlier send_to got an ICMP port unreachable back.
fn is_datagram_error(e: &std::io::Error) -> bool {
    const WSAEMSGSIZE: i32 = 10040;
    const WSAENETRESET: i32 = 10052;
    const WSAECONNRESET: i32 = 10054;
    cfg!(windows)
        && matches!(
            e.raw_os_error(),
            Some(WSAEMSGSIZE | WSAENETRESET | WSAECONNRESET)
        )
}

impl DhtWorker {
    fn on_send_error(&self, tid: u16, addr: SocketAddr, err: crate::Error) {
        if let Some((_, OutstandingRequest { done })) =
            self.dht.inflight_by_transaction_id.remove(&(tid, addr))
        {
            let _ = done.send(Err(err)).is_err();
        };
    }

    async fn bootstrap_hostname(&self, hostname: &str) -> crate::Result<()> {
        let addrs = tokio::net::lookup_host(hostname)
            .await
            .map_err(|err| Error::lookup(hostname, err))?
            .collect::<Vec<_>>();
        let v4 = RecursiveRequest::find_node_for_routing_table(
            self.dht.clone(),
            self.dht.id,
            std::iter::empty(),
            addrs.iter().copied().filter(|a| a.is_ipv4()),
        )
        .instrument(debug_span!("v4"));

        let v6 = RecursiveRequest::find_node_for_routing_table(
            self.dht.clone(),
            self.dht.id,
            std::iter::empty(),
            addrs.iter().copied().filter(|a| a.is_ipv6()),
        )
        .instrument(debug_span!("v6"));

        let (v4, v6) = tokio::join!(v4, v6);
        v4.or(v6)
    }

    async fn bootstrap_hostname_with_backoff(&self, addr: &str) -> crate::Result<()> {
        let backoff = ExponentialBuilder::new()
            .with_max_delay(Duration::from_secs(60))
            .with_jitter()
            .with_total_delay(Some(Duration::from_secs(86400)))
            .without_max_times();

        (|| self.bootstrap_hostname(addr))
            .retry(backoff)
            .notify(|error, retry_in| {
                warn!(?retry_in, ?addr, "error in bootstrap: {error:#}");
            })
            .instrument(debug_span!("bootstrap", hostname = addr))
            .await
    }

    async fn bootstrap(&self, bootstrap_addrs: &[String]) -> crate::Result<()> {
        if bootstrap_addrs.is_empty() {
            // Nothing to contact (e.g. the first node of a private network):
            // the node still answers queries and learns nodes from them.
            return Ok(());
        }
        let mut futs = FuturesUnordered::new();

        for addr in bootstrap_addrs.iter() {
            futs.push(self.bootstrap_hostname_with_backoff(addr));
        }
        let mut successes = 0;
        while let Some(resp) = futs.next().await {
            if resp.is_ok() {
                successes += 1
            }
        }
        if successes == 0 {
            return Err(Error::BootstrapFailed);
        }
        Ok(())
    }

    async fn bucket_refresher(&self, is_v4: bool) -> crate::Result<()> {
        let (tx, mut rx) = unbounded_channel();

        let table = if is_v4 {
            &self.dht.routing_table_v4
        } else {
            &self.dht.routing_table_v6
        };

        let mut futs = FuturesUnordered::new();
        let filler = async {
            let mut interval = tokio::time::interval(INACTIVITY_TIMEOUT);
            interval.tick().await;
            let mut iteration = 0;
            loop {
                interval.tick().await;
                let now = now();
                let mut found = 0;

                for bucket in table.read().iter_buckets() {
                    if now - bucket.leaf.last_refreshed < INACTIVITY_TIMEOUT {
                        continue;
                    }
                    found += 1;
                    let random_id = bucket.random_within();
                    tx.send(random_id).unwrap();
                }
                trace!("iteration {}, refreshing {} buckets", iteration, found);
                iteration += 1;
            }
        };

        tokio::pin!(filler);

        loop {
            tokio::select! {
                _ = &mut filler => {},
                random_id = rx.recv() => {
                    let random_id = random_id.unwrap();
                    let known = table
                        .read()
                        .sorted_by_distance_from(random_id, now())
                        .iter()
                        .map(|n| (n.id(), n.addr()))
                        .take(LOOKUP_SEEDS)
                        .collect::<Vec<_>>();
                    futs.push(
                        RecursiveRequest::find_node_for_routing_table(
                            self.dht.clone(), random_id, known.into_iter(), std::iter::empty()
                        ).instrument(debug_span!("refresh_bucket"))
                    );
                },
                _ = futs.next(), if !futs.is_empty() => {},
            }
        }
    }

    async fn pinger(&self, is_v4: bool) -> crate::Result<()> {
        let table = if is_v4 {
            &self.dht.routing_table_v4
        } else {
            &self.dht.routing_table_v6
        };
        let mut futs = FuturesUnordered::new();
        let mut interval = tokio::time::interval(INACTIVITY_TIMEOUT / 4);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let (tx, mut rx) = unbounded_channel();
        let looper = async {
            let mut iteration = 0;
            loop {
                interval.tick().await;
                let now = now();
                let due: Vec<(Id20, SocketAddr)> = table
                    .read()
                    .iter()
                    .filter(|node| {
                        matches!(
                            node.status(now),
                            NodeStatus::Questionable | NodeStatus::Unknown
                        )
                    })
                    .map(|node| (node.id(), node.addr()))
                    .collect();
                trace!("iteration {}, pinging {} nodes", iteration, due.len());
                // Spread the pings out instead of sending to the whole table
                // at once (after a restart every saved node is due): a full
                // table of 512 takes under two minutes, less than the
                // interval, and stays far below the session query cap.
                for (i, node) in due.into_iter().enumerate() {
                    if i > 0 {
                        tokio::time::sleep(PING_SPACING).await;
                    }
                    tx.send(node).unwrap();
                }
                iteration += 1;
            }
        };

        tokio::pin!(looper);

        loop {
            tokio::select! {
                _ = &mut looper => {},
                r = rx.recv() => {
                    let (id, addr) = r.unwrap();
                    futs.push(async move {
                        table.write().mark_outgoing_request(&id, now());
                        match self.dht.request(Request::Ping, addr).await {
                            Ok(_) => {
                                table.write().mark_response(&id, now());
                            },
                            Err(e) => {
                                table.write().mark_error(&id);
                                debug!("error: {e:#}");
                            }
                        }
                    }.instrument(debug_span!("ping", addr=addr.to_string())))
                },
                _ = futs.next(), if !futs.is_empty() => {},
            }
        }
    }

    async fn framer(
        &self,
        socket: &UdpSocket,
        mut input_rx: UnboundedReceiver<WorkerSendRequest>,
        output_tx: Sender<(Message<ByteBufOwned>, SocketAddr)>,
    ) -> crate::Result<()> {
        let writer = async {
            let mut buf = Vec::new();
            while let Some(WorkerSendRequest {
                our_tid,
                message,
                addr,
            }) = input_rx.recv().await
            {
                if our_tid.is_none() {
                    trace!("{}: sending {:?}", addr, &message);
                }
                buf.clear();
                bprotocol::serialize_message(
                    &mut buf,
                    message.transaction_id,
                    message.version,
                    message.ip,
                    message.kind,
                )
                .unwrap();
                if let Err(e) = socket.send_to(&buf, addr).await {
                    debug!("error sending to {addr}: {e:#}");
                    if let Some(tid) = our_tid {
                        self.on_send_error(tid, addr, Error::Send(e));
                    }
                }
            }
            Err(Error::DhtDead)
        };
        let reader = async {
            let mut buf = vec![0u8; 16384];
            loop {
                let (size, addr) = match socket.recv_from(&mut buf).await {
                    Ok(r) => r,
                    Err(e) if is_datagram_error(&e) => {
                        debug!("ignoring UDP receive error: {e:#}");
                        continue;
                    }
                    Err(e) => return Err(Error::Recv(e)),
                };
                match bprotocol::deserialize_message::<ByteBufOwned>(&buf[..size]) {
                    Ok(msg) => match output_tx.send((msg, addr)).await {
                        Ok(_) => {}
                        Err(_) => return Err(Error::DhtDead),
                    },
                    Err(e) => debug!("{}: error deserializing incoming message: {}", addr, e),
                }
            }
        };
        let result = tokio::select! {
            err = writer => err,
            err = reader => err,
        };
        result
    }

    async fn start(
        self,
        in_rx: UnboundedReceiver<WorkerSendRequest>,
        bootstrap_addrs: &[String],
    ) -> crate::Result<()> {
        let (out_tx, mut out_rx) = channel(1);
        let framer = self
            .framer(&self.socket, in_rx, out_tx)
            .instrument(debug_span!("dht_framer"));

        let bootstrap = self.bootstrap(bootstrap_addrs);
        let mut bootstrap_done = false;

        let response_reader = {
            let this = &self;
            async move {
                while let Some((response, addr)) = out_rx.recv().await {
                    if let Err(e) = this.dht.on_received_message(response, addr) {
                        debug!("error in on_response, addr={:?}: {}", addr, e)
                    }
                }
                Err(Error::DhtDead)
            }
        }
        .instrument(debug_span!("dht_response_reader"));

        let pinger_v4 = self.pinger(true).instrument(debug_span!("pinger_v4"));
        let bucket_refresher_v4 = self
            .bucket_refresher(true)
            .instrument(debug_span!("bucket_refresher_v4"));

        let pinger_v6 = self.pinger(false).instrument(debug_span!("pinger_v6"));
        let bucket_refresher_v6 = self
            .bucket_refresher(false)
            .instrument(debug_span!("bucket_refresher_v6"));

        // The initial bootstrap retries with backoff, but once it succeeds
        // nothing would refill a routing table that later drains (network
        // change, sleep, a burst of timeouts), leaving the DHT useless for
        // good. Run the bootstrap again whenever both tables are empty.
        let rebootstrap = async {
            let mut interval = tokio::time::interval(REBOOTSTRAP_CHECK_INTERVAL);
            interval.tick().await;
            loop {
                interval.tick().await;
                let empty = self.dht.routing_table_v4.read().len() == 0
                    && self.dht.routing_table_v6.read().len() == 0;
                if empty && !bootstrap_addrs.is_empty() {
                    warn!("DHT routing table is empty, bootstrapping again");
                    if let Err(e) = self.bootstrap(bootstrap_addrs).await {
                        warn!("error in repeated bootstrap: {e:#}");
                    }
                }
            }
        };

        tokio::pin!(framer);
        tokio::pin!(rebootstrap);
        tokio::pin!(bootstrap);
        tokio::pin!(response_reader);
        tokio::pin!(pinger_v4);
        tokio::pin!(bucket_refresher_v4);
        tokio::pin!(pinger_v6);
        tokio::pin!(bucket_refresher_v6);

        loop {
            tokio::select! {
                err = &mut framer => {
                    return Error::task_finished(&"framer", err);
                },
                _ = &mut rebootstrap, if bootstrap_done => {},
                result = &mut bootstrap, if !bootstrap_done => {
                    bootstrap_done = true;
                    result?;
                },
                err = &mut pinger_v4 => {
                    return Error::task_finished(&"pinger_v4", err);
                },
                err = &mut bucket_refresher_v4 => {
                    return Error::task_finished(&"bucket_refresher_v4", err);
                },
                err = &mut pinger_v6 => {
                    return Error::task_finished(&"pinger_v6", err);
                },
                err = &mut bucket_refresher_v6 => {
                    return Error::task_finished(&"bucket_refresher_v6", err);
                },
                err = &mut response_reader => {
                    return Error::task_finished(&"response_reader", err);
                }
            }
        }
    }
}

#[derive(Default)]
pub struct DhtConfig<'a> {
    pub peer_id: Option<Id20>,
    pub bootstrap_addrs: Option<Vec<String>>,
    pub routing_table: Option<RoutingTable>,
    pub routing_table_v6: Option<RoutingTable>,
    pub listen_addr: Option<SocketAddr>,
    pub peer_store: Option<PeerStore>,
    pub cancellation_token: Option<CancellationToken>,
    pub bind_device: Option<&'a BindDevice>,
}

impl DhtState {
    pub async fn new() -> crate::Result<Arc<Self>> {
        Self::with_config(DhtConfig::default()).await
    }
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    #[inline(never)]
    pub fn with_config<'a>(mut config: DhtConfig<'a>) -> BoxFuture<'a, crate::Result<Arc<Self>>> {
        async move {
            let addr = config
                .listen_addr
                .unwrap_or((Ipv6Addr::UNSPECIFIED, 0).into());
            let socket = UdpSocket::bind_udp(
                addr,
                librqbit_dualstack_sockets::BindOpts {
                    request_dualstack: true,
                    reuseport: false,
                    device: config.bind_device,
                },
            )
            .map_err(|e| Error::Bind(Box::new(e)))?;

            let listen_addr = socket.bind_addr();
            info!("DHT listening on {:?}", listen_addr);

            let peer_id = config
                .peer_id
                .unwrap_or_else(|| generate_azereus_style(*b"rQ", crate_version!()));
            info!("starting up DHT with peer id {:?}", peer_id);
            let bootstrap_addrs = config
                .bootstrap_addrs
                .unwrap_or_else(|| crate::DHT_BOOTSTRAP.iter().map(|v| v.to_string()).collect());

            let token = config.cancellation_token.take().unwrap_or_default();

            let (in_tx, in_rx) = unbounded_channel();
            let state = Arc::new(Self::new_internal(
                peer_id,
                in_tx,
                config.routing_table,
                config.routing_table_v6,
                listen_addr,
                config.peer_store.unwrap_or_else(|| PeerStore::new(peer_id)),
                token,
            ));

            spawn_with_cancel(
                debug_span!("dht"),
                "dht",
                state.cancellation_token.clone(),
                {
                    let state = state.clone();
                    async move {
                        let worker = DhtWorker { socket, dht: state };
                        worker.start(in_rx, &bootstrap_addrs).await
                    }
                },
            );
            Ok(state)
        }
        .boxed()
    }

    pub fn get_peers(
        self: &Arc<Self>,
        info_hash: Id20,
        announce_port: Option<u16>,
    ) -> RequestPeersStream {
        self.get_peers_paced(
            info_hash,
            announce_port,
            LookupPacer::fixed(REQUERY_INTERVAL),
        )
    }

    /// Like [`Self::get_peers`], with the time between lookup rounds chosen
    /// by `pacer`. `announce_port` is announced at most every
    /// [`crate::DHT_ANNOUNCE_INTERVAL`] (and on the first round).
    pub fn get_peers_paced(
        self: &Arc<Self>,
        info_hash: Id20,
        announce_port: Option<u16>,
        pacer: Arc<LookupPacer>,
    ) -> RequestPeersStream {
        RequestPeersStream::new(self.clone(), info_hash, announce_port, pacer)
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    pub fn stats(&self) -> DhtStats {
        self.get_stats()
    }

    pub fn with_routing_tables<R, F: FnOnce(&RoutingTable, &RoutingTable) -> R>(&self, f: F) -> R {
        f(&self.routing_table_v4.read(), &self.routing_table_v6.read())
    }

    // pub fn clone_routing_table(&self) -> RoutingTable {
    //     self.routing_table.read().clone()
    // }
}

trait FromSocketAddr: Sized {
    fn from_socket_addr(addr: SocketAddr) -> Option<Self>;
}

impl FromSocketAddr for SocketAddrV4 {
    fn from_socket_addr(addr: SocketAddr) -> Option<Self> {
        match addr {
            SocketAddr::V4(a) => Some(a),
            _ => None,
        }
    }
}

impl FromSocketAddr for SocketAddrV6 {
    fn from_socket_addr(addr: SocketAddr) -> Option<Self> {
        match addr {
            SocketAddr::V6(a) => Some(a),
            _ => None,
        }
    }
}

/// Whether a node or peer address received from the DHT node `from` is
/// worth contacting. Remote nodes are untrusted: they must not make us send
/// to special addresses (unspecified, multicast, broadcast, port 0) or,
/// unless they are on that network themselves, to loopback / LAN /
/// link-local addresses.
fn acceptable_remote_addr(a: SocketAddr, from: SocketAddr) -> bool {
    fn class(ip: std::net::IpAddr) -> u8 {
        match ip.to_canonical() {
            std::net::IpAddr::V4(v4) if v4.is_loopback() => 2,
            std::net::IpAddr::V4(v4) if v4.is_private() || v4.is_link_local() => 1,
            std::net::IpAddr::V6(v6) if v6.is_loopback() => 2,
            std::net::IpAddr::V6(v6) if v6.is_unique_local() || v6.is_unicast_link_local() => 1,
            _ => 0,
        }
    }
    let ip = a.ip().to_canonical();
    let special = match ip {
        std::net::IpAddr::V4(v4) => v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast(),
        std::net::IpAddr::V6(v6) => v6.is_unspecified() || v6.is_multicast(),
    };
    a.port() != 0 && !special && class(ip) <= class(from.ip())
}

#[cfg(test)]
mod tests {
    use std::{
        net::{Ipv4Addr, SocketAddr},
        time::Duration,
    };

    use bencode::ByteBufOwned;
    use tokio::net::UdpSocket;

    use super::{DhtConfig, DhtState, Id20};
    use crate::routing_table::RoutingTable;
    use crate::{
        Dht,
        bprotocol::{self, MessageKind},
    };

    async fn start_dht(bootstrap_addr: SocketAddr) -> Dht {
        DhtState::with_config(DhtConfig {
            listen_addr: Some((Ipv4Addr::LOCALHOST, 0).into()),
            bootstrap_addrs: Some(vec![bootstrap_addr.to_string()]),
            ..Default::default()
        })
        .await
        .unwrap()
    }

    async fn wait_for_nodes(dht: &Dht, min: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while dht.stats().routing_table_size < min {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("routing table stayed below {min}: {:?}", dht.stats()));
    }

    // A node that is bootstrapped from a single host that knows nobody still keeps that host.
    #[tokio::test]
    async fn test_bootstrap_host_joins_routing_table() {
        let seed = DhtState::with_config(DhtConfig {
            listen_addr: Some((Ipv4Addr::LOCALHOST, 0).into()),
            bootstrap_addrs: Some(Vec::new()),
            ..Default::default()
        })
        .await
        .unwrap();
        let node = start_dht(seed.listen_addr()).await;
        wait_for_nodes(&node, 1).await;
        assert_answers_ping(&node).await;
    }

    // Nodes learned from the bootstrap host are contacted and kept as well.
    #[tokio::test]
    async fn test_loopback_nodes_bootstrap_from_seed() {
        let mut leaves = Vec::new();
        let mut table = RoutingTable::new(Id20::new([0x55; 20]), None);
        for i in 0..5u8 {
            let leaf = DhtState::with_config(DhtConfig {
                listen_addr: Some((Ipv4Addr::LOCALHOST, 0).into()),
                bootstrap_addrs: Some(Vec::new()),
                ..Default::default()
            })
            .await
            .unwrap();
            let mut id = [i; 20];
            id[0] = 0x80 | i;
            table.add_node(Id20::new(id), leaf.listen_addr());
            leaves.push(leaf);
        }
        let seed = DhtState::with_config(DhtConfig {
            listen_addr: Some((Ipv4Addr::LOCALHOST, 0).into()),
            bootstrap_addrs: Some(Vec::new()),
            routing_table: Some(table),
            ..Default::default()
        })
        .await
        .unwrap();
        let node = start_dht(seed.listen_addr()).await;
        wait_for_nodes(&node, 6).await;
    }

    async fn assert_answers_ping(dht: &Dht) {
        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        client
            .send_to(
                b"d1:ad2:id20:abcdefghij0123456789e1:q4:ping1:t2:aa1:y1:qe",
                dht.listen_addr(),
            )
            .await
            .unwrap();
        let mut buf = [0u8; 1024];
        let len = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match client.recv_from(&mut buf).await {
                    Ok((len, addr)) if addr == dht.listen_addr() => return len,
                    _ => continue,
                }
            }
        })
        .await
        .expect("the DHT stopped answering");
        let response = bprotocol::deserialize_message::<ByteBufOwned>(&buf[..len]).unwrap();
        assert!(matches!(response.kind, MessageKind::Response(_)));
    }

    // On Windows, an ICMP port unreachable that comes back for an earlier send_to makes the next
    // recv_from on the same UDP socket fail with WSAECONNRESET (os error 10054).
    #[tokio::test]
    async fn test_dht_answers_after_icmp_port_unreachable() {
        // Nothing listens on this port once the socket is dropped.
        let closed = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap();
        let dht = start_dht(closed).await;

        // Let the bootstrap query reach the closed port and the ICMP error come back.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_answers_ping(&dht).await;
    }

    // On Windows, a datagram longer than the receive buffer makes recv_from fail with
    // WSAEMSGSIZE (os error 10040).
    #[tokio::test]
    async fn test_dht_answers_after_oversized_datagram() {
        // Stays open, so the bootstrap query gets neither an answer nor an ICMP error.
        let silent = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let dht = start_dht(silent.local_addr().unwrap()).await;

        // Some platforms refuse to send a datagram this large, which is fine.
        let _ = silent.send_to(&[0u8; 20_000], dht.listen_addr()).await;
        assert_answers_ping(&dht).await;
    }

    /// Queries from one source are answered at most 25 times a second, so
    /// spoofed queries cannot turn us into a reflector against that address.
    /// (Loopback is exempt, so this runs against the limiter directly.)
    #[test]
    fn test_queries_from_one_source_are_rate_limited() {
        let mut l = crate::query_limit::QueryLimiter::new(std::time::Instant::now());
        let now = std::time::Instant::now();
        let answered = (0..100)
            .filter(|_| l.allow("198.51.100.1".parse().unwrap(), now))
            .count();
        assert_eq!(
            answered,
            crate::query_limit::MAX_QUERIES_PER_SOURCE as usize
        );
    }

    #[tokio::test]
    async fn test_loopback_queries_are_not_rate_limited() {
        let seed = DhtState::with_config(DhtConfig {
            listen_addr: Some((Ipv4Addr::LOCALHOST, 0).into()),
            bootstrap_addrs: Some(Vec::new()),
            ..Default::default()
        })
        .await
        .unwrap();
        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        for _ in 0..100 {
            client
                .send_to(
                    b"d1:ad2:id20:abcdefghij0123456789e1:q4:ping1:t2:aa1:y1:qe",
                    seed.listen_addr(),
                )
                .await
                .unwrap();
        }
        let mut answers = 0;
        let mut buf = [0u8; 1024];
        while let Ok(Ok(_)) =
            tokio::time::timeout(Duration::from_millis(300), client.recv_from(&mut buf)).await
        {
            answers += 1;
        }
        assert_eq!(answers, 100);
    }

    /// Hostile nodes answer every query with hundreds of fresh, ever closer
    /// node ids, all pointing back at themselves. Each lookup must contact
    /// each address once and stay within its request budget instead of
    /// fanning out without bound (memory and our query rate limit).
    #[tokio::test]
    async fn test_lookup_fan_out_from_hostile_nodes_is_bounded() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        const SOCKETS: usize = 20;
        let mut socks = Vec::new();
        for _ in 0..SOCKETS {
            socks.push(Arc::new(
                UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap(),
            ));
        }
        let addrs: Vec<SocketAddr> = socks.iter().map(|s| s.local_addr().unwrap()).collect();
        let queries = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for sock in socks {
            let addrs = addrs.clone();
            let queries = queries.clone();
            tasks.push(tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                loop {
                    let Ok((len, from)) = sock.recv_from(&mut buf).await else {
                        continue;
                    };
                    let Ok(msg) = bprotocol::deserialize_message::<ByteBufOwned>(&buf[..len])
                    else {
                        continue;
                    };
                    if matches!(msg.kind, MessageKind::Response(_) | MessageKind::Error(_)) {
                        continue;
                    }
                    queries.fetch_add(1, Ordering::Relaxed);
                    // 300 nodes with random ids (macOS caps datagrams at 9 KiB), all on the hostile sockets.
                    let mut nodes = Vec::with_capacity(300 * 26);
                    for i in 0..300 {
                        nodes.extend_from_slice(&rand::random::<[u8; 20]>());
                        let a = addrs[i % addrs.len()];
                        nodes.extend_from_slice(&[127, 0, 0, 1]);
                        nodes.extend_from_slice(&a.port().to_be_bytes());
                    }
                    let tid = msg.transaction_id.as_ref();
                    let mut out = Vec::new();
                    out.extend_from_slice(b"d1:rd2:id20:");
                    out.extend_from_slice(&rand::random::<[u8; 20]>());
                    out.extend_from_slice(format!("5:nodes{}:", nodes.len()).as_bytes());
                    out.extend_from_slice(&nodes);
                    out.extend_from_slice(format!("e1:t{}:", tid.len()).as_bytes());
                    out.extend_from_slice(tid);
                    out.extend_from_slice(b"1:y1:re");
                    let _ = sock.send_to(&out, from).await;
                }
            }));
        }

        let dht = start_dht(addrs[0]).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let n = queries.load(Ordering::Relaxed);
        for t in tasks {
            t.abort();
        }
        drop(dht);
        // Without the bounds this is only held back by the query rate cap;
        // with them, each bootstrap lookup asks each of the 20 addresses at
        // most once.
        assert!(n <= 20 * 8, "{n} queries sent to hostile nodes");
    }

    /// All DHT queries share one budget: after a burst of
    /// `QUERY_BURST_SECONDS` worth, they go out at the configured rate, in
    /// order, and every waiting query is sent eventually (none is dropped).
    #[tokio::test(start_paused = true)]
    async fn test_query_rate_is_capped() {
        use super::{DEFAULT_QUERIES_PER_SECOND as QPS, QUERY_BURST_SECONDS};
        use std::sync::Arc;

        let limiter = Arc::new(super::make_rate_limiter_with(QPS));
        let start = tokio::time::Instant::now();
        // 40 lookups with 8 requests in flight each, all wanting to send.
        const WAITERS: usize = 320;
        let mut tasks = Vec::new();
        for _ in 0..WAITERS {
            let limiter = limiter.clone();
            tasks.push(tokio::spawn(async move {
                limiter.acquire_one().await;
                start.elapsed()
            }));
        }
        let mut sent = Vec::new();
        for t in tasks {
            sent.push(t.await.unwrap());
        }
        sent.sort();
        let burst = QPS * QUERY_BURST_SECONDS;
        let in_window =
            |from: Duration, to: Duration| sent.iter().filter(|t| **t >= from && **t < to).count();
        assert!(in_window(Duration::ZERO, Duration::from_secs(1)) <= burst + QPS);
        for s in 1..8 {
            let n = in_window(Duration::from_secs(s), Duration::from_secs(s + 1));
            assert!(n <= QPS + 1, "{n} queries in second {s}");
        }
        let expected = Duration::from_secs_f64((WAITERS - burst) as f64 / QPS as f64);
        let last = *sent.last().unwrap();
        assert!(
            last >= expected - Duration::from_millis(100)
                && last <= expected + Duration::from_secs(1),
            "last of {WAITERS} queries sent after {last:?}, expected about {expected:?}"
        );
    }

    /// A simulated network of `SIM_NODES` loopback nodes with Kademlia
    /// routing tables (up to 8 nodes per bucket). All ids share their first
    /// 16 bits with `SIM_INFO_HASH` (so they are close enough to announce
    /// to), and the 8 closest nodes to it have a peer stored for it.
    mod sim {
        use std::{
            collections::HashMap,
            net::{Ipv4Addr, SocketAddr, SocketAddrV4},
            sync::Arc,
        };

        use bencode::ByteBufOwned;
        use librqbit_core::compact_ip::{Compact, CompactListInBuffer};
        use parking_lot::Mutex;
        use tokio::net::UdpSocket;

        use crate::{
            Id20,
            bprotocol::{self, Message, MessageKind, Node, Response},
        };

        pub const SIM_NODES: usize = 1000;
        pub const K: usize = 8;

        pub fn sim_info_hash() -> Id20 {
            let mut h = [0x5a; 20];
            h[0] = 0xab;
            h[1] = 0xcd;
            Id20::new(h)
        }

        #[derive(Default)]
        pub struct Counters {
            /// get_peers queries for the info hash, per node index.
            pub get_peers: HashMap<usize, usize>,
            /// Nodes that received an announce_peer with their own token.
            pub announced: Vec<usize>,
            pub bad_tokens: usize,
            pub other_queries: usize,
        }

        pub struct SimNetwork {
            pub ids: Vec<Id20>,
            pub addrs: Vec<SocketAddr>,
            pub counters: Arc<Mutex<Counters>>,
            tasks: Vec<tokio::task::JoinHandle<()>>,
        }

        impl Drop for SimNetwork {
            fn drop(&mut self) {
                for t in &self.tasks {
                    t.abort();
                }
            }
        }

        fn splitmix(state: &mut u64) -> u64 {
            *state = state.wrapping_add(0x9e3779b97f4a7c15);
            let mut z = *state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
            z ^ (z >> 31)
        }

        fn leading_zeros(d: &Id20) -> usize {
            let mut n = 0;
            for b in d.0 {
                if b == 0 {
                    n += 8;
                } else {
                    return n + b.leading_zeros() as usize;
                }
            }
            n
        }

        pub fn peer_of(node: usize) -> SocketAddr {
            (Ipv4Addr::LOCALHOST, 20_000 + node as u16).into()
        }

        impl SimNetwork {
            /// The `K` nodes closest to the info hash.
            pub fn closest(&self) -> Vec<usize> {
                let h = sim_info_hash();
                let mut idx: Vec<usize> = (0..self.ids.len()).collect();
                idx.sort_by_key(|&i| self.ids[i].distance(&h));
                idx.truncate(K);
                idx
            }

            pub async fn start() -> Self {
                let _ = rlimit::increase_nofile_limit(4096);
                let mut rng = 0x1234_5678_u64;
                let h = sim_info_hash();
                let mut ids = Vec::new();
                for _ in 0..SIM_NODES {
                    let mut id = [0u8; 20];
                    for chunk in id.chunks_mut(8) {
                        let r = splitmix(&mut rng).to_be_bytes();
                        chunk.copy_from_slice(&r[..chunk.len()]);
                    }
                    id[0] = h.0[0];
                    id[1] = h.0[1];
                    ids.push(Id20::new(id));
                }
                let mut socks = Vec::new();
                for _ in 0..SIM_NODES {
                    socks.push(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
                }
                let addrs: Vec<SocketAddr> =
                    socks.iter().map(|s| s.local_addr().unwrap()).collect();
                // Kademlia tables: up to K nodes per bucket (prefix length
                // shared with the owner), picked in a shuffled order.
                let mut tables = Vec::new();
                for i in 0..SIM_NODES {
                    let mut order: Vec<usize> = (0..SIM_NODES).filter(|&j| j != i).collect();
                    for k in (1..order.len()).rev() {
                        let r = (splitmix(&mut rng) % (k as u64 + 1)) as usize;
                        order.swap(k, r);
                    }
                    let mut per_bucket: HashMap<usize, usize> = HashMap::new();
                    let mut table = Vec::new();
                    for j in order {
                        let b = leading_zeros(&ids[i].distance(&ids[j]));
                        let c = per_bucket.entry(b).or_default();
                        if *c < K {
                            *c += 1;
                            table.push(j);
                        }
                    }
                    tables.push(table);
                }
                let ids_arc = Arc::new(ids.clone());
                let addrs_arc = Arc::new(addrs.clone());
                let mut by_dist: Vec<usize> = (0..SIM_NODES).collect();
                by_dist.sort_by_key(|&i| ids[i].distance(&h));
                let holders: Arc<Vec<usize>> = Arc::new(by_dist[..K].to_vec());
                let counters = Arc::new(Mutex::new(Counters::default()));
                let mut tasks = Vec::new();
                for (i, (sock, table)) in socks.into_iter().zip(tables).enumerate() {
                    let ids = ids_arc.clone();
                    let addrs = addrs_arc.clone();
                    let holders = holders.clone();
                    let counters = counters.clone();
                    tasks.push(tokio::spawn(async move {
                        let token = format!("tok{i}");
                        let mut buf = vec![0u8; 4096];
                        let mut out = Vec::new();
                        loop {
                            let Ok((len, from)) = sock.recv_from(&mut buf).await else {
                                continue;
                            };
                            let Ok(msg) =
                                bprotocol::deserialize_message::<ByteBufOwned>(&buf[..len])
                            else {
                                continue;
                            };
                            let target = match &msg.kind {
                                MessageKind::GetPeersRequest(r) => {
                                    if r.info_hash == h {
                                        *counters.lock().get_peers.entry(i).or_default() += 1;
                                    }
                                    Some(r.info_hash)
                                }
                                MessageKind::FindNodeRequest(r) => {
                                    counters.lock().other_queries += 1;
                                    Some(r.target)
                                }
                                MessageKind::PingRequest(_) => {
                                    counters.lock().other_queries += 1;
                                    None
                                }
                                MessageKind::AnnouncePeer(a) => {
                                    let mut c = counters.lock();
                                    if a.info_hash == h && a.token.as_ref() == token.as_bytes() {
                                        c.announced.push(i);
                                    } else {
                                        c.bad_tokens += 1;
                                    }
                                    None
                                }
                                _ => continue,
                            };
                            let is_get_peers = matches!(msg.kind, MessageKind::GetPeersRequest(_));
                            let nodes = target.map(|t| {
                                let mut near = table.clone();
                                near.sort_by_key(|&j| ids[j].distance(&t));
                                CompactListInBuffer::new_from_iter(near.into_iter().take(K).map(
                                    |j| {
                                        let SocketAddr::V4(a) = addrs[j] else {
                                            unreachable!()
                                        };
                                        Node::<SocketAddrV4> {
                                            id: ids[j],
                                            addr: a,
                                        }
                                    },
                                ))
                            });
                            let values = (is_get_peers && holders.contains(&i))
                                .then(|| vec![Compact(peer_of(i))]);
                            let response = Response {
                                id: ids[i],
                                nodes,
                                nodes6: None,
                                values,
                                token: is_get_peers.then(|| ByteBufOwned::from(token.as_bytes())),
                            };
                            let reply = Message {
                                transaction_id: msg.transaction_id,
                                version: None,
                                ip: None,
                                kind: MessageKind::Response(response),
                            };
                            out.clear();
                            bprotocol::serialize_message(
                                &mut out,
                                reply.transaction_id,
                                reply.version,
                                reply.ip,
                                reply.kind,
                            )
                            .unwrap();
                            let _ = sock.send_to(&out, from).await;
                        }
                    }));
                }
                Self {
                    ids,
                    addrs,
                    counters,
                    tasks,
                }
            }
        }
    }

    /// One get_peers round in a simulated network of a thousand nodes: it
    /// reaches the nodes closest to the info hash (gets the peers they store
    /// and announces to them) while querying a few dozen nodes at most, like
    /// libtorrent's traversal. (Before rounds converged, this one asked 127
    /// nodes, and announced to all of them; 59 with 300 nodes. In the real
    /// network it used up the whole budget of 256.)
    #[tokio::test]
    async fn test_lookup_converges_in_simulated_network() {
        use futures::StreamExt;
        use std::collections::HashSet;

        let net = sim::SimNetwork::start().await;
        // Our id is far from the cluster, so the lookup starts from the few
        // nodes of one bucket and has to walk towards the info hash.
        let mut own = [0x11u8; 20];
        own[0] = 0x2b;
        let dht = DhtState::with_config(DhtConfig {
            peer_id: Some(Id20::new(own)),
            listen_addr: Some((Ipv4Addr::LOCALHOST, 0).into()),
            bootstrap_addrs: Some(vec![net.addrs[0].to_string()]),
            ..Default::default()
        })
        .await
        .unwrap();
        wait_for_nodes(&dht, 8).await;
        // Let the bootstrap lookup finish.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let closest = net.closest();
        let expected_peers: HashSet<SocketAddr> =
            closest.iter().map(|&i| sim::peer_of(i)).collect();
        let mut stream = dht.get_peers_paced(
            sim::sim_info_hash(),
            Some(6881),
            crate::LookupPacer::fixed(Duration::from_secs(3600)),
        );
        let mut found = HashSet::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(p) = stream.next().await {
                found.insert(p);
                if found.len() == expected_peers.len() {
                    break;
                }
            }
        })
        .await;
        // Let the round finish (and its announces arrive).
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let c = net.counters.lock();
        let distinct = c.get_peers.len();
        let total: usize = c.get_peers.values().sum();
        let announced: HashSet<usize> = c.announced.iter().copied().collect();
        eprintln!(
            "get_peers round: {distinct} distinct nodes, {total} queries, {} peers found, announced to {:?}, closest {:?}",
            found.len(),
            announced,
            closest
        );
        assert_eq!(found, expected_peers, "peers stored at the closest nodes");
        assert!(distinct <= 60, "{distinct} nodes queried in one round");
        assert_eq!(total, distinct, "a node is asked once per round");
        assert_eq!(c.bad_tokens, 0);
        assert_eq!(
            announced,
            closest.iter().copied().collect::<HashSet<_>>(),
            "announced to the closest nodes that returned a token"
        );
        assert_eq!(c.announced.len(), sim::K, "one announce per node");
        drop(c);
        drop(stream);
    }
}
