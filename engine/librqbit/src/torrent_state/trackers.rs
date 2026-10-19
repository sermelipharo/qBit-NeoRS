//! Per-torrent tracker manager.
//!
//! [`TorrentTrackers`] owns the torrent's mutable, tiered tracker list (it
//! lives in [`ManagedTorrentShared`] and so survives pause/unpause) and runs
//! an announcer while the torrent is started:
//! * the announcer is spawned lazily, when the torrent's peer stream is first
//!   polled (i.e. when the torrent goes live or starts resolving a magnet);
//! * it plans announces with [`TrackerList`] (tiers, intervals, backoff),
//!   sends `started` / `completed` / regular announces, scrapes when needed
//!   and forwards returned peers tagged [`PeerSource::Tracker`];
//! * when the peer stream is dropped (pause, delete, error) it sends `stopped`
//!   to the trackers it announced to, bounded by `stop_tracker_timeout`, and
//!   resets statuses to "not contacted";
//! * a new run waits for the previous run's `stopped` announces to finish.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime};

use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use librqbit_core::hash_id::Id20;
use librqbit_core::spawn_utils::spawn_with_cancel;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, debug_span, trace};
use tracker_comms::{
    AddressFamily, AnnounceEvent, AnnounceOutcome, AnnounceRequest, CallOptions, PlannedAnnounce, PlannedScrape,
    ScrapeData, TrackerEditError, TrackerError, TrackerId, TrackerList, TrackerOrigin,
    TrackerStatus, redact_url,
};

use crate::peer_source::{
    DisabledDiscovery, PeerSource, PeerSourceCounts, PeerSourceRegistry, PeerSources,
    SourcedPeerStream,
};
use crate::session_trackers::SessionTrackers;
use crate::torrent_state::live::peer::stats::snapshot::{PeerStatsFilter, PeerStatsFilterState};
use crate::torrent_state::{ManagedTorrent, ManagedTorrentShared, ManagedTorrentState};
use crate::type_aliases::PeerStream;

use super::TorrentStateLive;

/// libtorrent reports this much left when the size is unknown (magnet without
/// metadata), so the tracker does not take us for a seed.
const UNKNOWN_LEFT: u64 = 16 * 1024;
const MIN_LOOP_SLEEP: Duration = Duration::from_millis(500);

/// A torrent's trackers and announce state.
pub struct TorrentTrackers {
    info_hash: Id20,
    /// `info_hash` is a truncated v2 hash (see `set_announces_v2`).
    v2_hash: AtomicBool,
    key: u32,
    list: Mutex<TrackerList>,
    wake: Notify,
    private: AtomicBool,
    private_notify: Notify,
    /// Per-torrent DHT/PeX/LSD switches; the DHT and LSD streams watch it.
    disabled_discovery: tokio::sync::watch::Sender<DisabledDiscovery>,
    /// The torrent came from a local source: its trackers are trusted for
    /// SSRF purposes.
    local_source: bool,
    /// Never announce (per-torrent `disable_trackers`).
    disabled: bool,
    force_interval: Option<Duration>,
    torrent: OnceLock<Weak<ManagedTorrent>>,
    run_lock: tokio::sync::Mutex<()>,
    running: AtomicBool,
    pub(crate) peer_sources: PeerSourceRegistry,
}

impl std::fmt::Debug for TorrentTrackers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TorrentTrackers")
            .field("info_hash", &self.info_hash)
            .field("trackers", &self.list.lock().len())
            .field("running", &self.running.load(Ordering::Relaxed))
            .finish()
    }
}

#[derive(Clone, Copy, Debug)]
struct AnnounceStats {
    uploaded: u64,
    downloaded: u64,
    left: u64,
    known: bool,
}

impl AnnounceStats {
    fn is_seed(&self) -> bool {
        self.known && self.left == 0
    }
}

enum JobResult {
    Announce {
        id: TrackerId,
        url: String,
        event: AnnounceEvent,
        outcome: AnnounceOutcome,
        /// The main hash is a (truncated) v2 hash.
        v2: bool,
        /// The same announce under the v2 hash of a hybrid torrent.
        alt: Option<AnnounceOutcome>,
    },
    Scrape {
        id: TrackerId,
        result: Result<ScrapeData, TrackerError>,
    },
}

type Job = BoxFuture<'static, JobResult>;

