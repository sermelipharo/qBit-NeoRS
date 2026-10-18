//! Web seeds (BEP 19) as pseudo-peers of the live torrent.
//!
//! One task per live torrent schedules every web seed of the torrent: each
//! usable seed fetches one piece at a time, picked by the same piece picker
//! as peers (priorities, streams, sequential mode), with the torrent's and
//! the session's download rate limits. In the piece tracker a seed is the
//! pseudo-peer [`pseudo_peer_addr`] (IPv6 discard prefix), which "has" every
//! piece and never steals; peers may steal a slow seed's piece.

use std::{
    net::SocketAddr,
    num::NonZeroU32,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use buffers::ByteBuf;
use futures::{StreamExt, stream::FuturesUnordered};
use librqbit_core::lengths::ValidPieceIndex;
use librqbit_webseed::{Layout, LayoutFile, WebSeedError};
use peer_binary_protocol::Piece;
use tracing::{debug, trace, warn};

use crate::{
    chunk_tracker::ChunkMarkingResult,
    piece_tracker::{AcquireRequest, AcquireResult},
    torrent_state::webseeds::{FetchOutcome, pseudo_peer_addr},
};

use super::{TorrentMetadata, TorrentStateLive, TorrentStateLocked};

/// How often the scheduler looks again when nothing woke it up.
const IDLE_POLL: Duration = Duration::from_secs(5);
/// Safety net while the torrent is finished: priority changes and streams on
/// unwanted files wake the scheduler through `new_pieces_notify`, so this
/// timer only guards against a missed notification (a 5 s poll per finished
/// torrent kept idle seeding sessions waking up).
const FINISHED_POLL: Duration = Duration::from_secs(60);
/// Web seed fetches of one torrent at a time (a torrent may list 100 seeds,
/// each holding a piece buffer).
const MAX_FETCHES_PER_TORRENT: usize = 4;

/// Overall deadline of one piece fetch: a minute plus 16 KiB/s, so a server
/// dripping bytes can't hold a piece (and its buffer) forever.
fn fetch_deadline(piece_len: u32) -> Duration {
    Duration::from_secs(60 + u64::from(piece_len) / (16 * 1024))
}

/// The torrent's file layout for URL mapping, from the original (unsanitized)
/// path components of the metadata.
pub(crate) fn layout_from_metadata(metadata: &TorrentMetadata) -> Layout {
    let info = &metadata.info;
    let single_file = info.is_single_file();
    Layout {
        name: info.name().map(|n| n.into_owned()).unwrap_or_default(),
        piece_length: u64::from(metadata.lengths().default_piece_length()),
        single_file,
        files: info
            .iter_file_details()
            .map(|fd| LayoutFile {
                path: if single_file {
                    Vec::new()
                } else {
                    fd.filename
                        .iter_components()
                        .map(|c| c.into_owned())
                        .collect()
                },
                length: fd.len,
                is_pad: fd.attrs().padding,
            })
            .collect(),
    }
}

/// Gives the piece a web seed holds back to the queue when dropped (error,
/// removal, cancellation).
struct ReleaseGuard<'a> {
    state: &'a TorrentStateLive,
    addr: SocketAddr,
}

impl Drop for ReleaseGuard<'_> {
    fn drop(&mut self) {
        let released = self
            .state
            .lock_write("webseed_release")
            .get_pieces_mut()
            .map(|p| p.release_pieces_owned_by(self.addr))
            .unwrap_or(0);
        if released > 0 {
            self.state.new_pieces_notify.notify_waiters();
        }
    }
}

enum WriteResult {
    Verified,
    Wasted,
    HashFailed,
}

impl TorrentStateLive {
    /// The web seed scheduler of this live torrent.
    pub(crate) async fn task_webseeds(self: Arc<Self>) -> crate::Result<()> {
        let seeds = self.shared.web_seeds.clone();
        // The live state that ran before may have left seeds marked busy.
        seeds.reset_downloading();
        let mut layout: Option<Arc<Layout>> = None;
        let mut futs = FuturesUnordered::new();
        loop {
            let changed = seeds.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let new_pieces = self.new_pieces_notify.notified();
            tokio::pin!(new_pieces);
            new_pieces.as_mut().enable();

            let finished = self.is_finished_and_no_active_streams();
            // With I2P mixed mode off, an I2P torrent never fetches from web
            // seeds (clearnet HTTP would reveal our address).
            let i2p_only = self
                .shared
                .session
                .upgrade()
                .is_some_and(|s| s.i2p.i2p_only(&self.shared.tracker_manager));
            if !finished && !i2p_only {
                let room = MAX_FETCHES_PER_TORRENT.saturating_sub(futs.len());
                for (id, url, trusted) in seeds.take_ready_limited(Instant::now(), room) {
                    let layout = layout
                        .get_or_insert_with(|| Arc::new(layout_from_metadata(&self.metadata)))
                        .clone();
                    let this = self.clone();
                    futs.push(async move {
                        let outcome = this.webseed_fetch_one(id, &url, trusted, &layout).await;
                        (id, outcome)
                    });
                }
            }

            let wait = if finished {
                FINISHED_POLL
            } else {
                seeds
                    .next_wakeup()
                    .map(|t| t.saturating_duration_since(Instant::now()))
                    .unwrap_or(IDLE_POLL)
                    .clamp(Duration::from_millis(50), IDLE_POLL)
            };

            tokio::select! {
                Some((id, outcome)) = futs.next(), if !futs.is_empty() => {
                    trace!(id, ?outcome, "web seed fetch finished");
                    seeds.finish(id, outcome);
                }
                _ = &mut changed => {}
                _ = &mut new_pieces, if finished && !seeds.is_empty() => {}
                _ = tokio::time::sleep(wait), if !seeds.is_empty() => {}
            }
        }
    }

