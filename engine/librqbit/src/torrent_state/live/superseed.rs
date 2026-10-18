//! Super seeding (BEP 16).
//!
//! When super seeding is on and we have the whole torrent, new connections
//! don't get our bitfield. Instead each peer is told (HAVE) about one piece at
//! a time: the rarest piece it doesn't have, avoiding pieces already revealed
//! to other peers. When the peer announces that it got a revealed piece (i.e.
//! the piece propagated to it), it is shown the next one. Each peer may
//! request only the (up to two) pieces revealed to it, which libtorrent does
//! too. Connections made before super seeding was enabled are unaffected.

use std::{collections::HashSet, sync::atomic::Ordering};

use librqbit_core::lengths::ValidPieceIndex;
use peer_binary_protocol::Message;
use rand::RngExt;

use crate::{peer_connection::WriterRequest, type_aliases::PeerHandle};

use super::{TorrentStateLive, peer::LivePeerState};

/// The revealed pieces of a super-seeded peer, newest first.
pub(crate) type SuperseedPieces = [Option<ValidPieceIndex>; 2];

pub(crate) fn superseed_allows(live: &LivePeerState, piece: ValidPieceIndex) -> bool {
    !live.super_seeding || live.superseed_pieces.contains(&Some(piece))
}

impl TorrentStateLive {
    /// We have every piece of the torrent.
    pub(crate) fn is_seed(&self) -> bool {
        self.stats.have_bytes.load(Ordering::Relaxed) >= self.lengths.total_length()
    }

    /// Super seeding is enabled and we are a seed.
    pub(crate) fn super_seeding_active(&self) -> bool {
        self.shared.runtime.super_seeding() && self.is_seed()
    }

    /// Reveal a new piece to a super-seeded peer, replacing `replace` (a piece
    /// the peer now has) if given.
    pub(crate) fn superseed_offer(&self, addr: PeerHandle, replace: Option<ValidPieceIndex>) {
        let total = self.lengths.total_pieces() as usize;
        let offered_elsewhere: HashSet<u32> = self
            .peers
            .states
            .iter()
            .filter(|e| *e.key() != addr)
            .filter_map(|e| e.value().get_live().map(|l| l.superseed_pieces))
            .flat_map(|p| p.into_iter().flatten().map(|p| p.get()))
            .collect();

        let choice = self
            .peers
            .with_live(addr, |live| {
                if !live.super_seeding {
                    return None;
                }
                let mut rng = rand::rng();
                let mut best: Option<usize> = None;
                let mut best_score = u32::MAX;
                let mut ties = 0u32;
                for piece in 0..total {
                    if live.bitfield.get(piece).is_some_and(|b| *b) {
                        continue;
                    }
                    #[allow(clippy::cast_possible_truncation)]
                    let idx = piece as u32;
                    if live
                        .superseed_pieces
                        .iter()
                        .flatten()
                        .any(|p| p.get() == idx)
                    {
                        continue;
                    }
                    let mut score = u32::from(self.peers.availability.get(piece));
                    // Avoid revealing the same piece to two peers when possible.
                    if offered_elsewhere.contains(&idx) {
                        score += 999;
                    }
                    if score < best_score {
                        best_score = score;
                        best = Some(piece);
                        ties = 1;
                    } else if score == best_score {
                        ties += 1;
                        if rng.random_range(0..ties) == 0 {
                            best = Some(piece);
                        }
                    }
                }
                best
            })
            .flatten();

        #[allow(clippy::cast_possible_truncation)]
        let Some(piece) = choice.and_then(|p| self.lengths.validate_piece_index(p as u32)) else {
            return;
        };
        self.peers.with_live_mut(addr, "superseed_offer", |live| {
            let slots = &mut live.superseed_pieces;
            if replace.is_some() && slots[0] == replace {
                slots.swap(0, 1);
            }
            slots[1] = slots[0];
            slots[0] = Some(piece);
            let _ = live
                .tx
                .send(WriterRequest::Message(Message::Have(piece.get())));
        });
    }

    /// Make sure a super-seeded peer has a revealed piece that it doesn't have
    /// yet (called when it connects and when its bitfield arrives).
    pub(crate) fn superseed_check(&self, addr: PeerHandle) {
        let need = self
            .peers
            .with_live(addr, |l| {
                if !l.super_seeding {
                    return None;
                }
                match l.superseed_pieces[0] {
                    None => Some(None),
                    Some(p) if l.bitfield.get(p.get_usize()).is_some_and(|b| *b) => Some(Some(p)),
                    Some(_) => None,
                }
            })
            .flatten();
        if let Some(replace) = need {
            self.superseed_offer(addr, replace);
        }
    }

    /// A peer announced it has `piece`: if it was revealed to it while super
    /// seeding, reveal the next one.
    pub(crate) fn superseed_on_have(&self, addr: PeerHandle, piece: ValidPieceIndex) {
        let revealed = self
            .peers
            .with_live(addr, |l| {
                l.super_seeding && l.superseed_pieces.contains(&Some(piece))
            })
            .unwrap_or(false);
        if revealed {
            self.superseed_offer(addr, Some(piece));
        }
    }

    /// Super seeding was turned off: tell super-seeded peers about every
    /// piece they lack, so they can download normally again.
    pub(crate) fn superseed_disable(&self) {
        let total = self.lengths.total_pieces() as usize;
        let have: Vec<u8> = match self.lock_read("superseed_disable").get_chunks() {
            Ok(c) => c.get_have_pieces().as_bytes().to_vec(),
            Err(_) => return,
        };
        let we_have = |i: usize| have.get(i / 8).is_some_and(|b| b & (0x80 >> (i % 8)) != 0);
        for mut e in self.peers.states.iter_mut() {
            let Some(live) = e.value_mut().get_live_mut() else {
                continue;
            };
            if !live.super_seeding {
                continue;
            }
            live.super_seeding = false;
            live.superseed_pieces = [None, None];
            for piece in 0..total {
                if we_have(piece) && !live.bitfield.get(piece).is_some_and(|b| *b) {
                    #[allow(clippy::cast_possible_truncation)]
                    let _ = live
                        .tx
                        .send(WriterRequest::Message(Message::Have(piece as u32)));
                }
            }
        }
    }
}