impl TorrentTrackers {
    pub(crate) fn new(
        info_hash: Id20,
        list: TrackerList,
        private: bool,
        local_source: bool,
        disabled: bool,
        force_interval: Option<Duration>,
    ) -> Arc<Self> {
        Arc::new(Self {
            info_hash,
            v2_hash: AtomicBool::new(false),
            key: rand::random(),
            list: Mutex::new(list),
            wake: Notify::new(),
            private: AtomicBool::new(private),
            private_notify: Notify::new(),
            disabled_discovery: tokio::sync::watch::Sender::new(DisabledDiscovery::default()),
            local_source,
            disabled,
            force_interval,
            torrent: OnceLock::new(),
            run_lock: tokio::sync::Mutex::new(()),
            running: AtomicBool::new(false),
            peer_sources: Default::default(),
        })
    }

    pub(crate) fn set_torrent(&self, torrent: &Arc<ManagedTorrent>) {
        let _ = self.torrent.set(Arc::downgrade(torrent));
    }

    fn torrent(&self) -> Option<Arc<ManagedTorrent>> {
        self.torrent.get().and_then(Weak::upgrade)
    }

    /// The hash this list was created with is a truncated v2 hash (a `btmh`
    /// magnet): peers found under it are v2 peers.
    pub(crate) fn set_announces_v2(&self) {
        self.v2_hash.store(true, Ordering::Release);
    }

    /// `h` is a (truncated) v2 hash of this torrent, as far as discovery
    /// before the torrent exists is concerned.
    pub(crate) fn is_v2_hash(&self, h: Id20) -> bool {
        h == self.info_hash && self.v2_hash.load(Ordering::Acquire)
    }

    /// Hashes to announce: (main hash, v2 hash of a hybrid torrent) once
    /// the torrent exists (a hybrid runs on its v1 hash even if it was added
    /// by a `btmh` magnet), the hash it was added with before.
    fn announce_hashes(&self) -> (Id20, Option<Id20>) {
        match self.torrent() {
            Some(t) => (t.info_hash(), t.info_hash_alt()),
            None => (self.info_hash, None),
        }
    }

    pub fn is_private(&self) -> bool {
        self.private.load(Ordering::Acquire)
    }

    /// Metadata arrived and says the torrent is private: drop session-wide
    /// extra trackers and stop DHT/LSD discovery.
    pub(crate) fn set_private(&self) {
        self.private.store(true, Ordering::Release);
        self.private_notify.notify_waiters();
        self.with_list(|l| l.remove_origin(TrackerOrigin::Session));
    }

    /// Peer discovery switched off for this torrent.
    pub fn disabled_discovery(&self) -> DisabledDiscovery {
        *self.disabled_discovery.borrow()
    }

    /// Switch peer discovery off or on for this torrent. Running DHT/LSD
    /// streams stop or restart at once; PeX follows on the next message.
    /// Returns true if anything changed.
    pub(crate) fn set_disabled_discovery(&self, d: DisabledDiscovery) -> bool {
        self.disabled_discovery.send_if_modified(|cur| {
            let changed = *cur != d;
            *cur = d;
            changed
        })
    }

    pub(crate) fn watch_disabled_discovery(
        &self,
    ) -> tokio::sync::watch::Receiver<DisabledDiscovery> {
        self.disabled_discovery.subscribe()
    }

