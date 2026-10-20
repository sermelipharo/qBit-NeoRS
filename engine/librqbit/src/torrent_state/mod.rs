mod control;
pub(crate) mod dht_pacing;
pub mod initializing;
pub mod live;
pub(crate) mod missing_layers;
pub(crate) mod resume_stats;
pub mod paused;
pub(crate) mod runtime;
pub mod stats;
mod storage_ops;
mod streaming;
pub mod trackers;
pub mod utils;
pub mod webseeds;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;
use arc_swap::ArcSwapOption;
use buffers::ByteBufOwned;
use bytes::Bytes;
use futures::FutureExt;
use futures::future::BoxFuture;
use librqbit_core::hash_id::Id20;
use librqbit_core::lengths::Lengths;

use librqbit_core::spawn_utils::spawn_with_cancel;
use librqbit_core::torrent_metainfo::ValidatedTorrentMetaV1Info;
pub use live::*;
use parking_lot::RwLock;

use tokio::sync::Notify;
use tokio::time::timeout;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::debug_span;
use tracing::trace;
use tracing::warn;

use crate::Session;
use crate::chunk_tracker::ChunkTracker;
use crate::file_info::FileInfo;
use crate::session::TorrentId;
use crate::spawn_utils::BlockingSpawner;
use crate::storage::BoxStorageFactory;
use crate::storage::StorageLayout;
use crate::storage::TorrentErrorKind;
use crate::storage::error::error_kind_of;
use crate::storage::paths::sanitized_torrent_path;
use crate::stream_connect::StreamConnector;
use crate::torrent_state::stats::LiveStats;
use crate::type_aliases::FileInfos;
use crate::type_aliases::PeerStream;

use initializing::TorrentStateInitializing;

use self::paused::TorrentStatePaused;
pub use self::stats::{TorrentStats, TorrentStatsState};
pub use self::streaming::FileStream;

// State machine transitions.
//
// - error -> initializing
// - initializing -> paused
// - paused -> live
// - live -> paused
//
// - initializing -> error
// - live -> error
pub enum ManagedTorrentState {
    Initializing(Arc<TorrentStateInitializing>),
    Paused(TorrentStatePaused),
    Live(Arc<TorrentStateLive>),
    Error(anyhow::Error),

    // This is used when swapping between states, outside world should never see it.
    None,
}

impl ManagedTorrentState {
    pub fn name(&self) -> &'static str {
        match self {
            ManagedTorrentState::Initializing(_) => "initializing",
            ManagedTorrentState::Paused(_) => "paused",
            ManagedTorrentState::Live(_) => "live",
            ManagedTorrentState::Error(_) => "error",
            ManagedTorrentState::None => "<invalid: none>",
        }
    }

    fn assert_paused(self) -> TorrentStatePaused {
        match self {
            Self::Paused(paused) => paused,
            _ => panic!("Expected paused state"),
        }
    }

    pub(crate) fn take(&mut self) -> Self {
        std::mem::replace(self, Self::None)
    }
}

pub(crate) struct ManagedTorrentLocked {
    // The torrent might not be in "paused" state technically,
    // but the intention might be for it to stay paused.
    //
    // This should change only on "unpause".
    pub(crate) paused: bool,
    pub(crate) state: ManagedTorrentState,
    pub(crate) only_files: Option<Vec<usize>>,
    // Download priority of every file; 0 for files not in only_files.
    pub(crate) file_priorities: Vec<u8>,
}

#[derive(Default)]
pub(crate) struct ManagedTorrentOptions {
    pub peer_connect_timeout: Option<Duration>,
    pub peer_read_write_timeout: Option<Duration>,
    pub peer_max_request_window: Option<usize>,
    pub allow_overwrite: bool,
    /// Assume all data is present instead of hashing it on the first check; pieces
    /// are verified when first read.
    pub skip_hash_check: bool,
    pub initial_peers: Vec<SocketAddr>,
    #[cfg(feature = "disable-upload")]
    pub _disable_upload: bool,
}

impl ManagedTorrentOptions {
    #[cfg(feature = "disable-upload")]
    pub fn disable_upload(&self) -> bool {
        self._disable_upload
    }

    #[cfg(not(feature = "disable-upload"))]
    pub const fn disable_upload(&self) -> bool {
        false
    }
}