    fn webseed_acquire(&self, addr: SocketAddr) -> crate::Result<Option<ValidPieceIndex>> {
        let mut g = self.lock_write("webseed_acquire");
        let TorrentStateLocked {
            pieces,
            piece_priorities,
            v2_blocked,
            ..
        } = &mut **g;
        let pieces = pieces.as_mut().ok_or(crate::Error::ChunkTrackerEmpty)?;
        let result = pieces.acquire_piece(AcquireRequest {
            peer: addr,
            // Web seeds never steal pieces from peers.
            peer_avg_time: None,
            priority_pieces: self.streams.iter_next_pieces(&self.lengths),
            piece_priorities,
            availability: |p| self.peers.availability.get(p),
            sequential: self.shared.runtime.sequential(),
            seed: self.picker_seed,
            // Pieces that can't be verified yet are never fetched.
            peer_has_piece: |p| !super::is_blocked(v2_blocked.as_deref(), p),
            can_steal: |_| false,
        });
        Ok(match result {
            AcquireResult::Reserved(p) | AcquireResult::Stolen { piece: p, .. } => Some(p),
            AcquireResult::NoneAvailable => None,
        })
    }

    async fn webseed_fetch_one(
        &self,
        id: u32,
        url: &str,
        trusted: bool,
        layout: &Layout,
    ) -> FetchOutcome {
        // Session-wide cap on concurrent fetches (memory: one piece buffer each).
        let limiter = match self.shared.session.upgrade() {
            Some(s) => s.webseed_fetches.clone(),
            None => return FetchOutcome::Failed("session is gone".to_owned()),
        };
        let Ok(_fetch_permit) = limiter.acquire_owned().await else {
            return FetchOutcome::Failed("session is gone".to_owned());
        };
        let addr = pseudo_peer_addr(id);
        let piece = match self.webseed_acquire(addr) {
            Ok(Some(p)) => p,
            Ok(None) => return FetchOutcome::NoPiece,
            Err(e) => return FetchOutcome::Failed(format!("{e:#}")),
        };
        let _release = ReleaseGuard { state: self, addr };
        let piece_len = self.lengths.piece_length(piece);

        // Rate limits, block by block like a peer's requests.
        let session = self.shared.session.upgrade();
        let session_limited = session
            .as_ref()
            .is_some_and(|s| s.peer_policy.session_limits_apply(false, false));
        for chunk in self.lengths.iter_chunk_infos(piece) {
            let Some(size) = NonZeroU32::new(chunk.size) else {
                continue;
            };
            if let Err(e) = self
                .shared
                .runtime
                .ratelimits
                .prepare_for_download(size)
                .await
            {
                return FetchOutcome::Failed(format!("{e:#}"));
            }
            if session_limited
                && let Some(s) = session.as_ref()
                && let Err(e) = s.ratelimits.prepare_for_download(size).await
            {
                return FetchOutcome::Failed(format!("{e:#}"));
            }
        }
        let client = match session.as_ref() {
            Some(s) => {
                // The IP filter and bans apply to seeds on literal addresses
                // (names are not resolved here).
                let ip = match url::Url::parse(url).ok().and_then(|u| u.host().map(|h| h.to_owned())) {
                    Some(url::Host::Ipv4(ip)) => Some(std::net::IpAddr::V4(ip)),
                    Some(url::Host::Ipv6(ip)) => Some(std::net::IpAddr::V6(ip)),
                    _ => None,
                };
                if ip.is_some_and(|ip| s.peer_filter.check(ip).is_some()) {
                    return FetchOutcome::Failed("blocked by the IP filter".to_owned());
                }
                s.webseed_http_client(trusted)
            }
            None => return FetchOutcome::Failed("session is gone".to_owned()),
        };
        drop(session);

        let fetch = librqbit_webseed::fetch_piece(&client, url, layout, piece.get());
        let data = match tokio::time::timeout(fetch_deadline(piece_len), fetch).await {
            Err(_) => return FetchOutcome::Failed("timed out".to_owned()),
            Ok(Ok(d)) => d,
            Ok(Err(WebSeedError::RetryAfter(d))) => return FetchOutcome::RetryAfter(d),
            Ok(Err(e)) => {
                debug!(id, piece = piece.get(), "web seed error: {e}");
                return FetchOutcome::Failed(e.to_string());
            }
        };
        if data.len() != piece_len as usize {
            return FetchOutcome::Failed("wrong piece size".to_owned());
        }
        let n = data.len() as u64;
        self.stats.fetched_bytes.fetch_add(n, Ordering::Relaxed);
        self.session_stats.wake_peer_tick();
        self.session_stats
            .counters
            .fetched_bytes
            .fetch_add(n, Ordering::Relaxed);

        // The seed may have been removed while fetching.
        if !self.shared.web_seeds.contains(id) {
            self.add_wasted(0, n);
            return FetchOutcome::Wasted;
        }

        let res = self
            .shared
            .spawner
            .block_in_place_with_semaphore(|| self.webseed_write_and_check(addr, piece, &data))
            .await;
        match res {
            Ok(WriteResult::Verified) => FetchOutcome::Ok { bytes: n },
            Ok(WriteResult::Wasted) => FetchOutcome::Wasted,
            Ok(WriteResult::HashFailed) => FetchOutcome::HashFailed,
            Err(e) => FetchOutcome::Failed(format!("{e:#}")),
        }
    }