    /// Resolves once the torrent is known to be private.
    pub(crate) async fn wait_private(self: Arc<Self>) {
        loop {
            let notified = self.private_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_private() {
                return;
            }
            notified.await;
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// Whether the torrent came from a local source (its trackers are trusted
    /// by SSRF mitigation).
    pub fn is_local_source(&self) -> bool {
        self.local_source
    }

    /// Mutate the list and wake the announcer.
    fn with_list<R>(&self, f: impl FnOnce(&mut TrackerList) -> R) -> R {
        let r = f(&mut self.list.lock());
        self.wake.notify_one();
        r
    }

    pub fn statuses(&self) -> Vec<TrackerStatus> {
        self.list.lock().statuses(Instant::now())
    }

    pub fn entries(&self) -> Vec<(String, u32)> {
        self.list.lock().entries()
    }

    pub fn len(&self) -> usize {
        self.list.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.lock().is_empty()
    }

    pub fn add(&self, entries: Vec<(String, u32)>) -> usize {
        self.with_list(|l| {
            entries
                .iter()
                .filter(|(url, tier)| l.add(url, *tier, TrackerOrigin::User, true))
                .count()
        })
    }

    pub fn remove(&self, urls: &[String]) -> usize {
        self.with_list(|l| l.remove(urls))
    }

    pub fn edit(&self, old: &str, new: &str) -> Result<(), TrackerEditError> {
        self.with_list(|l| l.edit(old, new, true))
    }

    pub fn replace(&self, entries: Vec<(String, u32)>) {
        self.with_list(|l| l.replace(entries, TrackerOrigin::User, true))
    }

    /// Replaces the list; new URLs are trusted only if listed in
    /// `trusted_urls` (or the torrent is from a local source).
    pub fn replace_with_trust(&self, entries: Vec<(String, u32)>, trusted_urls: &[String]) {
        let local = self.local_source;
        self.with_list(|l| {
            l.replace_with(entries, TrackerOrigin::User, |url| {
                local || trusted_urls.iter().any(|t| t.trim() == url)
            })
        })
    }

    pub fn force_reannounce(&self, url: Option<&str>) -> bool {
        self.with_list(|l| l.force_reannounce(url))
    }

    pub fn swarm_counts(&self) -> (Option<u32>, Option<u32>) {
        self.list.lock().swarm_counts()
    }

    /// Wake the announcer (settings changed, completion, ...).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// The tracker announce stream for one run of the torrent. The announcer
    /// starts when the stream is first polled and stops (sending `stopped`)
    /// when it is dropped.
    pub(crate) fn start(self: &Arc<Self>, ctx: Arc<SessionTrackers>) -> SourcedPeerStream {
        let this = self.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut tx = Some(tx);
        async_stream::stream! {
            if let Some(tx) = tx.take() {
                let span = debug_span!("trackers", info_hash = ?this.info_hash);
                spawn_with_cancel::<std::convert::Infallible>(
                    span,
                    "tracker_announcer",
                    ctx.cancel.clone(),
                    run_announcer(this.clone(), ctx.clone(), tx).map(Ok),
                );
            }
            while let Some(item) = rx.recv().await {
                yield item;
            }
        }
        .boxed()
    }
}

/// Record the source of every peer and strip it (the live torrent consumes
/// plain addresses). This is the boundary where sourced streams meet the
/// existing `PeerStream` type.
pub(crate) fn record_and_strip(
    stream: SourcedPeerStream,
    trackers: Arc<TorrentTrackers>,
) -> PeerStream {
    stream
        .filter(|(addr, source)| {
            // DHT and LSD are clearnet: never accept synthetic I2P addresses
            // from them (trackers are filtered per URL, manual peers are the
            // user's choice).
            let refused = matches!(source, PeerSource::Dht | PeerSource::Lsd)
                && crate::i2p::is_synthetic_addr(*addr);
            std::future::ready(!refused)
        })
        .map(move |(addr, source)| {
            trackers.peer_sources.record(addr, source);
            addr
        })
        .boxed()
}

struct Runner {
    this: Arc<TorrentTrackers>,
    ctx: Arc<SessionTrackers>,
    tx: UnboundedSender<(SocketAddr, PeerSource)>,
    live: Option<Arc<TorrentStateLive>>,
    last_stats: AnnounceStats,
}

impl Runner {
    fn refresh_live(&mut self) {
        if self.live.is_none() {
            self.live = self.this.torrent().and_then(|t| t.live());
        }
    }

    fn stats(&mut self) -> AnnounceStats {
        if let Some(l) = &self.live {
            let total = l.info().lengths().total_length();
            let mut s = AnnounceStats {
                uploaded: l.get_uploaded_bytes(),
                downloaded: l.get_downloaded_bytes(),
                ..self.last_stats
            };
            // Once paused, the live state has no piece tracker any more: keep
            // the last known `left`.
            if let Some(hns) = l.get_hns() {
                s.left = total.saturating_sub(hns.have_bytes);
                s.known = true;
            }
            self.last_stats = s;
            return s;
        }
        if let Some(t) = self.this.torrent()
            && let Some(m) = t.metadata.load_full()
        {
            let total = m.lengths().total_length();
            let have = t.with_state(|s| match s {
                ManagedTorrentState::Paused(p) => Some(p.hns().have_bytes),
                ManagedTorrentState::Live(l) => l.get_hns().map(|h| h.have_bytes),
                _ => None,
            });
            self.last_stats = AnnounceStats {
                uploaded: 0,
                downloaded: 0,
                left: have.map(|h| total.saturating_sub(h)).unwrap_or(total),
                known: have.is_some(),
            };
        }
        self.last_stats
    }

    /// With I2P mixed mode off, an I2P torrent must not reveal our address
    /// to clearnet trackers (libtorrent skips them too).
    fn clearnet_tracker_refused(&self, url: &url::Url) -> bool {
        !url.host_str()
            .is_some_and(|h| h.to_ascii_lowercase().ends_with(".i2p"))
            && self
                .this
                .torrent()
                .and_then(|t| t.shared().session.upgrade())
                .is_some_and(|s| s.i2p.i2p_only(&self.this))
    }

    fn announce_job(&self, p: PlannedAnnounce, stats: AnnounceStats) -> Job {
        if self.clearnet_tracker_refused(&p.url) {
            let (id, url, event) = (p.id, redact_url(&p.url), p.event);
            return Box::pin(async move {
                JobResult::Announce {
                    id,
                    url,
                    event,
                    outcome: AnnounceOutcome::single(
                        AddressFamily::Any,
                        Err(TrackerError::skipped("I2P mixed mode is off")),
                    ),
                    v2: false,
                    alt: None,
                }
            });
        }
        let ctx = self.ctx.clone();
        let s = ctx.settings();
        let private = self.this.is_private();
        let stopping = p.event == AnnounceEvent::Stopped;
        let (info_hash, alt_hash) = self.this.announce_hashes();
        let v2 = self.this.is_v2_hash(info_hash);
        let req = AnnounceRequest {
            info_hash,
            peer_id: ctx.peer_id(),
            port: ctx.announce_port(&s),
            uploaded: stats.uploaded,
            downloaded: stats.downloaded,
            left: stats.left,
            event: p.event,
            key: self.this.key,
            num_want: Some(if stopping { 0 } else { s.num_want }),
            ip: ctx.announce_ip(&s),
            tracker_id: p.tracker_id.clone(),
        };
        let call = CallOptions {
            user_agent: ctx.user_agent(&s, private),
            timeout: if stopping {
                s.stop_tracker_timeout
            } else {
                s.announce_timeout
            },
            ipv4_only: ctx.ipv4_only,
        };
        // A hybrid torrent is announced under both hashes, as libtorrent
        // does (same event, same stats; the tracker id belongs to the main
        // announce). Both run in this job, concurrently.
        let alt_req = alt_hash.map(|h| AnnounceRequest {
            info_hash: h,
            tracker_id: None,
            ..req.clone()
        });
        Box::pin(async move {
            let url = redact_url(&p.url);
            trace!(tracker = %url, event = ?p.event, "announcing");
            let main = ctx.client.announce(&p.url, p.trusted, &req, &call);
            let (outcome, alt) = match &alt_req {
                Some(alt_req) => {
                    let other = ctx.client.announce(&p.url, p.trusted, alt_req, &call);
                    let (a, b) = futures::join!(main, other);
                    (a, Some(b))
                }
                None => (main.await, None),
            };
            JobResult::Announce {
                id: p.id,
                url,
                event: p.event,
                outcome,
                v2,
                alt,
            }
        })
    }

    fn scrape_job(&self, p: PlannedScrape) -> Job {
        if self.clearnet_tracker_refused(&p.url) {
            let id = p.id;
            return Box::pin(async move {
                JobResult::Scrape {
                    id,
                    result: Err(TrackerError::skipped("I2P mixed mode is off")),
                }
            });
        }
        let ctx = self.ctx.clone();
        let s = ctx.settings();
        let call = CallOptions {
            user_agent: ctx.user_agent(&s, self.this.is_private()),
            timeout: s.announce_timeout,
            ipv4_only: ctx.ipv4_only,
        };
        let info_hash = self.this.announce_hashes().0;
        Box::pin(async move {
            let result = ctx.client.scrape(&p.url, p.trusted, info_hash, &call).await;
            JobResult::Scrape { id: p.id, result }
        })
    }

    fn handle(&mut self, res: JobResult) {
        match res {
            JobResult::Announce {
                id,
                url,
                event,
                outcome,
                v2,
                alt,
            } => {
                let s = self.ctx.settings();
                let settings = self.ctx.announce_settings(&s, self.this.force_interval);
                // The tracker's status follows the main announce, or the v2
                // one when only that worked.
                let status = match &alt {
                    Some(a) if !outcome.is_success() && a.is_success() => a,
                    _ => &outcome,
                };
                let applied = self.this.list.lock().apply_announce(
                    id,
                    event,
                    status,
                    Instant::now(),
                    SystemTime::now(),
                    &settings,
                );
                for ep in outcome.endpoints.iter().chain(alt.iter().flat_map(|a| a.endpoints.iter())) {
                    match &ep.result {
                        Ok(r) => {
                            if let Some(ip) = r.external_ip {
                                self.ctx.record_observed_ip(&url, ip);
                            }
                            debug!(tracker = %url, family = ?ep.family, ?event, peers = r.peers.len(), "announce ok")
                        }
                        Err(e) => {
                            debug!(tracker = %url, family = ?ep.family, ?event, "announce failed: {e}")
                        }
                    }
                }
                if applied.is_some() {
                    // Only `.i2p` trackers may hand out (synthetic) I2P peers.
                    let i2p_tracker = url::Url::parse(&url)
                        .ok()
                        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
                        .is_some_and(|h| h.ends_with(".i2p"));
                    let peers = outcome
                        .peers()
                        .map(|p| (p, v2))
                        .chain(alt.iter().flat_map(|a| a.peers().map(|p| (p, true))));
                    for (peer, v2) in peers {
                        if !i2p_tracker && crate::i2p::is_synthetic_addr(peer) {
                            debug!(tracker = %url, "ignoring a discard-prefix peer from a clearnet tracker");
                            continue;
                        }
                        if v2 {
                            self.this.peer_sources.mark_v2(peer);
                        }
                        let _ = self.tx.send((peer, PeerSource::Tracker));
                    }
                }
            }
            JobResult::Scrape { id, result } => {
                self.this.list.lock().apply_scrape(id, result);
            }
        }
    }
}

async fn run_announcer(
    this: Arc<TorrentTrackers>,
    ctx: Arc<SessionTrackers>,
    tx: UnboundedSender<(SocketAddr, PeerSource)>,
) {
    struct RunningGuard<'a>(&'a AtomicBool);
    impl Drop for RunningGuard<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }

