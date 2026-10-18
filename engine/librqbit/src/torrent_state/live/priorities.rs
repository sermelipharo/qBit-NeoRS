//! Download priorities and piece picking.
//!
//! File priorities use libtorrent levels `0..=7` (`0` = don't download). They
//! are turned into a compact per-piece priority array (1 byte per piece): a
//! piece shared by several files takes the highest priority among them, and
//! with "first/last piece priority" the head and tail of every wanted file get
//! the top priority.
//!
//! The picker takes the highest priority piece first. Among equal priorities
//! it is rarest-first (lowest availability among connected peers) with a
//! per-torrent pseudo-random tie-break, or lowest index first in sequential
//! mode.

use crate::type_aliases::{BS, FileInfos};

/// "Do not download".
pub const PRIORITY_IGNORE: u8 = 0;
/// The default priority of every file (qBittorrent's "Normal").
pub const PRIORITY_DEFAULT: u8 = 1;
/// The top priority (qBittorrent's "Maximum").
pub const PRIORITY_MAX: u8 = 7;

/// Piece state as returned by `ManagedTorrent::piece_states`: not downloaded.
pub const PIECE_STATE_MISSING: u8 = 0;
/// Piece state: being downloaded (requested from a peer right now).
pub const PIECE_STATE_DOWNLOADING: u8 = 1;
/// Piece state: downloaded and hash-checked.
pub const PIECE_STATE_HAVE: u8 = 2;

/// Clamps priorities to `0..=7` and pads/truncates to `file_count` entries
/// (missing entries get the default priority).
pub(crate) fn normalize_file_priorities(mut prios: Vec<u8>, file_count: usize) -> Vec<u8> {
    prios.resize(file_count, PRIORITY_DEFAULT);
    for p in prios.iter_mut() {
        *p = (*p).min(PRIORITY_MAX);
    }
    prios
}

/// File priorities matching an `only_files` selection (`None` = all files).
pub(crate) fn file_priorities_from_only_files(
    file_count: usize,
    only_files: Option<&[usize]>,
) -> Vec<u8> {
    (0..file_count)
        .map(|i| match only_files {
            Some(of) if !of.contains(&i) => PRIORITY_IGNORE,
            _ => PRIORITY_DEFAULT,
        })
        .collect()
}

/// How many pieces at each end of a file get the top priority with
/// first/last piece priority on: 1% of the file, at least one piece
/// (qBittorrent's rule, which covers e.g. AVI indexes at the file end).
fn first_last_piece_count(file_len: u64, piece_length: u32) -> usize {
    if file_len == 0 || piece_length == 0 {
        return 0;
    }
    let n = (file_len as f64 * 0.01 / f64::from(piece_length)).ceil();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = n as usize;
    n.max(1)
}

/// Computes the per-piece priority array from file priorities.
pub(crate) fn compute_piece_priorities(
    total_pieces: usize,
    piece_length: u32,
    file_infos: &FileInfos,
    file_priorities: &[u8],
    first_last: bool,
) -> Box<[u8]> {
    let mut out = vec![PRIORITY_IGNORE; total_pieces].into_boxed_slice();
    let wanted = || {
        file_infos
            .iter()
            .zip(file_priorities.iter().copied())
            .filter(|(fi, prio)| !fi.attrs.padding && *prio > PRIORITY_IGNORE && fi.len > 0)
    };
    for (fi, prio) in wanted() {
        if let Some(r) = out.get_mut(fi.piece_range_usize()) {
            for p in r.iter_mut() {
                *p = (*p).max(prio.min(PRIORITY_MAX));
            }
        }
    }
    if first_last {
        for (fi, _) in wanted() {
            let range = fi.piece_range_usize();
            let n = first_last_piece_count(fi.len, piece_length).min(range.len());
            let head = range.start..range.start + n;
            let tail = range.end - n..range.end;
            for p in head.chain(tail) {
                if let Some(p) = out.get_mut(p) {
                    *p = PRIORITY_MAX;
                }
            }
        }
    }
    out
}

