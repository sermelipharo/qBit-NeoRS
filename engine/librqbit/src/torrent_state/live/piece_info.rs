//! Piece priorities and piece state export of a live torrent.

use std::collections::HashSet;

use super::{
    TorrentStateLive,
    availability::count_ones,
    priorities::{
        PIECE_STATE_DOWNLOADING, PIECE_STATE_HAVE, PIECE_STATE_MISSING, compute_piece_priorities,
        distributed_copies,
    },
};

impl TorrentStateLive {
    pub(crate) fn compute_piece_priorities(&self, file_priorities: &[u8]) -> Box<[u8]> {
        compute_piece_priorities(
            self.lengths.total_pieces() as usize,
            self.lengths.default_piece_length(),
            &self.metadata.file_infos,
            file_priorities,
            self.shared.runtime.first_last_piece_priority(),
        )
    }

    /// Apply new file priorities. `only_files` (the files with priority > 0)
    /// is given when the set of wanted files changed.
    pub(crate) fn update_file_priorities(
        &self,
        file_priorities: &[u8],
        only_files: Option<&HashSet<usize>>,
    ) -> anyhow::Result<()> {
        let piece_priorities = self.compute_piece_priorities(file_priorities);
        let hns = {
            let mut g = self.lock_write("update_file_priorities");
            g.piece_priorities = piece_priorities;
            match only_files {
                Some(only_files) => Some(
                    g.get_pieces_mut()?
                        .update_only_files(&self.metadata.file_infos, only_files)?,
                ),
                None => None,
            }
        };
        // With the state lock released. A dying peer holds its shard of the peer table
        // while it asks whether the torrent is finished (on_peer_died), so touching the
        // table under the state lock is the reverse order, and the two deadlock.
        if let Some(hns) = hns
            && !hns.finished()
        {
            self.reconnect_all_not_needed_peers();
            // Downloading again: the DHT lookups may need to run sooner.
            self.shared.tracker_manager.dht_pacing.wake();
        }
        self.new_pieces_notify.notify_waiters();
        Ok(())
    }

    /// 0 = missing, 1 = downloading (in flight), 2 = have, for every piece.
    pub(crate) fn piece_states(&self) -> Vec<u8> {
        let total = self.lengths.total_pieces() as usize;
        let g = self.lock_read("piece_states");
        let Ok(pieces) = g.get_pieces() else {
            return Vec::new();
        };
        let have = pieces.chunks().get_have_pieces().as_slice();
        let mut out: Vec<u8> = (0..total)
            .map(|i| {
                if have.get(i).is_some_and(|b| *b) {
                    PIECE_STATE_HAVE
                } else {
                    PIECE_STATE_MISSING
                }
            })
            .collect();
        for p in pieces.inflight_pieces() {
            if let Some(s) = out.get_mut(p.get_usize())
                && *s == PIECE_STATE_MISSING
            {
                *s = PIECE_STATE_DOWNLOADING;
            }
        }
        out
    }

    pub(crate) fn pieces_have(&self) -> u32 {
        let total = self.lengths.total_pieces() as usize;
        self.lock_read("pieces_have")
            .get_chunks()
            .map(|c| count_ones(c.get_have_pieces().as_bytes(), total))
            .unwrap_or_default()
    }

    /// Connected peers having each piece; empty when we are a seed
    /// (libtorrent semantics).
    pub(crate) fn piece_availability(&self) -> Vec<u32> {
        if self.is_seed() {
            return Vec::new();
        }
        self.peers.availability.snapshot()
    }

    /// libtorrent `distributed_copies`, -1 when we are a seed.
    pub(crate) fn distributed_copies(&self) -> f64 {
        if self.is_seed() {
            return -1.0;
        }
        let total = self.lengths.total_pieces() as usize;
        let have: Vec<u8> = match self.lock_read("distributed_copies").get_chunks() {
            Ok(c) => c.get_have_pieces().as_bytes().to_vec(),
            Err(_) => return -1.0,
        };
        let has = |i: usize| have.get(i / 8).is_some_and(|b| b & (0x80 >> (i % 8)) != 0);
        distributed_copies(total, has, |i| u32::from(self.peers.availability.get(i)))
    }
}