// Torrent bencodee "info" + some precomputed fields based on it for frequent access.
pub struct TorrentMetadata {
    pub info: ValidatedTorrentMetaV1Info<ByteBufOwned>,
    pub torrent_bytes: Bytes,
    pub info_bytes: Bytes,
    pub file_infos: FileInfos,
    /// A pure v2 torrent whose `torrent_bytes` lack some piece layers
    /// (fetched while it runs, see [`ManagedTorrent::torrent_bytes`]).
    pub layers_were_missing: bool,
}

impl TorrentMetadata {
    pub(crate) fn new(
        info: ValidatedTorrentMetaV1Info<ByteBufOwned>,
        torrent_bytes: Bytes,
        info_bytes: Bytes,
    ) -> anyhow::Result<Self> {
        // Torrent supplied names are sanitized so that no file can end up outside
        // the output folder whatever the metadata says (see storage::paths).
        let mut seen = HashSet::new();
        let file_infos = info
            .iter_file_details_ext()
            .enumerate()
            .map(|(idx, fd)| {
                let mut relative_filename =
                    sanitized_torrent_path(fd.details.filename.iter_components());
                if !fd.details.attrs().padding && !seen.insert(relative_filename.clone()) {
                    // Only possible if sanitizing made two names equal.
                    let mut name = relative_filename
                        .file_name()
                        .map(|n| n.to_os_string())
                        .unwrap_or_default();
                    name.push(format!("_{idx}"));
                    relative_filename.set_file_name(name);
                    seen.insert(relative_filename.clone());
                }
                Ok::<_, anyhow::Error>(FileInfo {
                    relative_filename,
                    offset_in_torrent: fd.offset,
                    piece_range: fd.pieces,
                    len: fd.details.len,
                    attrs: fd.details.attrs(),
                })
            })
            .collect::<anyhow::Result<Vec<FileInfo>>>()?;

        Ok(Self {
            layers_were_missing: info.is_pure_v2() && !info.has_piece_hashes(),
            info,
            torrent_bytes,
            info_bytes,
            file_infos,
        })
    }

    pub fn lengths(&self) -> &Lengths {
        self.info.lengths()
    }
}

/// Common information about torrent shared among all possible states.
///
// The reason it's not inlined into ManagedTorrent is to break the Arc cycle:
// ManagedTorrent contains the current torrent state, which in turn needs access to a bunch
// of stuff, but it shouldn't access the state.
pub struct ManagedTorrentShared {
    pub id: TorrentId,
    pub info_hash: Id20,
    /// A hybrid (v1 + v2) torrent's v2 hash truncated to 20 bytes: like
    /// libtorrent, the torrent is announced (trackers, DHT, LSD) and
    /// reachable (handshakes, MSE) under it too. `info_hash` stays its v1
    /// hash. `None` for v1-only and pure v2 torrents.
    pub info_hash_alt: Option<Id20>,
    pub(crate) spawner: BlockingSpawner,
    /// Trackers the torrent was added with. The live, editable list is in
    /// `tracker_manager` (see `ManagedTorrent::trackers`).
    pub trackers: HashSet<url::Url>,
    pub(crate) tracker_manager: Arc<trackers::TorrentTrackers>,
    /// Web seeds (BEP 19) and their status; survives pause/unpause.
    pub(crate) web_seeds: Arc<webseeds::TorrentWebSeeds>,
    pub peer_id: Id20,
    pub span: tracing::Span,
    pub(crate) options: ManagedTorrentOptions,
    pub(crate) connector: Arc<StreamConnector>,
    pub(crate) storage_factory: BoxStorageFactory,
    /// Output folder, file paths and their on-disk decorations.
    pub(crate) layout: Arc<StorageLayout>,
    pub(crate) session: Weak<Session>,

    // "dn" from magnet link
    pub(crate) magnet_name: Option<String>,

    pub(crate) client_name_and_version: String,

    // Settings changeable at runtime that survive pause/unpause.
    pub(crate) runtime: runtime::TorrentRuntime,

    /// `piece layers` of a hybrid torrent added without them (magnet link),
    /// fetched from a v2 peer while it ran (see `live::v2_hashes`); kept
    /// across pause/unpause.
    pub(crate) v2_fetched_layers: std::sync::OnceLock<Bytes>,
    /// Layers fetched while running (`v2_fetched_layers`, or those a pure
    /// v2 torrent lacked) were not handed out by
    /// [`ManagedTorrent::take_fetched_piece_layers`] yet.
    pub(crate) v2_layers_unsaved: std::sync::atomic::AtomicBool,
    /// A pure v2 torrent missing piece layers: pieces with data on disk
    /// that couldn't be verified yet (set by the check), hash-checked when
    /// their file's layer arrives (see `missing_layers`).
    pub(crate) v2_unverified: parking_lot::Mutex<Option<crate::type_aliases::BF>>,
    /// The session's budget for piece layers being fetched.
    pub(crate) fetch_budget: Arc<crate::peer_info_reader::budget::FetchBudget>,
}

