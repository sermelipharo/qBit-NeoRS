//! Pure v2 torrents whose .torrent lacks the piece layers of some files.
//!
//! Like libtorrent, such a torrent starts at once: the files whose layer is
//! known (and files no longer than a piece, verified by their `pieces
//! root`) download and seed as usual while the missing layers are fetched
//! from peers (`live::v2_hashes`). The pieces of a file without a layer
//! can't be verified, so they are never requested (the picker skips them
//! as if unwanted, user priorities untouched) and never enter the have
//! bitfield. Data already on disk for them (a resumed or rechecked
//! torrent, a seed added from an incomplete .torrent) is remembered and
//! hash-checked as soon as the layer arrives.

use buffers::ByteBufOwned;
use librqbit_core::torrent_metainfo::ValidatedTorrentMetaV1Info;

use crate::type_aliases::{BF, BS};

/// The pieces of the files whose layer isn't known yet; `None` when every
/// piece can be verified (v1, hybrid and complete pure v2 torrents).
pub(crate) fn unknown_pieces(info: &ValidatedTorrentMetaV1Info<ByteBufOwned>) -> Option<BF> {
    let layout = info.v2_layout()?;
    let hashes = info.v2_piece_hashes()?;
    if hashes.is_complete() {
        return None;
    }
    let mut bf = BF::from_boxed_slice(
        vec![0u8; info.lengths().piece_bitfield_bytes()].into_boxed_slice(),
    );
    for (i, s) in layout.spans.iter().enumerate() {
        if !hashes.span_known(i) {
            let start = s.first_piece as usize;
            if let Some(r) = bf.get_mut(start..start + s.num_pieces as usize) {
                r.fill(true);
            }
        }
    }
    Some(bf)
}

/// Takes the `unknown` pieces out of `have` (they can't have been
/// verified); returns those that were set (data assumed on disk).
pub(crate) fn take_unknown(have: &mut BS, unknown: &BS) -> BF {
    let mut taken = BF::from_boxed_slice(vec![0u8; have.len().div_ceil(8)].into_boxed_slice());
    for i in unknown.iter_ones() {
        if have.get(i).is_some_and(|b| *b) {
            have.set(i, false);
            taken.set(i, true);
        }
    }
    taken
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_pieces_leave_the_have_bitfield() {
        let mut have = BF::from_boxed_slice(vec![0b1111_0110u8, 0b1000_0000].into_boxed_slice());
        let unknown = BF::from_boxed_slice(vec![0b0011_1100u8, 0b1000_0000].into_boxed_slice());
        let taken = take_unknown(&mut have, &unknown);
        assert_eq!(have.as_raw_slice(), &[0b1100_0010, 0]);
        assert_eq!(taken.as_raw_slice(), &[0b0011_0100, 0b1000_0000]);
    }
}