    fn webseed_write_and_check(
        &self,
        addr: SocketAddr,
        piece: ValidPieceIndex,
        data: &[u8],
    ) -> anyhow::Result<WriteResult> {
        let piece_len = u64::from(self.lengths.piece_length(piece));
        // While the per-piece lock is held nobody can steal the piece.
        let ppl_guard = {
            let g = self.lock_read("webseed_check_steal");
            let ppl = self.piece_lock(piece.get_usize()).read();
            match g.get_pieces()?.get_inflight(piece) {
                Some(inflight) if inflight.peer == addr => {}
                _ => {
                    self.add_wasted(0, data.len() as u64);
                    return Ok(WriteResult::Wasted);
                }
            }
            ppl
        };

        let mut download_time = None;
        for chunk in self.lengths.iter_chunk_infos(piece) {
            let start = chunk.offset as usize;
            let block = &data[start..start + chunk.size as usize];
            let msg: Piece<ByteBuf<'_>> = Piece::from_data(piece.get(), chunk.offset, block);
            if let Err(e) = self.file_ops().write_chunk(addr, &msg, &chunk) {
                warn!(
                    id = self.shared.id,
                    info_hash = ?self.shared.info_hash,
                    "FATAL: error writing web seed data to disk: {e:#}"
                );
                return self.on_fatal_error(e).map(|_| WriteResult::Wasted);
            }
            let mut g = self.lock_write("webseed_mark_chunk");
            match g.get_pieces_mut()?.mark_chunk_downloaded(&msg) {
                Some(ChunkMarkingResult::Completed) => {
                    download_time = g.get_pieces_mut()?.take_inflight(piece);
                }
                Some(ChunkMarkingResult::NotCompleted) => {}
                Some(ChunkMarkingResult::PreviouslyCompleted) | None => {
                    drop(g);
                    self.add_wasted(0, data.len() as u64);
                    return Ok(WriteResult::Wasted);
                }
            }
        }
        drop(ppl_guard);
        let Some(download_time) = download_time else {
            anyhow::bail!("web seed piece {piece} incomplete after writing all blocks");
        };

        if self.file_ops().check_piece(piece)? {
            self.lock_write("webseed_piece_ok")
                .get_pieces_mut()?
                .mark_piece_hash_ok(piece, &self.metadata.file_infos);
            self.stats
                .downloaded_and_checked_bytes
                .fetch_add(piece_len, Ordering::Release);
            self.stats
                .downloaded_and_checked_pieces
                .fetch_add(1, Ordering::Release);
            self.stats
                .have_bytes
                .fetch_add(piece_len, Ordering::Relaxed);
            #[allow(clippy::cast_possible_truncation)]
            self.stats
                .total_piece_download_ms
                .fetch_add(download_time.as_millis() as u64, Ordering::Relaxed);
            self.on_piece_completed(piece)?;
            self.transmit_haves(piece);
            Ok(WriteResult::Verified)
        } else {
            warn!(
                id = self.shared.id,
                info_hash = ?self.shared.info_hash,
                "piece {piece} from a web seed failed the hash check"
            );
            self.lock_write("webseed_piece_broken")
                .get_pieces_mut()?
                .mark_piece_hash_failed(piece);
            self.add_wasted(piece_len, 0);
            self.new_pieces_notify.notify_waiters();
            Ok(WriteResult::HashFailed)
        }
    }
}