impl ManagedTorrentShared {
    pub(crate) fn client_name_and_version(&self) -> &str {
        &self.client_name_and_version
    }

    /// The on-disk folder this torrent's files are written under (changes when the
    /// storage is moved).
    pub fn output_folder(&self) -> PathBuf {
        self.layout.output_folder()
    }
}

pub struct ManagedTorrent {
    // Static torrent configuration that doesn't change.
    pub shared: Arc<ManagedTorrentShared>,
    // Torrent metadata. Maybe be None when the magnet is resolving (not implemented yet)
    pub metadata: ArcSwapOption<TorrentMetadata>,
    pub(crate) state_change_notify: Notify,
    pub(crate) locked: RwLock<ManagedTorrentLocked>,
}

impl ManagedTorrent {
    pub fn id(&self) -> TorrentId {
        self.shared.id
    }

    pub fn name(&self) -> Option<String> {
        if let Some(m) = &*self.metadata.load() {
            return m
                .info
                .name()
                .map(|n| n.into_owned())
                .or_else(|| self.shared.magnet_name.clone());
        }
        self.shared.magnet_name.clone()
    }

    pub fn shared(&self) -> &ManagedTorrentShared {
        &self.shared
    }

    /// The resolved on-disk folder this torrent's files are written under (changes
    /// when the storage is moved).
    pub fn output_folder(&self) -> PathBuf {
        self.shared.layout.output_folder()
    }

    pub fn with_metadata<R>(
        &self,
        mut f: impl FnMut(&Arc<TorrentMetadata>) -> R,
    ) -> anyhow::Result<R> {
        let r = self.metadata.load();
        let r = r.as_ref().context("torrent is not resolved")?;
        Ok(f(r))
    }

    pub fn info_hash(&self) -> Id20 {
        self.shared.info_hash
    }

    /// The truncated v2 hash of a hybrid torrent (see
    /// [`ManagedTorrentShared::info_hash_alt`]).
    pub fn info_hash_alt(&self) -> Option<Id20> {
        self.shared.info_hash_alt
    }