    let _active = ctx.active.enter();
    // Let a previous run finish its `stopped` announces first.
    let _run = this.run_lock.lock().await;
    if tx.is_closed() {
        return;
    }
    this.running.store(true, Ordering::Release);
    let _running = RunningGuard(&this.running);

    let mut r = Runner {
        this: this.clone(),
        ctx: ctx.clone(),
        tx,
        live: None,
        last_stats: AnnounceStats {
            uploaded: 0,
            downloaded: 0,
            left: UNKNOWN_LEFT,
            known: false,
        },
    };
    r.refresh_live();
    let first = r.stats();
    let mut seed = first.is_seed();
    let mut known = first.known;
    this.list.lock().reset_for_start(seed);

    let mut jobs: FuturesUnordered<Job> = FuturesUnordered::new();
    loop {
        r.refresh_live();
        let stats = r.stats();
        if stats.is_seed() && !seed {
            seed = true;
            if known {
                // Completed during this run: announce `completed`.
                this.list.lock().on_completed();
            } else {
                // Size was unknown (magnet): complete as soon as checked.
                this.list.lock().mark_complete_sent();
            }
        }
        known |= stats.known;
        let s = ctx.settings();
        let settings = ctx.announce_settings(&s, this.force_interval);
        let now = Instant::now();
        let (planned, scrapes, wakeup) = {
            let mut l = this.list.lock();
            let planned = l.plan(now, seed, &settings);
            let scrapes = l.plan_scrapes(now);
            (planned, scrapes, l.next_wakeup(now, &settings))
        };
        for p in planned {
            jobs.push(r.announce_job(p, stats));
        }
        for p in scrapes {
            jobs.push(r.scrape_job(p));
        }
        let deadline = wakeup
            .unwrap_or(now + Duration::from_secs(24 * 3600))
            .max(now + MIN_LOOP_SLEEP);

        // Completion wakes us up to send `completed`.
        let live = r.live.clone();
        let completed = async move {
            match live {
                Some(l) if l.get_hns().is_some_and(|h| !h.finished()) => {
                    l.wait_until_completed().await
                }
                _ => std::future::pending().await,
            }
        };

        tokio::select! {
            biased;
            _ = r.tx.closed() => break,
            Some(res) = jobs.next(), if !jobs.is_empty() => r.handle(res),
            _ = this.wake.notified() => {},
            _ = completed => {},
            _ = tokio::time::sleep_until(deadline.into()) => {},
        }
    }

