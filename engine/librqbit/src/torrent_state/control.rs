//! Runtime control and inspection of a managed torrent: file priorities,
//! piece picking modes, super seeding, upload slots, connection and rate
//! limits, piece states and peers.
//!
//! Settings are kept outside the live state, so they work while the torrent
//! is paused (or still checking) and survive pause/unpause. They are not
//! persisted by the engine.

use std::{collections::HashSet, net::SocketAddr};

use anyhow::Context;

use crate::limits::LimitsConfig;

use super::{
    ManagedTorrent, ManagedTorrentState,
    live::{
        availability::count_ones,
        peer_details::{PeerDetails, PeerSource},
        priorities::{
            PIECE_STATE_HAVE, PIECE_STATE_MISSING, PRIORITY_IGNORE, distributed_copies,
            normalize_file_priorities,
        },
    },
};

fn wanted_files(priorities: &[u8]) -> HashSet<usize> {
    priorities
        .iter()
        .enumerate()
        .filter(|(_, p)| **p > PRIORITY_IGNORE)
        .map(|(i, _)| i)
        .collect()
}

impl ManagedTorrent {
    fn file_count(&self) -> anyhow::Result<usize> {
        self.with_metadata(|m| m.file_infos.len())
    }

    fn total_pieces(&self) -> usize {
        self.metadata
            .load()
            .as_ref()
            .map(|m| m.lengths().total_pieces() as usize)
            .unwrap_or(0)
    }

    /// The download priority (libtorrent level 0..=7, 0 = don't download) of
    /// every file. Files start at 1 (or 0 when excluded by `only_files`).
    pub fn file_priorities(&self) -> Vec<u8> {
        self.locked.read().file_priorities.clone()
    }

    /// Set the download priority of every file (one entry per file, values
    /// above 7 are clamped). Priority 0 excludes the file like `only_files`;
    /// pieces of higher priority files are downloaded first, and a piece shared
    /// by several files gets the highest of their priorities. Works in every
    /// state; while checking, it applies once the check is done.
    pub fn set_file_priorities(&self, priorities: Vec<u8>) -> anyhow::Result<()> {
        let file_count = self.file_count()?;
        if priorities.len() != file_count {
            anyhow::bail!(
                "expected {file_count} file priorities, got {}",
                priorities.len()
            );
        }
        let priorities = normalize_file_priorities(priorities, file_count);
        let wanted = wanted_files(&priorities);
        let wanted_now = wanted.clone();

        let mut g = self.locked.write();
        let selection_changed = wanted != wanted_files(&g.file_priorities);
        match &mut g.state {
            ManagedTorrentState::Paused(p) => {
                if selection_changed {
                    p.update_only_files(&wanted)?;
                }
            }
            ManagedTorrentState::Live(l) => {
                l.update_file_priorities(&priorities, selection_changed.then_some(&wanted))?;
            }
            ManagedTorrentState::Initializing(_)
            | ManagedTorrentState::Error(_)
            | ManagedTorrentState::None => {}
        }
        if wanted.len() != file_count || g.only_files.is_some() {
            let mut only_files: Vec<usize> = wanted.into_iter().collect();
            only_files.sort_unstable();
            g.only_files = Some(only_files);
        }
        g.file_priorities = priorities;
        drop(g);
        if selection_changed {
            self.on_files_wanted_changed(|idx| wanted_now.contains(&idx));
        }
        Ok(())
    }

    /// Whether pieces are downloaded in order (within each priority level).
    pub fn sequential(&self) -> bool {
        self.shared.runtime.sequential()
    }

    /// Download pieces in index order instead of rarest-first. Higher priority
    /// pieces (including first/last pieces) still come first.
    pub fn set_sequential(&self, sequential: bool) {
        self.shared.runtime.set_sequential(sequential);
    }

    pub fn first_last_piece_priority(&self) -> bool {
        self.shared.runtime.first_last_piece_priority()
    }