/// A cheap bijective scramble of the piece index, used as a per-torrent
/// pseudo-random tie-break so that different clients pick different pieces
/// among equally rare ones.
fn scramble(index: usize, seed: u32) -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    let mut h = (index as u32) ^ seed;
    // MurmurHash3's finalizer: a bijection that mixes every input bit.
    h ^= h >> 16;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}

/// Inputs of [`pick_piece`].
pub(crate) struct PickRequest<'a, H, A>
where
    H: Fn(usize) -> bool,
    A: Fn(usize) -> u16,
{
    /// Pieces that are wanted and neither downloaded nor in flight.
    pub queue: &'a BS,
    pub piece_priorities: &'a [u8],
    /// Whether the peer has the piece.
    pub peer_has: H,
    /// How many connected peers have the piece.
    pub availability: A,
    pub sequential: bool,
    pub seed: u32,
}

/// Picks the next piece to download from a peer, or `None`.
pub(crate) fn pick_piece<H, A>(req: PickRequest<'_, H, A>) -> Option<usize>
where
    H: Fn(usize) -> bool,
    A: Fn(usize) -> u16,
{
    let mut best: Option<(u64, usize)> = None;
    for piece in req.queue.iter_ones() {
        if !(req.peer_has)(piece) {
            continue;
        }
        let prio = u64::from(req.piece_priorities.get(piece).copied().unwrap_or(0));
        let score = if req.sequential {
            // Index order: the first top-priority piece can't be beaten.
            if prio == u64::from(PRIORITY_MAX) {
                return Some(piece);
            }
            (prio << 48) | (0xFFFF_FFFF_FFFF - piece as u64)
        } else {
            let avail = u64::from((req.availability)(piece));
            (prio << 48)
                | ((0xFFFF - avail.min(0xFFFF)) << 32)
                | u64::from(u32::MAX - scramble(piece, req.seed))
        };
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, piece));
        }
    }
    best.map(|(_, p)| p)
}