    // Stopping: abandon in-flight requests, then tell trackers we left.
    drop(jobs);
    let stop_timeout = ctx.settings().stop_tracker_timeout;
    let stops = {
        let mut l = this.list.lock();
        l.cancel_updating();
        if stop_timeout.is_zero() {
            Vec::new()
        } else {
            l.plan_stop()
        }
    };
    if !stops.is_empty() {
        let stats = r.stats();
        debug!(count = stops.len(), ?stats, "sending stopped announces");
        let jobs: FuturesUnordered<Job> = stops
            .into_iter()
            .map(|p| r.announce_job(p, stats))
            .collect();
        let _ = tokio::time::timeout(stop_timeout, jobs.collect::<Vec<_>>()).await;
    }
    this.list.lock().reset_after_stop();
}

impl ManagedTorrentShared {
    /// Record where a peer address was learned (for per-source counts).
    pub fn record_peer_source(&self, addr: SocketAddr, source: PeerSource) {
        self.tracker_manager.peer_sources.record(addr, source);
    }

    /// The torrent's tracker manager.
    pub fn tracker_manager(&self) -> &Arc<TorrentTrackers> {
        &self.tracker_manager
    }
}

impl ManagedTorrent {
    fn tm(&self) -> &Arc<TorrentTrackers> {
        &self.shared.tracker_manager
    }