    /// Give the first and last pieces (1% of the file, at least one piece at
    /// each end) of every wanted file the top priority.
    pub fn set_first_last_piece_priority(&self, enabled: bool) {
        if self.shared.runtime.set_first_last_piece_priority(enabled) == enabled {
            return;
        }
        let g = self.locked.read();
        if let ManagedTorrentState::Live(l) = &g.state
            && let Err(e) = l.update_file_priorities(&g.file_priorities, None)
        {
            tracing::warn!(
                id = self.shared.id,
                "error applying piece priorities: {e:#}"
            );
        }
    }

    pub fn super_seeding(&self) -> bool {
        self.shared.runtime.super_seeding()
    }

    /// BEP 16 super seeding. Only has an effect while we have the whole
    /// torrent, and only on connections made while it is on. Turning it off
    /// tells super-seeded peers about all pieces.
    pub fn set_super_seeding(&self, enabled: bool) {
        if self.shared.runtime.set_super_seeding(enabled) == enabled || enabled {
            return;
        }
        if let Some(live) = self.live() {
            live.superseed_disable();
        }
    }

    /// Upload slots of this torrent (`None` = unlimited).
    pub fn max_uploads(&self) -> Option<u32> {
        self.shared.runtime.max_uploads()
    }

    /// Change the upload slots of this torrent (`None` = unlimited); applied
    /// at the next rechoke, which is triggered right away.
    pub fn set_max_uploads(&self, max_uploads: Option<u32>) {
        self.shared.runtime.set_max_uploads(max_uploads);
        if let Some(session) = self.shared.session.upgrade() {
            session.peer_policy.request_rechoke();
        }
    }

    /// The connection limit of this torrent (`None` = unlimited).
    pub fn peer_limit(&self) -> Option<usize> {
        self.shared.runtime.peer_limit()
    }

    /// Change the connection limit (`None` = unlimited). Lowering it below the
    /// current number of connections disconnects the least useful peers.
    pub fn set_peer_limit(&self, limit: Option<usize>) {
        self.shared.runtime.set_peer_limit(limit);
        if let Some(live) = self.live() {
            live.set_peer_limit(limit);
        }
    }

    /// Per-torrent rate limits (bytes/s).
    pub fn ratelimits(&self) -> LimitsConfig {
        self.shared.runtime.ratelimits.get_config()
    }

    /// Change the per-torrent rate limits (bytes/s, `None` = unlimited).
    pub fn set_ratelimits(&self, limits: LimitsConfig) {
        let l = &self.shared.runtime.ratelimits;
        l.set_upload_bps(limits.upload_bps);
        l.set_download_bps(limits.download_bps);
    }