    /// Piece layers fetched from peers while the torrent ran, for the
    /// caller to save in its copy of the .torrent: those of a hybrid torrent
    /// added without them (by a magnet link), once all are known, or every
    /// layer known so far of a pure v2 torrent whose .torrent lacked some
    /// (after each newly known file). Returned once per change.
    pub fn take_fetched_piece_layers(&self) -> Option<Bytes> {
        if !self
            .shared
            .v2_layers_unsaved
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            return None;
        }
        if let Some(l) = self.shared.v2_fetched_layers.get() {
            return Some(l.clone());
        }
        self.known_piece_layers()
    }

    /// A pure v2 torrent: the bencoded `piece layers` known now.
    fn known_piece_layers(&self) -> Option<Bytes> {
        let m = self.metadata.load_full()?;
        let layout = m.info.v2_layout()?;
        let hashes = m.info.v2_piece_hashes()?;
        Some(hashes.piece_layers_bencode(layout).into())
    }

    /// Files of a pure v2 torrent whose piece layer isn't known yet (the
    /// .torrent lacked it and it hasn't been fetched); 0 otherwise.
    pub fn missing_piece_layers(&self) -> u32 {
        self.metadata
            .load()
            .as_ref()
            .and_then(|m| m.info.v2_piece_hashes().map(|h| h.missing_count()))
            .unwrap_or(0)
    }

    /// Adds piece layers from another copy of the .torrent (its bencoded
    /// `piece layers` dictionary) to a pure v2 torrent that lacks some.
    /// Every entry must hash up to its file's `pieces root` (else nothing
    /// is taken); the files that get their layer are then verified and
    /// downloaded like the others. Returns how many files got one.
    pub fn add_piece_layers(&self, piece_layers: &[u8]) -> anyhow::Result<usize> {
        let m = self
            .metadata
            .load_full()
            .context("torrent is not resolved")?;
        let (Some(layout), Some(hashes)) = (m.info.v2_layout(), m.info.v2_piece_hashes()) else {
            return Ok(0);
        };
        if hashes.is_complete() {
            return Ok(0);
        }
        let spans = self.shared.spawner.block_in_place(|| {
            let given =
                librqbit_core::v2::V2PieceHashes::from_piece_layers(Some(piece_layers), layout)?;
            let mut spans = Vec::new();
            for (i, s) in layout.spans.iter().enumerate() {
                if hashes.span_known(i) || !given.span_known(i) {
                    continue;
                }
                if let Some(r) = given.with_span(layout, i, |layer| {
                    hashes.set_layer(layout, &s.pieces_root, layer)
                }) {
                    spans.extend(r?);
                }
            }
            anyhow::Ok(spans)
        })?;
        let added = spans.len();
        if !spans.is_empty()
            && let Some(live) = self.live()
        {
            live.on_v2_layers_known(spans);
        }
        Ok(added)
    }

    /// The .torrent: the one the torrent was added with, plus (for a pure
    /// v2 torrent that lacked some) the piece layers fetched since.
    pub fn torrent_bytes(&self) -> Option<Bytes> {
        let m = self.metadata.load_full()?;
        if !m.layers_were_missing {
            return Some(m.torrent_bytes.clone());
        }
        let layers = self.known_piece_layers()?;
        Some(
            librqbit_core::torrent_metainfo::torrent_with_piece_layers(&m.torrent_bytes, &layers)
                .map(Bytes::from)
                .unwrap_or_else(|_| m.torrent_bytes.clone()),
        )
    }

    /// `h` identifies this torrent on the wire (either hash of a hybrid).
    pub fn has_info_hash(&self, h: Id20) -> bool {
        self.shared.info_hash == h || self.shared.info_hash_alt == Some(h)
    }

    pub fn only_files(&self) -> Option<Vec<usize>> {
        self.locked.read().only_files.clone()
    }

    pub fn with_state<R>(&self, f: impl FnOnce(&ManagedTorrentState) -> R) -> R {
        f(&self.locked.read().state)
    }

    pub(crate) fn with_state_mut<R>(&self, f: impl FnOnce(&mut ManagedTorrentState) -> R) -> R {
        f(&mut self.locked.write().state)
    }

    pub(crate) fn with_chunk_tracker<R>(
        &self,
        f: impl FnOnce(&ChunkTracker) -> R,
    ) -> anyhow::Result<R> {
        let g = self.locked.read();
        match &g.state {
            ManagedTorrentState::Paused(p) => Ok(f(&p.chunk_tracker)),
            ManagedTorrentState::Live(l) => Ok(f(l
                .lock_read("chunk_tracker")
                .get_chunks()
                .context("error getting chunks")?)),
            _ => bail!("no chunk tracker, torrent neither paused nor live"),
        }
    }

    /// Get the live state if the torrent is live.
    pub fn live(&self) -> Option<Arc<TorrentStateLive>> {
        let g = self.locked.read();
        match &g.state {
            ManagedTorrentState::Live(live) => Some(live.clone()),
            _ => None,
        }
    }

    // Get live torrent but wait a bit until it's initialized if it is
    pub(crate) async fn live_wait_initializing(
        &self,
        duration: Duration,
    ) -> Option<Arc<TorrentStateLive>> {
        timeout(duration, self.wait_until_initialized())
            .await
            .ok()?
            .ok()?;
        self.live()
    }

    fn stop_with_error(&self, error: anyhow::Error) {
        let mut g = self.locked.write();

        match g.state.take() {
            ManagedTorrentState::Live(live) => {
                if let Err(err) = live.pause() {
                    warn!(
                        id = self.shared.id,
                        info_hash = ?self.shared.info_hash,
                        "error pausing live torrent during fatal error handling: {err:#}",
                    );
                }
            }
            ManagedTorrentState::Error(e) => {
                warn!(
                    id = self.shared.id,
                    info_hash = ?self.shared.info_hash,
                    "bug: torrent already was in error state when trying to stop it. Previous error was: {e:#}",
                );
            }
            ManagedTorrentState::None => {
                warn!(
                    id = self.shared.id,
                    info_hash = ?self.shared.info_hash,
                    "bug: torrent encountered in None state during fatal error handling"
                )
            }
            _ => {}
        };

        self.state_change_notify.notify_waiters();

        g.state = ManagedTorrentState::Error(error)
    }

    /// peer_rx: the peer stream. If start_paused=false, must be set.
    /// start_paused: if set, the torrent will initialize (check file integrity), but will not start
    pub(crate) fn start(
        self: &Arc<Self>,
        peer_rx: Option<PeerStream>,
        start_paused: bool,
    ) -> anyhow::Result<()> {
        fn _start<'a>(
            t: &'a Arc<ManagedTorrent>,
            peer_rx: Option<PeerStream>,
            start_paused: bool,
            session: Arc<Session>,
            g: Option<parking_lot::RwLockWriteGuard<'a, ManagedTorrentLocked>>,
            token: CancellationToken,
        ) -> anyhow::Result<()> {
            let mut g = g.unwrap_or_else(|| t.locked.write());

            match &g.state {
                ManagedTorrentState::Live(_) => {
                    bail!("torrent is already live");
                }
                ManagedTorrentState::Initializing(init) => {
                    let init = init.clone();
                    init.clear_pause_request();
                    if !init.try_start_check() {
                        return Ok(());
                    }

                    let t = t.clone();
                    let span = t.shared().span.clone();
                    let token = token.clone();

                    spawn_with_cancel(
                        debug_span!(parent: span.clone(), "initialize_and_start"),
                        "initialize_and_start",
                        token.clone(),
                        async move {
                            // Waiting for a checking slot can take long; a pause
                            // request (e.g. from a storage operation) ends the wait.
                            let permit = tokio::select! {
                                permit = session.checking_limiter.acquire() => Some(permit),
                                _ = init.wait_pause_requested() => None,
                            };
                            let check_result = match permit {
                                Some(_permit) => init.check().await,
                                None => Err(anyhow::anyhow!("initial check paused")),
                            };
                            init.finish_check();

                            match check_result {
                                Ok(mut paused) => {
                                    let mut g = t.locked.write();
                                    if let ManagedTorrentState::Initializing(_) = &g.state {
                                    } else {
                                        debug!(
                                            "no need to start torrent anymore, as it switched state from initializing"
                                        );
                                        return Ok(());
                                    }

                                    // The file selection may have changed while checking.
                                    if let Some(only_files) = &g.only_files {
                                        let only_files: HashSet<usize> =
                                            only_files.iter().copied().collect();
                                        if let Err(e) = paused.update_only_files(&only_files) {
                                            warn!("error applying file selection: {e:#}");
                                        }
                                    }

                                    g.state = ManagedTorrentState::Paused(paused);
                                    t.state_change_notify.notify_waiters();
                                    _start(&t, peer_rx, start_paused, session, Some(g), token)
                                }
                                Err(err) => {
                                    if init.is_pause_requested() {
                                        debug!("initial check paused");
                                        t.state_change_notify.notify_waiters();
                                        return Ok(());
                                    }

                                    let result = anyhow::anyhow!("{:?}", err);
                                    t.locked.write().state = ManagedTorrentState::Error(err);
                                    t.state_change_notify.notify_waiters();
                                    Err(result)
                                }
                            }
                        },
                    );
                    Ok(())
                }
                ManagedTorrentState::Paused(p) => {
                    if start_paused {
                        return Ok(());
                    }
                    if let Some(err) = t.missing_files_error(p) {
                        g.state = ManagedTorrentState::Error(err);
                        t.state_change_notify.notify_waiters();
                        return Ok(());
                    }
                    let paused = g.state.take().assert_paused();
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let live =
                        TorrentStateLive::new(paused, &g.file_priorities, tx, token.clone())?;
                    g.state = ManagedTorrentState::Live(live.clone());
                    t.state_change_notify.notify_waiters();

                    spawn_fatal_errors_receiver(t, rx, token);
                    if let Some(peer_rx) = peer_rx {
                        spawn_peer_adder(&live, peer_rx);
                    }
                    Ok(())
                }
                ManagedTorrentState::Error(e) => {
                    // Missing files keep their fastresume data, so that they are
                    // detected again (or validated, if they are back).
                    let full_check = error_kind_of(e) != TorrentErrorKind::MissingFiles;
                    let metadata = t.metadata.load_full().expect("TODO");
                    let initializing = Arc::new(TorrentStateInitializing::new(
                        t.shared.clone(),
                        metadata.clone(),
                        g.only_files.clone(),
                        t.shared
                            .storage_factory
                            .create_and_init(t.shared(), &metadata)?,
                        full_check,
                    ));
                    g.state = ManagedTorrentState::Initializing(initializing.clone());
                    t.state_change_notify.notify_waiters();

                    // Recurse.
                    _start(t, peer_rx, start_paused, session, Some(g), token)
                }
                ManagedTorrentState::None => bail!("bug: torrent is in empty state"),
            }
        }

        let session = self
            .shared
            .session
            .upgrade()
            .context("session is dead, cannot start torrent")?;
        self.shared.layout.set_owner(Arc::downgrade(self));
        let mut g = self.locked.write();
        g.paused = start_paused;
        if self.shared.layout.is_busy() {
            // A storage operation has the torrent paused internally; it applies the
            // intent recorded above when it's done.
            return Ok(());
        }
        let cancellation_token = session.cancellation_token().child_token();

        _start(
            self,
            peer_rx,
            start_paused,
            session,
            Some(g),
            cancellation_token,
        )
    }

    pub fn is_paused(&self) -> bool {
        self.locked.read().paused
    }

    /// Records the files' sizes and mtimes with the fastresume bitfield (paused
    /// or live torrents), so that the next start trusts unchanged files without
    /// reading them. See `resume_stats` for why this is safe while writing.
    pub(crate) async fn save_resume_stats(&self) {
        let Some(session) = self.shared.session.upgrade() else {
            return;
        };
        let Some(metadata) = self.metadata.load_full() else {
            return;
        };
        let has_bits = self.with_state(|s| {
            matches!(s, ManagedTorrentState::Paused(_) | ManagedTorrentState::Live(_))
        });
        if !has_bits {
            return;
        }
        let stats = self
            .shared
            .spawner
            .block_in_place_with_semaphore(|| {
                resume_stats::record_for(&self.shared.layout, &metadata.file_infos, || {
                    self.with_chunk_tracker(|ct| resume_stats::copy_bits(ct.get_have_pieces()))
                        .ok()
                })
            })
            .await;
        if let Err(e) = session
            .bitv_factory
            .store_resume_stats(self.shared.info_hash.into(), stats.map(|s| s.encode()))
            .await
        {
            warn!(id = self.shared.id, "error storing file stats: {e:#}");
        }
    }

    /// Pause the torrent if it's live.
    pub(crate) fn pause(&self) -> anyhow::Result<()> {
        let mut g = self.locked.write();
        if self.shared.layout.is_busy() {
            // See start().
            g.paused = true;
            return Ok(());
        }
        match &g.state {
            ManagedTorrentState::Live(live) => {
                let paused = live.pause()?;
                g.state = ManagedTorrentState::Paused(paused);
                g.paused = true;
                self.state_change_notify.notify_waiters();
                Ok(())
            }
            ManagedTorrentState::Initializing(init) => {
                let init = init.clone();
                g.paused = true;
                init.request_pause();
                self.state_change_notify.notify_waiters();
                Ok(())
            }
            ManagedTorrentState::Paused(_) => {
                bail!("torrent is already paused");
            }
            ManagedTorrentState::Error(_) => {
                bail!("can't pause torrent in error state")
            }
            ManagedTorrentState::None => bail!("bug: torrent is in empty state"),
        }
    }

    /// Get stats.
    pub fn stats(&self) -> TorrentStats {
        use stats::TorrentStatsState as S;
        let mut resp = TorrentStats {
            total_bytes: self
                .metadata
                .load()
                .as_ref()
                .map(|r| r.info.lengths().total_length())
                .unwrap_or_default(),
            file_progress: Vec::new(),
            state: S::Error,
            error: None,
            progress_bytes: 0,
            uploaded_bytes: 0,
            finished: false,
            live: None,
        };

        {
            let g = self.locked.read();
            match &g.state {
                ManagedTorrentState::Initializing(i) => {
                    resp.state = S::Initializing { paused: g.paused };
                    resp.progress_bytes = i.checked_bytes.load(Ordering::Relaxed);
                }
                ManagedTorrentState::Paused(p) => {
                    resp.state = S::Paused;
                    let hns = p.hns();
                    resp.total_bytes = hns.total();
                    resp.progress_bytes = hns.progress();
                    resp.finished = hns.finished();
                    resp.file_progress = p.chunk_tracker.per_file_have_bytes().to_owned();
                }
                ManagedTorrentState::Live(l) => {
                    resp.state = S::Live;
                    let live_stats = LiveStats::from(l.as_ref());
                    let hns = l.get_hns().unwrap_or_default();
                    resp.total_bytes = hns.total();
                    resp.progress_bytes = hns.progress();
                    resp.finished = hns.finished();
                    resp.uploaded_bytes = l.get_uploaded_bytes();
                    resp.file_progress = l
                        .lock_read("file_progress")
                        .get_chunks()
                        .ok()
                        .map(|c| c.per_file_have_bytes().to_owned())
                        .unwrap_or_default();
                    resp.live = Some(live_stats);
                }
                ManagedTorrentState::Error(e) => {
                    resp.state = S::Error;
                    resp.error = Some(format!("{e:?}"))
                }
                ManagedTorrentState::None => {
                    resp.state = S::Error;
                    resp.error = Some("bug: torrent in broken \"None\" state".to_string());
                }
            }
        }

        resp
    }

    #[inline(never)]
    pub fn wait_until_initialized(&self) -> BoxFuture<'_, anyhow::Result<()>> {
        async move {
            // TODO: rewrite, this polling is horrible
            loop {
                let done = self.with_state(|s| match s {
                    ManagedTorrentState::Initializing(_) => Ok(false),
                    ManagedTorrentState::Error(e) => bail!("{:?}", e),
                    ManagedTorrentState::None => bail!("bug: torrent state is None"),
                    _ => Ok(true),
                })?;
                if done {
                    return Ok(());
                }
                let _ = timeout(
                    Duration::from_millis(100),
                    self.state_change_notify.notified(),
                )
                .await;
            }
        }
        .boxed()
    }

    #[inline(never)]
    pub fn wait_until_completed(&self) -> BoxFuture<'_, anyhow::Result<()>> {
        async move {
            // TODO: rewrite, this polling is horrible
            let live = loop {
                let live = self.with_state(|s| match s {
                    ManagedTorrentState::Initializing(_) | ManagedTorrentState::Paused(_) => {
                        Ok(None)
                    }
                    ManagedTorrentState::Live(l) => Ok(Some(l.clone())),
                    ManagedTorrentState::Error(e) => bail!("{:?}", e),
                    ManagedTorrentState::None => bail!("bug: torrent state is None"),
                })?;
                if let Some(live) = live {
                    break live;
                }
                let _ = timeout(Duration::from_secs(1), self.state_change_notify.notified()).await;
            };

            live.wait_until_completed().await;
            Ok(())
        }
        .boxed()
    }
}

