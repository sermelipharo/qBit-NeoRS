//! Automatic bans of peers that send corrupt data (libtorrent's
//! `torrent::piece_failed` / `penalize_peers` / `ban_peer`).
//!
//! libtorrent keeps `trust_points` in [-7, 8] on every peer list entry: +1
//! for each piece that passed the hash check with a block from the peer, -2
//! for each failed one. A peer is banned for the torrent once it reaches -7,
//! or at the first failure of a piece it sent alone (`known_bad_peer`: we can
//! be sure it sent the bad data). The ban is posted as `peer_ban_alert`,
//! which qBittorrent logs in the peer log as a banned (not blocked) IP.
//!
//! Here a piece is downloaded from one peer unless it was stolen from a slow
//! peer midway; only the peer that completed the piece is penalized (the
//! previous owner of a stolen piece is not known any more), and it counts as
//! the only source unless the piece was stolen.
//!
//! The bans live as long as the torrent (they survive pause and resume, as
//! libtorrent's peer list does) and are bounded: past [`MAX_AUTO_BANS`] the
//! oldest one is forgotten.

use std::{
    collections::VecDeque,
    net::{IpAddr, SocketAddr},
};

use parking_lot::RwLock;

use crate::net::filter::canonical_ip;

/// libtorrent's `trust_points` bounds (a 4-bit signed field).
const MIN_TRUST: i8 = -7;
const MAX_TRUST: i8 = 8;

/// Automatic bans kept per torrent.
pub(crate) const MAX_AUTO_BANS: usize = 256;

/// The hash check record of one peer entry.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PeerTrust {
    trust_points: i8,
    hashfails: u8,
}

impl PeerTrust {
    /// A piece with data from this peer passed the hash check.
    pub fn on_piece_passed(&mut self) {
        self.trust_points = (self.trust_points + 1).min(MAX_TRUST);
    }

    /// A piece with data from this peer failed the hash check. `sole_source`:
    /// the peer sent the whole piece. Returns whether the peer is to be
    /// banned.
    pub fn on_piece_failed(&mut self, sole_source: bool) -> bool {
        // "we decrease more than we increase, to keep the allowed
        // failed/passed ratio low."
        self.trust_points = (self.trust_points - 2).max(MIN_TRUST);
        self.hashfails = self.hashfails.saturating_add(1);
        sole_source || self.trust_points <= MIN_TRUST
    }

    #[cfg(test)]
    pub fn hashfails(&self) -> u8 {
        self.hashfails
    }
}

/// One banned peer: the address we dial (`port`), or the whole IP when the
/// peer connected to us (its port was an ephemeral one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ban {
    ip: IpAddr,
    port: Option<u16>,
}

/// The peers a torrent banned on its own.
///
/// Our engine allows several connections from one IP (libtorrent's
/// `allow_multiple_connections_per_ip`), where libtorrent bans the
/// `ip:port` entry. A peer we dialled is banned by its listen address, so
/// other peers behind the same IP (NAT, LAN) can still be dialled; incoming
/// connections can't be told apart by their port, so every incoming
/// connection from a banned IP is refused, as libtorrent does by default
/// (one peer per IP).
#[derive(Debug, Default)]
pub(crate) struct AutoBans {
    bans: RwLock<VecDeque<Ban>>,
}

impl AutoBans {
    /// Bans `addr`; `incoming`: the peer connected to us. Returns false if
    /// it was banned already.
    pub fn ban(&self, addr: SocketAddr, incoming: bool) -> bool {
        let ban = Ban {
            ip: canonical_ip(addr.ip()),
            port: (!incoming).then_some(addr.port()),
        };
        let mut bans = self.bans.write();
        if bans.contains(&ban) {
            return false;
        }
        if bans.len() >= MAX_AUTO_BANS {
            bans.pop_front();
        }
        bans.push_back(ban);
        true
    }