    /// Per-piece state: 0 missing, 1 downloading (requested right now), 2
    /// downloaded and verified. Empty without metadata or while checking.
    pub fn piece_states(&self) -> Vec<u8> {
        let g = self.locked.read();
        match &g.state {
            ManagedTorrentState::Live(l) => l.piece_states(),
            ManagedTorrentState::Paused(p) => {
                let have = p.chunk_tracker.get_have_pieces().as_slice();
                (0..self.total_pieces())
                    .map(|i| {
                        if have.get(i).is_some_and(|b| *b) {
                            PIECE_STATE_HAVE
                        } else {
                            PIECE_STATE_MISSING
                        }
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// Number of downloaded and verified pieces.
    pub fn pieces_have(&self) -> u32 {
        let total = self.total_pieces();
        let g = self.locked.read();
        match &g.state {
            ManagedTorrentState::Live(l) => l.pieces_have(),
            ManagedTorrentState::Paused(p) => {
                count_ones(p.chunk_tracker.get_have_pieces().as_bytes(), total)
            }
            _ => 0,
        }
    }

    /// How many connected peers have each piece. Empty when we have the whole
    /// torrent (as libtorrent does) or without metadata; all zeros when paused.
    pub fn piece_availability(&self) -> Vec<u32> {
        let total = self.total_pieces();
        let g = self.locked.read();
        match &g.state {
            ManagedTorrentState::Live(l) => l.piece_availability(),
            ManagedTorrentState::Paused(p) => {
                if p.hns().have_bytes >= p.chunk_tracker.get_lengths().total_length() {
                    Vec::new()
                } else {
                    vec![0; total]
                }
            }
            _ => Vec::new(),
        }
    }

    /// libtorrent's distributed copies: the lowest number of copies of any
    /// piece among connected peers and us, plus the fraction of pieces with
    /// more copies than that. -1 when we have the whole torrent or the value is
    /// unknown.
    pub fn distributed_copies(&self) -> f64 {
        let total = self.total_pieces();
        let g = self.locked.read();
        match &g.state {
            ManagedTorrentState::Live(l) => l.distributed_copies(),
            ManagedTorrentState::Paused(p) => {
                if p.hns().have_bytes >= p.chunk_tracker.get_lengths().total_length() {
                    return -1.0;
                }
                let have = p.chunk_tracker.get_have_pieces().as_slice();
                distributed_copies(total, |i| have.get(i).is_some_and(|b| *b), |_| 0)
            }
            _ => -1.0,
        }
    }

    /// Connected peers with flags, rates, progress and more. Empty unless live.
    pub fn peers_detailed(&self) -> Vec<PeerDetails> {
        self.live().map(|l| l.peers_detailed()).unwrap_or_default()
    }

    /// Number of connected (handshaken) peers.
    pub fn connections(&self) -> u32 {
        self.live()
            .map(|l| l.stats_snapshot().peer_stats.live)
            .unwrap_or(0)
    }

    /// Add peers to connect to (source "manual"). While the torrent isn't
    /// live, up to 500 are remembered and tried when it starts. Returns how
    /// many were new.
    pub fn add_peers(&self, addrs: Vec<SocketAddr>) -> usize {
        addrs
            .into_iter()
            .filter(|a| self.add_peer_with_source(*a, PeerSource::Manual))
            .count()
    }

    /// Add one peer with its source. Returns true if it was new.
    pub fn add_peer_with_source(&self, addr: SocketAddr, source: PeerSource) -> bool {
        match self.live() {
            Some(live) => live.add_peer_with_source(addr, source).unwrap_or(false),
            None => self.shared.runtime.queue_pending_peer(addr, source),
        }
    }

    /// Disconnect a peer. It isn't reconnected automatically (it can come
    /// back if it connects to us or is added again). Returns false if it
    /// wasn't connected.
    pub fn disconnect_peer(&self, addr: SocketAddr) -> bool {
        self.live()
            .map(|l| l.kick_peer(addr, true))
            .unwrap_or(false)
    }

    /// Bytes downloaded and thrown away since the torrent was added to the
    /// session: failed hash checks plus redundant blocks.
    pub fn wasted_bytes(&self) -> u64 {
        self.shared.runtime.wasted_bytes()
    }

    /// Like `update_only_files`, but in terms of priorities: files not in the
    /// set get priority 0, excluded files that are now wanted get the default.
    pub(crate) fn update_only_files(&self, only_files: &HashSet<usize>) -> anyhow::Result<()> {
        let file_count = self.file_count().context("torrent is not resolved")?;
        for f in only_files.iter().copied() {
            if f >= file_count {
                anyhow::bail!("only_files contains invalid value {f}")
            }
        }
        let mut priorities = self.file_priorities();
        priorities.resize(file_count, super::live::priorities::PRIORITY_DEFAULT);
        for (i, p) in priorities.iter_mut().enumerate() {
            if !only_files.contains(&i) {
                *p = PRIORITY_IGNORE;
            } else if *p == PRIORITY_IGNORE {
                *p = super::live::priorities::PRIORITY_DEFAULT;
            }
        }
        let mut g = self.locked.write();
        match &mut g.state {
            ManagedTorrentState::Paused(p) => p.update_only_files(only_files)?,
            ManagedTorrentState::Live(l) => {
                l.update_file_priorities(&priorities, Some(only_files))?
            }
            ManagedTorrentState::Initializing(_)
            | ManagedTorrentState::Error(_)
            | ManagedTorrentState::None => {}
        }
        let mut sorted: Vec<usize> = only_files.iter().copied().collect();
        sorted.sort_unstable();
        g.only_files = Some(sorted);
        g.file_priorities = priorities;
        drop(g);
        self.on_files_wanted_changed(|idx| only_files.contains(&idx));
        Ok(())
    }
}