    /// Status of every tracker, in announce order.
    pub fn trackers(&self) -> Vec<TrackerStatus> {
        self.tm().statuses()
    }

    /// The current tracker list as `(url, tier)`, for persistence.
    pub fn tracker_list(&self) -> Vec<(String, u32)> {
        self.tm().entries()
    }

    /// Add trackers; duplicates (by URL) and invalid URLs are ignored.
    /// Returns how many were added. Takes effect immediately.
    pub fn add_trackers(&self, entries: Vec<(String, u32)>) -> usize {
        self.tm().add(entries)
    }

    /// Remove trackers by URL. Returns how many were removed.
    pub fn remove_trackers(&self, urls: &[String]) -> usize {
        self.tm().remove(urls)
    }

    /// Replace a tracker URL in place (keeping its tier).
    pub fn edit_tracker(&self, old: &str, new: &str) -> Result<(), TrackerEditError> {
        self.tm().edit(old, new)
    }

    /// Replace the whole tracker list. Trackers that stay keep their status.
    pub fn replace_trackers(&self, entries: Vec<(String, u32)>) {
        self.tm().replace(entries)
    }

    /// Replace the whole tracker list; only new URLs listed in
    /// `trusted_urls` are trusted by SSRF mitigation (unless the torrent is
    /// from a local source). Trackers that stay keep their status and trust.
    pub fn replace_trackers_with_trust(&self, entries: Vec<(String, u32)>, trusted_urls: &[String]) {
        self.tm().replace_with_trust(entries, trusted_urls)
    }

    /// Whether a tracker of this torrent is trusted by SSRF mitigation.
    pub fn is_tracker_trusted(&self, url: &str) -> Option<bool> {
        self.tm().with_list(|l| l.is_trusted(url))
    }

    /// Announce now, ignoring the minimum interval: to one tracker (even a
    /// backup tracker of its tier) or to all. Returns false if the URL is not
    /// in the list. No effect while the torrent is stopped.
    pub fn force_reannounce(&self, url: Option<&str>) -> bool {
        self.tm().force_reannounce(url)
    }

    /// Largest seeder and leecher counts reported by any tracker.
    pub fn swarm_counts(&self) -> (Option<u32>, Option<u32>) {
        self.tm().swarm_counts()
    }

