//! Per-piece availability (how many connected peers have each piece) and
//! small bitfield helpers.
//!
//! Counts are lock-free `u16` atomics (2 bytes per piece) updated whenever a
//! live peer's bitfield changes or the peer leaves the live state, so readers
//! (piece picker, super seeding, UI) never need the peer table locks.

use std::sync::atomic::{AtomicU16, Ordering};

use crate::type_aliases::BS;

pub(crate) struct PieceAvailability {
    counts: Box<[AtomicU16]>,
}

impl Default for PieceAvailability {
    fn default() -> Self {
        Self::new(0)
    }
}

impl PieceAvailability {
    pub fn new(total_pieces: usize) -> Self {
        Self {
            counts: (0..total_pieces).map(|_| AtomicU16::new(0)).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.counts.len()
    }

    pub fn get(&self, piece: usize) -> u16 {
        self.counts
            .get(piece)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn inc(&self, piece: usize) {
        if let Some(c) = self.counts.get(piece) {
            let _ = c.try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1));
        }
    }

    pub fn dec(&self, piece: usize) {
        if let Some(c) = self.counts.get(piece) {
            let _ = c.try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_sub(1));
        }
    }

    pub fn add_bitfield(&self, bf: &BS) {
        let len = self.len().min(bf.len());
        for piece in bf[..len].iter_ones() {
            self.inc(piece);
        }
    }

    pub fn remove_bitfield(&self, bf: &BS) {
        let len = self.len().min(bf.len());
        for piece in bf[..len].iter_ones() {
            self.dec(piece);
        }
    }

    pub fn snapshot(&self) -> Vec<u32> {
        self.counts
            .iter()
            .map(|c| u32::from(c.load(Ordering::Relaxed)))
            .collect()
    }
}

/// Bit mask of the valid bits in the last byte of an MSB-first bitfield of
/// `total` bits.
fn last_byte_mask(total: usize) -> u8 {
    match total % 8 {
        0 => 0xff,
        rem => 0xffu8 << (8 - rem),
    }
}

/// Number of pieces (among the first `total`) set in an MSB-first raw bitfield.
pub(crate) fn count_ones(raw: &[u8], total: usize) -> u32 {
    let nbytes = total.div_ceil(8).min(raw.len());
    let mut n = 0u32;
    for (i, b) in raw[..nbytes].iter().enumerate() {
        let b = if i + 1 == total.div_ceil(8) {
            b & last_byte_mask(total)
        } else {
            *b
        };
        n += b.count_ones();
    }
    n
}

/// Number of pieces (among the first `total`) the peer has and we don't.
pub(crate) fn count_peer_has_we_lack(peer: &[u8], have: &[u8], total: usize) -> u32 {
    let nbytes = total.div_ceil(8);
    let mut n = 0u32;
    for i in 0..nbytes {
        let p = peer.get(i).copied().unwrap_or(0);
        let h = have.get(i).copied().unwrap_or(0);
        let mut b = p & !h;
        if i + 1 == nbytes {
            b &= last_byte_mask(total);
        }
        n += b.count_ones();
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::type_aliases::BF;

    #[test]
    fn availability_counts_bitfields() {
        let a = PieceAvailability::new(10);
        let mut bf = BF::from_boxed_slice(vec![0u8; 2].into_boxed_slice());
        bf.set(1, true);
        bf.set(9, true);
        // Bits past the torrent end are ignored.
        bf.set(12, true);
        a.add_bitfield(&bf);
        a.add_bitfield(&bf);
        assert_eq!(a.get(1), 2);
        assert_eq!(a.get(9), 2);
        assert_eq!(a.get(0), 0);
        a.remove_bitfield(&bf);
        assert_eq!(a.snapshot(), vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 1]);
        // Never underflows.
        a.dec(0);
        assert_eq!(a.get(0), 0);
        // Out of range is ignored.
        a.inc(100);
    }

    #[test]
    fn raw_bit_counting() {
        // 10 pieces: peer has 0,1,2,9 and garbage in the spare bits.
        let peer = [0b1110_0000, 0b0111_1111];
        let have = [0b0100_0000, 0b0000_0000];
        assert_eq!(count_ones(&peer, 10), 4);
        assert_eq!(count_peer_has_we_lack(&peer, &have, 10), 3);
        assert_eq!(count_ones(&[0xff], 8), 8);
        assert_eq!(count_peer_has_we_lack(&[], &have, 10), 0);
    }
}
