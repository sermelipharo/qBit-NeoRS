//! A live pure v2 torrent whose .torrent lacked some piece layers (see
//! `torrent_state::missing_layers`): files are handed to the picker as their
//! layers become known, after the data already on disk for them is checked.

use std::{ops::Range, sync::Arc, sync::atomic::Ordering};

use librqbit_core::{lengths::ValidPieceIndex, v2::V2FileSpan};
use tracing::{debug, debug_span};

use super::TorrentStateLive;
use crate::type_aliases::BF;

impl TorrentStateLive {
    /// Whether the file of `span` is wanted (priority above 0): its layer
    /// is fetched before those of unwanted files.
    pub(crate) fn v2_file_wanted(&self, span: &V2FileSpan) -> bool {
        self.lock_read("v2_file_wanted")
            .piece_priorities
            .get(span.first_piece as usize)
            .is_some_and(|p| *p > 0)
    }

    /// Pieces still held back at start although their layer is known (it
    /// arrived while the torrent wasn't live): those with unverified data
    /// on disk, checked by [`Self::check_and_unblock`].
    pub(crate) fn v2_known_unverified(&self) -> Option<BF> {
        let g = self.shared.v2_unverified.lock();
        let u = g.as_ref()?;
        let mut out = BF::from_boxed_slice(vec![0u8; u.as_raw_slice().len()].into_boxed_slice());
        let mut any = false;
        for p in u.iter_ones() {
            #[allow(clippy::cast_possible_truncation)]
            if self.metadata.info.piece_hash_known(p as u32) {
                out.set(p, true);
                any = true;
            }
        }
        any.then_some(out)
    }

    /// The layers of the files `spans` (indexes into the layout's spans)
    /// became known: their pieces can be verified now.
    pub(crate) fn on_v2_layers_known(self: &Arc<Self>, spans: Vec<usize>) {
        self.shared.v2_layers_unsaved.store(true, Ordering::Release);
        let Some(layout) = self.metadata.info.v2_layout() else {
            return;
        };
        let ranges = spans
            .iter()
            .filter_map(|&i| layout.spans.get(i))
            .map(|s| s.first_piece as usize..(s.first_piece + s.num_pieces) as usize)
            .collect();
        self.check_and_unblock(ranges);
    }

    /// Hash-checks the pieces in `ranges` that have unverified data on disk
    /// (off the async threads, in a task of its own that ends with the
    /// check or the torrent), then lets the picker have every piece in
    /// `ranges`: those that matched are had, the others are downloaded.
    pub(crate) fn check_and_unblock(self: &Arc<Self>, ranges: Vec<Range<usize>>) {
        let to_check: Vec<ValidPieceIndex> = {
            let g = self.shared.v2_unverified.lock();
            match g.as_ref() {
                Some(u) => ranges
                    .iter()
                    .flat_map(|r| r.clone())
                    .filter(|&p| u.get(p).is_some_and(|b| *b))
                    .filter_map(|p| self.lengths.validate_piece_index(u32::try_from(p).ok()?))
                    .collect(),
                None => Vec::new(),
            }
        };
        if to_check.is_empty() {
            self.unblock(&ranges, &[]);
            return;
        }
        let state = self.clone();
        self.spawn(
            debug_span!(parent: self.shared.span.clone(), "v2_check_unverified"),
            format!("[{}]v2_check_unverified", self.shared.id),
            async move {
                let s = state.clone();
                let ok = state
                    .shared
                    .spawner
                    .block_in_place_with_semaphore(move || {
                        let fo = s.file_ops();
                        to_check
                            .into_iter()
                            .filter(|p| matches!(fo.check_piece(*p), Ok(true)))
                            .collect::<Vec<_>>()
                    })
                    .await;
                debug!(
                    id = state.shared.id,
                    pieces = ok.len(),
                    "pieces found on disk once their layer was known"
                );
                state.unblock(&ranges, &ok);
                Ok(())
            },
        );
    }

    /// Hands the pieces in `ranges` to the picker; `verified` (checked from
    /// disk) are had already.
    fn unblock(&self, ranges: &[Range<usize>], verified: &[ValidPieceIndex]) {
        {
            let mut u = self.shared.v2_unverified.lock();
            if let Some(bits) = u.as_mut() {
                for r in ranges {
                    if let Some(s) = bits.get_mut(r.clone()) {
                        s.fill(false);
                    }
                }
                if bits.not_any() {
                    *u = None;
                }
            }
        }
        let mut newly_had = Vec::with_capacity(verified.len());
        {
            let mut g = self.lock_write("v2_unblock");
            if let Some(b) = g.v2_blocked.as_mut() {
                for r in ranges {
                    if let Some(s) = b.get_mut(r.clone()) {
                        s.fill(false);
                    }
                }
                if b.not_any() {
                    g.v2_blocked = None;
                }
            }
            if let Ok(pieces) = g.get_pieces_mut() {
                for &p in verified {
                    if !pieces.chunks().is_piece_have(p) {
                        pieces.mark_piece_hash_ok(p, &self.metadata.file_infos);
                        newly_had.push(p);
                    }
                }
            }
        }
        for p in newly_had {
            let len = u64::from(self.lengths.piece_length(p));
            self.stats.have_bytes.fetch_add(len, Ordering::Relaxed);
            if let Err(e) = self.on_piece_completed(p) {
                debug!(id = self.shared.id, "error completing piece {p}: {e:#}");
            }
            self.transmit_haves(p);
        }
        self.new_pieces_notify.notify_waiters();
    }
}
