//! Which of our pieces a peer has been told about (BITFIELD + HAVE).
//!
//! New pieces reach each connection's writer through a bounded broadcast
//! channel. A writer that falls more than the channel's capacity behind (a
//! fast download completing pieces while the socket is slow) loses the oldest
//! entries. A BITFIELD may not be sent again mid-connection, so the writer
//! instead compares the torrent's have bitfield with what it advertised and
//! sends a HAVE for each difference.
//!
//! Memory: one bit per piece per connection, `ceil(pieces / 8)` bytes, e.g.
//! 250 B for 2000 pieces and 2.5 KiB for 20 000 pieces, freed with the
//! connection. Nothing is allocated when we already had every piece when the
//! connection started (seeding): no HAVE can follow then. The alternative, a
//! per-torrent append-only log of completed pieces with a cursor per peer,
//! costs 4 B per completed piece for as long as the torrent runs (it can only
//! be trimmed once every connection read past an entry) and is cheaper only
//! above ~32 peers on one downloading torrent.

use crate::type_aliases::{BF, BS};

#[derive(Debug, Default)]
pub(crate) struct AdvertisedHaves {
    /// One bit per piece; `None` when no HAVE can ever be needed.
    bits: Option<BF>,
    /// Lagged behind the broadcast: compare with the have bitfield from this
    /// piece index on.
    resync_from: Option<usize>,
}

impl AdvertisedHaves {
    /// The peer was sent `have` as its BITFIELD (or, if nothing was sent, an
    /// empty `have` of the torrent's size).
    pub fn init(&mut self, have: &BS, bitfield_sent: bool) {
        if bitfield_sent && have.all() {
            self.bits = None;
            return;
        }
        let mut bits = BF::from_bitslice(have);
        if !bitfield_sent {
            bits.fill(false);
        }
        self.bits = Some(bits);
    }

    /// A HAVE for `index` arrived through the broadcast: true if it must be
    /// sent (it is then recorded as sent).
    pub fn mark(&mut self, index: u32) -> bool {
        let Some(bits) = self.bits.as_mut() else {
            return true;
        };
        match bits.get_mut(index as usize) {
            Some(mut bit) => !std::mem::replace(&mut *bit, true),
            None => true,
        }
    }

    /// The broadcast dropped entries: send what the peer misses.
    pub fn start_resync(&mut self) {
        if self.bits.is_some() {
            self.resync_from = Some(0);
        }
    }

    pub fn cancel_resync(&mut self) {
        self.resync_from = None;
    }

    pub fn is_resyncing(&self) -> bool {
        self.resync_from.is_some()
    }

    /// Hands every piece of `have` the peer was not told about to `emit`, in
    /// index order, marking it as sent when `emit` returns true. Stops when
    /// `emit` returns false (no room); the next call goes on from there.
    pub fn resync_step(&mut self, have: &BS, emit: &mut dyn FnMut(u32) -> bool) {
        let (Some(bits), Some(from)) = (self.bits.as_mut(), self.resync_from) else {
            self.resync_from = None;
            return;
        };
        let end = have.len().min(bits.len());
        let from = from.min(end);
        for index in have[from..end].iter_ones().map(|i| i + from) {
            if bits[index] {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            if !emit(index as u32) {
                self.resync_from = Some(index);
                return;
            }
            bits.set(index, true);
        }
        self.resync_from = None;
    }

    #[cfg(test)]
    pub fn heap_bytes(&self) -> usize {
        self.bits.as_ref().map_or(0, |b| b.as_raw_slice().len())
    }
}

#[cfg(test)]
mod tests {
    use super::AdvertisedHaves;
    use crate::type_aliases::BF;

    fn have(n: usize, set: impl IntoIterator<Item = usize>) -> BF {
        let mut b = bitvec::bitvec![u8, bitvec::order::Msb0; 0; n].into_boxed_bitslice();
        for i in set {
            b.set(i, true);
        }
        b
    }

    #[test]
    fn bitfield_pieces_are_not_announced_again() {
        let mut a = AdvertisedHaves::default();
        a.init(&have(16, [1, 2]), true);
        assert!(!a.mark(1));
        assert!(a.mark(3));
        assert!(!a.mark(3));
    }

    #[test]
    fn nothing_allocated_for_a_seed() {
        let mut a = AdvertisedHaves::default();
        a.init(&have(16, 0..16), true);
        assert_eq!(a.heap_bytes(), 0);
        a.start_resync();
        assert!(!a.is_resyncing());
        let mut a = AdvertisedHaves::default();
        a.init(&have(20_000, [0]), true);
        assert_eq!(a.heap_bytes(), 2500);
    }

    #[test]
    fn no_bitfield_means_nothing_advertised() {
        let mut a = AdvertisedHaves::default();
        a.init(&have(8, [0, 1]), false);
        a.start_resync();
        let mut sent = vec![];
        a.resync_step(&have(8, [0, 1, 5]), &mut |i| {
            sent.push(i);
            true
        });
        assert_eq!(sent, [0, 1, 5]);
    }

    /// 300 pieces complete while the writer is stuck: the broadcast (128
    /// slots) keeps only the newest ones. The resync plus the broadcast
    /// remainder must announce each piece exactly once, also when the
    /// writer's buffer only takes a few HAVEs at a time.
    #[test]
    fn lag_resync_announces_every_piece_once() {
        let n = 1000;
        let mut a = AdvertisedHaves::default();
        a.init(&have(n, 0..100), true);

        let completed: Vec<u32> = (100..400).collect();
        let now_have = have(n, (0..100).chain(completed.iter().map(|&p| p as usize)));
        let (tx, mut rx) = tokio::sync::broadcast::channel::<u32>(128);
        for &p in &completed {
            tx.send(p).unwrap();
        }

        let mut sent = vec![];
        loop {
            match rx.try_recv() {
                Ok(i) => {
                    if a.mark(i) {
                        sent.push(i);
                    }
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    a.start_resync();
                    while a.is_resyncing() {
                        // Room for 7 HAVEs per batch.
                        let mut room = 7;
                        a.resync_step(&now_have, &mut |i| {
                            if room == 0 {
                                return false;
                            }
                            room -= 1;
                            sent.push(i);
                            true
                        });
                    }
                }
                Err(_) => break,
            }
        }
        sent.sort_unstable();
        assert_eq!(sent, completed);
    }
}