    /// Connected peers counted per discovery source (Trackers tab DHT, PeX
    /// and LSD rows). PeX counts need the live engine to record PeX sources.
    pub fn peer_source_counts(&self) -> PeerSourceCounts {
        let Some(live) = self.live() else {
            return PeerSourceCounts::default();
        };
        let snapshot = live.per_peer_stats_snapshot(PeerStatsFilter {
            state: PeerStatsFilterState::Live,
        });
        self.tm().peer_sources.count(
            snapshot
                .peers
                .keys()
                .filter_map(|a| a.parse::<SocketAddr>().ok()),
        )
    }

    /// Sources a peer address was learned from.
    pub fn peer_sources(&self, addr: SocketAddr) -> PeerSources {
        self.tm().peer_sources.sources(addr)
    }

    /// Tell the tracker manager that the torrent may have completed; it sends
    /// `completed` once the torrent has all pieces. Completion is also
    /// detected automatically.
    pub fn notify_completed(&self) {
        self.tm().wake();
    }

    /// Whether the torrent is private (BEP 27).
    pub fn is_private(&self) -> bool {
        self.tm().is_private()
    }

    /// DHT/PeX/LSD switched off for this torrent (libtorrent torrent flags).
    pub fn disabled_discovery(&self) -> DisabledDiscovery {
        self.tm().disabled_discovery()
    }

    /// Switch DHT, PeX or LSD off (or back on) for this torrent, at once:
    /// DHT/LSD lookups and announces stop or restart, PeX messages are no
    /// longer sent or accepted (and `ut_pex` is not offered on new
    /// connections). Private torrents never use them anyway.
    pub fn set_disabled_discovery(&self, d: DisabledDiscovery) {
        self.tm().set_disabled_discovery(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> Arc<TorrentTrackers> {
        let mut list = TrackerList::new();
        list.add("http://t/announce", 0, TrackerOrigin::Torrent, false);
        list.add("http://s/announce", 1, TrackerOrigin::Session, true);
        TorrentTrackers::new(Id20::new([7; 20]), list, false, false, false, None)
    }

    #[tokio::test]
    async fn private_metadata_drops_session_trackers_and_signals() {
        let tm = manager();
        assert!(!tm.is_private());
        let waiter = tokio::spawn(tm.clone().wait_private());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished());
        tm.set_private();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        assert!(tm.is_private());
        assert_eq!(tm.entries(), vec![("http://t/announce".to_owned(), 0)]);
        // Already private: resolves immediately.
        tokio::time::timeout(Duration::from_secs(1), tm.clone().wait_private())
            .await
            .unwrap();
    }

    #[test]
    fn disabled_discovery_switches() {
        let tm = manager();
        let mut rx = tm.watch_disabled_discovery();
        assert_eq!(tm.disabled_discovery(), DisabledDiscovery::default());
        let off = DisabledDiscovery {
            dht: true,
            pex: false,
            lsd: true,
        };
        assert!(tm.set_disabled_discovery(off));
        assert!(rx.has_changed().unwrap());
        assert_eq!(*rx.borrow_and_update(), off);
        // Setting the same value again doesn't wake the streams.
        assert!(!tm.set_disabled_discovery(off));
        assert!(!rx.has_changed().unwrap());
        assert!(off.blocks(PeerSource::Dht) && off.blocks(PeerSource::Lsd));
        assert!(!off.blocks(PeerSource::Pex) && !off.blocks(PeerSource::Tracker));
    }

    #[test]
    fn edits_go_through_the_list() {
        let tm = manager();
        assert_eq!(
            tm.add(vec![
                ("udp://u:1/announce".into(), 0),
                ("http://t/announce".into(), 3)
            ]),
            1
        );
        assert_eq!(tm.len(), 3);
        assert_eq!(tm.edit("http://t/announce", "http://t2/announce"), Ok(()));
        assert_eq!(
            tm.edit("http://nope/", "http://t3/announce"),
            Err(TrackerEditError::NotFound)
        );
        assert_eq!(tm.remove(&["http://s/announce".to_owned()]), 1);
        tm.replace(vec![("http://t2/announce".into(), 1)]);
        assert_eq!(tm.entries(), vec![("http://t2/announce".to_owned(), 1)]);
        assert!(tm.force_reannounce(Some("http://t2/announce")));
        assert!(!tm.force_reannounce(Some("http://zzz/")));
        assert!(
            tm.statuses()
                .iter()
                .all(|s| s.state == tracker_comms::TrackerState::NotContacted)
        );
    }
}