    /// Whether we may not dial `addr`.
    pub fn blocks_outgoing(&self, addr: SocketAddr) -> bool {
        let ip = canonical_ip(addr.ip());
        let bans = self.bans.read();
        bans.iter()
            .any(|b| b.ip == ip && b.port.is_none_or(|p| p == addr.port()))
    }

    /// Whether a connection from `ip` is refused.
    pub fn blocks_incoming(&self, ip: IpAddr) -> bool {
        let ip = canonical_ip(ip);
        let bans = self.bans.read();
        bans.iter().any(|b| b.ip == ip)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.bans.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sole_source_is_banned_at_the_first_failure() {
        let mut t = PeerTrust::default();
        assert!(t.on_piece_failed(true));
        assert_eq!(t.hashfails(), 1);
    }

    #[test]
    fn shared_pieces_ban_after_four_failures() {
        // 0 -> -2 -> -4 -> -6 -> -7 (clamped): banned at the fourth.
        let mut t = PeerTrust::default();
        for _ in 0..3 {
            assert!(!t.on_piece_failed(false));
        }
        assert!(t.on_piece_failed(false));
        assert_eq!(t.hashfails(), 4);
    }

    #[test]
    fn passed_pieces_earn_trust_up_to_eight() {
        let mut t = PeerTrust::default();
        for _ in 0..20 {
            t.on_piece_passed();
        }
        // 8 -> 6 -> 4 -> 2 -> 0 -> -2 -> -4 -> -6 -> -7: eight failures.
        for _ in 0..7 {
            assert!(!t.on_piece_failed(false));
        }
        assert!(t.on_piece_failed(false));
    }

    #[test]
    fn alternating_pass_and_fail_eventually_bans() {
        // Each pass/fail round loses one point.
        let mut t = PeerTrust::default();
        let mut rounds = 0;
        loop {
            rounds += 1;
            t.on_piece_passed();
            if t.on_piece_failed(false) {
                break;
            }
            assert!(rounds < 20);
        }
        // After each round: -1, -2, -3, -4, -5, -6, then -5 - 2 = -7.
        assert_eq!(rounds, 7);
    }

    #[test]
    fn hashfails_saturate() {
        let mut t = PeerTrust::default();
        for _ in 0..300 {
            t.on_piece_failed(false);
        }
        assert_eq!(t.hashfails(), u8::MAX);
    }

    #[test]
    fn outgoing_bans_are_per_address_incoming_per_ip() {
        let bans = AutoBans::default();
        let a: SocketAddr = "10.0.0.1:6881".parse().unwrap();
        assert!(bans.ban(a, false));
        assert!(!bans.ban(a, false));
        assert!(bans.blocks_outgoing(a));
        assert!(!bans.blocks_outgoing("10.0.0.1:6882".parse().unwrap()));
        assert!(!bans.blocks_outgoing("10.0.0.2:6881".parse().unwrap()));
        assert!(bans.blocks_incoming(a.ip()));
        // IPv4-mapped IPv6 is the same peer.
        assert!(bans.blocks_incoming("::ffff:10.0.0.1".parse().unwrap()));
        assert!(!bans.blocks_incoming("10.0.0.2".parse().unwrap()));

        let b: SocketAddr = "10.0.0.3:50123".parse().unwrap();
        assert!(bans.ban(b, true));
        assert!(bans.blocks_outgoing("10.0.0.3:6881".parse().unwrap()));
        assert!(bans.blocks_incoming(b.ip()));
    }

    #[test]
    fn bans_are_bounded() {
        let bans = AutoBans::default();
        let first: SocketAddr = "10.1.0.0:1".parse().unwrap();
        bans.ban(first, false);
        for i in 1..=MAX_AUTO_BANS {
            let [.., hi, lo] = u32::try_from(i).unwrap().to_be_bytes();
            bans.ban(SocketAddr::from(([10, 1, hi, lo], 1)), false);
        }
        assert_eq!(bans.len(), MAX_AUTO_BANS);
        assert!(!bans.blocks_outgoing(first));
    }
}