pub type ManagedTorrentHandle = Arc<ManagedTorrent>;

fn spawn_fatal_errors_receiver(
    state: &Arc<ManagedTorrent>,
    rx: tokio::sync::oneshot::Receiver<anyhow::Error>,
    token: CancellationToken,
) {
    let span = state.shared.span.clone();
    let id = state.shared.id;
    let info_hash = state.shared.info_hash;
    let state = Arc::downgrade(state);
    spawn_with_cancel::<&'static str>(
        debug_span!(parent: span, "fatal_errors_receiver"),
        "fatal_errors_receiver",
        token,
        async move {
            let e = match rx.await {
                Ok(e) => e,
                Err(_) => return Ok(()),
            };
            if let Some(state) = state.upgrade() {
                state.stop_with_error(e);
            } else {
                warn!(
                    ?id,
                    ?info_hash,
                    "tried to stop the torrent with error, but couldn't upgrade the arc"
                );
            }
            Ok(())
        },
    );
}

fn spawn_peer_adder(live: &Arc<TorrentStateLive>, mut peer_rx: PeerStream) {
    live.spawn(
        debug_span!(parent: live.torrent().span.clone(), "external_peer_adder"),
        format!("[{}]external_peer_adder", live.shared.id),
        {
            let live = live.clone();
            async move {
                let live = {
                    let weak = Arc::downgrade(&live);
                    drop(live);
                    weak
                };

                // The task is cancelled with the live state (pause, error,
                // session stop), so there is no need to poll for it: a 5 s
                // timeout per torrent here kept idle sessions waking up.
                while let Some(peer) = peer_rx.next().await {
                    trace!(?peer, "received peer");
                    let Some(live) = live.upgrade() else {
                        return Ok(());
                    };
                    live.add_peer_if_not_seen(peer)?;
                }
                debug!("peer_rx closed, closing peer adder");
                Ok(())
            }
        },
    );
}