/// libtorrent's `distributed_copies`: the lowest availability over all
/// pieces (counting our own copy) plus the fraction (in thousandths) of
/// pieces that are more available than that.
pub(crate) fn distributed_copies(
    total_pieces: usize,
    have: impl Fn(usize) -> bool,
    availability: impl Fn(usize) -> u32,
) -> f64 {
    if total_pieces == 0 {
        return 1.0;
    }
    let mut min = u32::MAX;
    let mut integer_part = 0usize;
    let mut fraction_part = 0usize;
    for piece in 0..total_pieces {
        let count = availability(piece) + u32::from(have(piece));
        match count.cmp(&min) {
            std::cmp::Ordering::Less => {
                min = count;
                fraction_part += integer_part;
                integer_part = 1;
            }
            std::cmp::Ordering::Equal => integer_part += 1,
            std::cmp::Ordering::Greater => fraction_part += 1,
        }
    }
    let thousandths = fraction_part * 1000 / total_pieces;
    f64::from(min) + thousandths as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{file_info::FileInfo, type_aliases::BF};

    fn file(start: u32, end: u32, len: u64) -> FileInfo {
        FileInfo {
            relative_filename: "f".into(),
            offset_in_torrent: 0,
            piece_range: start..end,
            attrs: Default::default(),
            len,
        }
    }

    fn queue(n: usize) -> BF {
        let mut bf = BF::from_boxed_slice(vec![0u8; n.div_ceil(8)].into_boxed_slice());
        for i in 0..n {
            bf.set(i, true);
        }
        bf
    }

    #[test]
    fn piece_priorities_take_max_of_shared_files() {
        // 3 files over 6 pieces; piece 2 is shared by files 0 and 1,
        // piece 4 by files 1 and 2.
        let files = vec![file(0, 3, 300), file(2, 5, 300), file(4, 6, 200)];
        let p = compute_piece_priorities(6, 100, &files, &[6, 0, 1], false);
        assert_eq!(&*p, &[6, 6, 6, 0, 1, 1]);
        let p = compute_piece_priorities(6, 100, &files, &[0, 0, 0], false);
        assert_eq!(&*p, &[0; 6]);
        let p = compute_piece_priorities(6, 100, &files, &[1, 7, 6], false);
        assert_eq!(&*p, &[1, 1, 7, 7, 7, 6]);
    }

    #[test]
    fn first_last_piece_priority() {
        // One file of 10 pieces: first and last piece get top priority.
        let files = vec![file(0, 10, 1000)];
        let p = compute_piece_priorities(10, 100, &files, &[1], true);
        assert_eq!(&*p, &[7, 1, 1, 1, 1, 1, 1, 1, 1, 7]);
        // 1% of a 1000-piece file = 10 pieces at each end.
        let files = vec![file(0, 1000, 100_000)];
        let p = compute_piece_priorities(1000, 100, &files, &[6], true);
        assert!(p[..10].iter().all(|p| *p == 7));
        assert_eq!(p[10], 6);
        assert!(p[990..].iter().all(|p| *p == 7));
        assert_eq!(p[989], 6);
        // Ignored files are untouched.
        let files = vec![file(0, 5, 500), file(5, 10, 500)];
        let p = compute_piece_priorities(10, 100, &files, &[0, 1], true);
        assert_eq!(&*p, &[0, 0, 0, 0, 0, 7, 1, 1, 1, 7]);
    }

    #[test]
    fn picker_prefers_priority_then_rarity() {
        let q = queue(6);
        let prios = [1, 1, 6, 6, 1, 7];
        let avail = [1u16, 1, 5, 2, 1, 9];
        let pick = |q: &BS, has: &dyn Fn(usize) -> bool, seq: bool| {
            pick_piece(PickRequest {
                queue: q,
                piece_priorities: &prios,
                peer_has: has,
                availability: |p| avail[p],
                sequential: seq,
                seed: 12345,
            })
        };
        // Highest priority first, even if very common.
        assert_eq!(pick(&q, &|_| true, false), Some(5));
        // Then the rarer of the two priority-6 pieces.
        assert_eq!(pick(&q, &|p| p != 5, false), Some(3));
        // Only pieces the peer has.
        assert_eq!(
            pick(&q, &|p| p == 0 || p == 4, false).map(|p| p == 0 || p == 4),
            Some(true)
        );
        assert_eq!(pick(&q, &|_| false, false), None);
        // Sequential: priority, then index order.
        assert_eq!(pick(&q, &|_| true, true), Some(5));
        assert_eq!(pick(&q, &|p| p != 5, true), Some(2));
        assert_eq!(pick(&q, &|p| p < 2 || p == 4, true), Some(0));

        // Only queued pieces are considered.
        let mut q2 = queue(6);
        q2.set(5, false);
        q2.set(2, false);
        assert_eq!(pick(&q2, &|_| true, true), Some(3));
    }

    #[test]
    fn picker_tie_break_spreads_choices() {
        let q = queue(64);
        let prios = [1u8; 64];
        let first = |seed| {
            pick_piece(PickRequest {
                queue: &q,
                piece_priorities: &prios,
                peer_has: |_| true,
                availability: |_| 1,
                sequential: false,
                seed,
            })
            .unwrap()
        };
        let picks: std::collections::HashSet<usize> = (0..16u32).map(|s| first(s * 7919)).collect();
        assert!(picks.len() > 4, "{picks:?}");
    }

    #[test]
    fn distributed_copies_matches_libtorrent() {
        // 4 pieces, we have none: availability 2,2,3,4 -> 2 + 2/4.
        let avail = [2u32, 2, 3, 4];
        assert_eq!(distributed_copies(4, |_| false, |p| avail[p]), 2.5);
        // Our own copy counts.
        assert_eq!(distributed_copies(4, |p| p < 2, |p| avail[p]), 3.25);
        // A missing piece nobody has: 0 copies + fraction.
        let avail = [0u32, 1, 1];
        assert_eq!(distributed_copies(3, |_| false, |p| avail[p]), 0.666);
        assert_eq!(distributed_copies(3, |_| false, |_| 1), 1.0);
    }
}
