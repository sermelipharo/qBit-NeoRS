//! Our public address, voted on by the parties that can see it (like
//! libtorrent's `ip_voter`): the gateway (NAT-PMP / UPnP), trackers (BEP 24
//! `external ip`) and peers (BEP 10 `yourip`).
//!
//! Each source holds at most one vote per address family, so one peer or
//! tracker cannot win alone by repeating itself. Only the most recent votes
//! are kept (older ones fall out), which bounds memory and lets the result
//! follow an address change. Addresses that cannot be ours on the internet
//! (private, loopback, link-local, CGNAT, unique-local ...) are ignored from
//! every source.

use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::net::IpAddr;

use parking_lot::Mutex;

/// Votes remembered per address family (one per source).
const MAX_VOTES: usize = 32;
/// Weight of the gateway's answer: it knows the address best, but enough
/// peers and trackers seeing another one (e.g. through a VPN) win.
const ROUTER_WEIGHT: u32 = 4;
const TRACKER_WEIGHT: u32 = 2;
const PEER_WEIGHT: u32 = 1;
/// A candidate needs this much support: one tracker, or two peers.
const MIN_WEIGHT: u32 = 2;

/// Who told us the address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IpVoteSource<'a> {
    /// The NAT-PMP / UPnP gateway (one slot per family; replaces the last).
    Router,
    /// A tracker, identified by its host.
    Tracker(&'a str),
    /// A peer, identified by its address.
    Peer(IpAddr),
}

#[derive(Clone, Copy)]
struct Vote {
    source: u64,
    ip: IpAddr,
    weight: u32,
}

#[derive(Default)]
struct Family {
    router: Option<IpAddr>,
    votes: VecDeque<Vote>,
    current: Option<IpAddr>,
}

impl Family {
    fn cast(&mut self, source: u64, ip: IpAddr, weight: u32) {
        self.votes.retain(|v| v.source != source);
        if self.votes.len() >= MAX_VOTES {
            self.votes.pop_front();
        }
        self.votes.push_back(Vote { source, ip, weight });
        self.elect();
    }

    fn elect(&mut self) {
        // At most MAX_VOTES + 1 distinct candidates.
        let mut tally: Vec<(IpAddr, u32)> = Vec::new();
        let mut add = |ip: IpAddr, w: u32| match tally.iter_mut().find(|(a, _)| *a == ip) {
            Some((_, t)) => *t += w,
            None => tally.push((ip, w)),
        };
        if let Some(r) = self.router {
            add(r, ROUTER_WEIGHT);
        }
        for v in &self.votes {
            add(v.ip, v.weight);
        }
        let weight_of = |ip: Option<IpAddr>| {
            ip.and_then(|ip| tally.iter().find(|(a, _)| *a == ip).map(|t| t.1))
        };
        let Some(&(best, best_weight)) = tally.iter().max_by_key(|(_, w)| *w) else {
            self.current = None;
            return;
        };
        // Ties keep the current address, then prefer the gateway's.
        if weight_of(self.current) == Some(best_weight) {
            return;
        }
        if weight_of(self.router) == Some(best_weight) {
            self.current = self.router;
            return;
        }
        self.current = (best_weight >= MIN_WEIGHT).then_some(best).or(
            // Keep a still supported address rather than showing nothing.
            self.current.filter(|_| weight_of(self.current).is_some()),
        );
    }
}

/// Session-wide external address voter (IPv4 and IPv6 separately).
#[derive(Default)]
pub(crate) struct ExternalIpVoter {
    families: Mutex<[Family; 2]>,
}

fn family_index(ip: &IpAddr) -> usize {
    match ip {
        IpAddr::V4(_) => 0,
        IpAddr::V6(_) => 1,
    }
}

fn source_key(source: IpVoteSource<'_>) -> Option<u64> {
    let mut h = DefaultHasher::new();
    match source {
        IpVoteSource::Router => return None,
        IpVoteSource::Tracker(host) => {
            0u8.hash(&mut h);
            host.to_ascii_lowercase().hash(&mut h);
        }
        IpVoteSource::Peer(ip) => {
            1u8.hash(&mut h);
            ip.to_canonical().hash(&mut h);
        }
    }
    Some(h.finish())
}

impl ExternalIpVoter {
    /// `source` says our address is `ip`. Returns whether the elected
    /// address of that family changed.
    pub fn cast_vote(&self, ip: IpAddr, source: IpVoteSource<'_>) -> bool {
        let ip = ip.to_canonical();
        if !is_public_ip(&ip) {
            return false;
        }
        let weight = match source {
            IpVoteSource::Router => ROUTER_WEIGHT,
            IpVoteSource::Tracker(_) => TRACKER_WEIGHT,
            // Peers on our own network cannot know our public address.
            IpVoteSource::Peer(p) if !is_public_ip(&p.to_canonical()) => return false,
            IpVoteSource::Peer(_) => PEER_WEIGHT,
        };
        let mut g = self.families.lock();
        let f = &mut g[family_index(&ip)];
        let before = f.current;
        match source_key(source) {
            None => {
                f.router = Some(ip);
                f.elect();
            }
            Some(key) => f.cast(key, ip, weight),
        }
        f.current != before
    }

    /// The gateway no longer reports an address (port mapping restarted or
    /// switched off).
    pub fn clear_router(&self) {
        let mut g = self.families.lock();
        for f in g.iter_mut() {
            if f.router.take().is_some() {
                f.elect();
            }
        }
    }

    /// Forget everything (the network changed).
    pub fn reset(&self) {
        *self.families.lock() = Default::default();
    }

    /// The elected IPv4 and IPv6 address.
    pub fn external_ips(&self) -> (Option<IpAddr>, Option<IpAddr>) {
        let g = self.families.lock();
        (g[0].current, g[1].current)
    }
}

/// Whether `ip` could be a public internet address.
pub(crate) fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            !(a.is_private()
                || a.is_loopback()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_broadcast()
                || a.is_multicast()
                || a.is_documentation()
                // 0.0.0.0/8, CGNAT 100.64.0.0/10, benchmarking 198.18.0.0/15,
                // reserved 240.0.0.0/4.
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                || o[0] >= 240)
        }
        IpAddr::V6(a) => {
            let s = a.segments();
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                // Unique-local fc00::/7, link-local fe80::/10, site-local
                // fec0::/10, documentation 2001:db8::/32, IPv4-mapped.
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] & 0xffc0) == 0xfec0
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                || a.to_ipv4_mapped().is_some())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn peer(i: u8) -> IpVoteSource<'static> {
        IpVoteSource::Peer(IpAddr::from([81, 2, 3, i]))
    }

    #[test]
    fn only_public_addresses_are_kept() {
        for ok in ["8.8.8.8", "93.184.216.34", "2606:4700::1111"] {
            assert!(is_public_ip(&ip(ok)), "{ok}");
        }
        for bad in [
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.5",
            "127.0.0.1",
            "169.254.1.1",
            "0.0.0.0",
            "0.1.2.3",
            "100.64.0.1",
            "100.127.255.254",
            "198.18.0.1",
            "192.0.2.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "fc00::1",
            "2001:db8::1",
            "::ffff:1.2.3.4",
            "ff02::1",
        ] {
            assert!(!is_public_ip(&ip(bad)), "{bad}");
        }
        assert!(is_public_ip(&ip("100.128.0.1")));
    }

    #[test]
    fn non_public_addresses_and_lan_peers_do_not_vote() {
        let v = ExternalIpVoter::default();
        assert!(!v.cast_vote(ip("100.64.1.2"), IpVoteSource::Router));
        assert!(!v.cast_vote(ip("192.168.0.2"), IpVoteSource::Tracker("t.example")));
        assert!(!v.cast_vote(ip("fd00::2"), IpVoteSource::Tracker("t.example")));
        // A LAN peer cannot know our public address.
        for _ in 0..3 {
            v.cast_vote(ip("5.6.7.8"), IpVoteSource::Peer(ip("192.168.0.9")));
        }
        assert_eq!(v.external_ips(), (None, None));
    }

    #[test]
    fn router_and_tracker_are_enough_alone() {
        let v = ExternalIpVoter::default();
        assert!(v.cast_vote(ip("5.6.7.8"), IpVoteSource::Router));
        assert_eq!(v.external_ips(), (Some(ip("5.6.7.8")), None));

        let v = ExternalIpVoter::default();
        assert!(v.cast_vote(ip("2a01::5"), IpVoteSource::Tracker("t.example")));
        assert_eq!(v.external_ips(), (None, Some(ip("2a01::5"))));
        // IPv4-mapped addresses count as IPv4.
        assert!(v.cast_vote(ip("::ffff:5.6.7.8"), IpVoteSource::Tracker("t.example")));
        assert_eq!(v.external_ips(), (Some(ip("5.6.7.8")), Some(ip("2a01::5"))));
    }

    #[test]
    fn one_source_cannot_win_alone() {
        let v = ExternalIpVoter::default();
        // A single peer repeating itself is one vote, below the threshold.
        for _ in 0..50 {
            v.cast_vote(ip("1.1.1.1"), peer(1));
        }
        assert_eq!(v.external_ips().0, None);
        v.cast_vote(ip("1.1.1.1"), peer(2));
        assert_eq!(v.external_ips().0, Some(ip("1.1.1.1")));

        // Against a tracker's address, a peer flooding another one stays
        // at one vote.
        let v = ExternalIpVoter::default();
        v.cast_vote(ip("5.6.7.8"), IpVoteSource::Tracker("t.example"));
        for _ in 0..50 {
            v.cast_vote(ip("6.6.6.6"), peer(1));
        }
        assert_eq!(v.external_ips().0, Some(ip("5.6.7.8")));
    }

    #[test]
    fn a_source_moves_its_vote() {
        let v = ExternalIpVoter::default();
        v.cast_vote(ip("5.6.7.8"), IpVoteSource::Tracker("t.example"));
        // The same tracker (any case) now sees another address.
        assert!(v.cast_vote(ip("5.6.7.9"), IpVoteSource::Tracker("T.Example")));
        assert_eq!(v.external_ips().0, Some(ip("5.6.7.9")));
    }

    #[test]
    fn router_counts_strongly_but_can_be_outvoted() {
        let v = ExternalIpVoter::default();
        v.cast_vote(ip("5.6.7.8"), IpVoteSource::Router);
        // Three peers see a VPN address: not enough against the gateway.
        for i in 1..=3 {
            v.cast_vote(ip("9.9.9.9"), peer(i));
        }
        assert_eq!(v.external_ips().0, Some(ip("5.6.7.8")));
        // A tie keeps the current address.
        v.cast_vote(ip("9.9.9.9"), peer(4));
        assert_eq!(v.external_ips().0, Some(ip("5.6.7.8")));
        v.cast_vote(ip("9.9.9.9"), peer(5));
        assert_eq!(v.external_ips().0, Some(ip("9.9.9.9")));
    }

    #[test]
    fn ties_prefer_the_gateway() {
        let v = ExternalIpVoter::default();
        for i in 1..=4 {
            v.cast_vote(ip("9.9.9.9"), peer(i));
        }
        assert_eq!(v.external_ips().0, Some(ip("9.9.9.9")));
        // Equal support: the current address stays.
        v.cast_vote(ip("5.6.7.8"), IpVoteSource::Router);
        assert_eq!(v.external_ips().0, Some(ip("9.9.9.9")));
        // The gateway's answer gone, the peers' address remains.
        v.clear_router();
        assert_eq!(v.external_ips().0, Some(ip("9.9.9.9")));

        // A peer moving its vote breaks the tie.
        v.cast_vote(ip("5.6.7.8"), IpVoteSource::Router);
        v.cast_vote(ip("5.6.7.8"), peer(1));
        assert_eq!(v.external_ips().0, Some(ip("5.6.7.8")));
        // A network change forgets everything.
        v.reset();
        assert_eq!(v.external_ips(), (None, None));
    }

    #[test]
    fn votes_are_bounded_and_old_ones_fall_out() {
        let v = ExternalIpVoter::default();
        for i in 0..u8::try_from(MAX_VOTES).unwrap() {
            v.cast_vote(ip("1.1.1.1"), peer(i));
        }
        assert_eq!(v.external_ips().0, Some(ip("1.1.1.1")));
        // The address changed: new votes push the old ones out.
        for i in 0..=u8::try_from(MAX_VOTES / 2).unwrap() {
            v.cast_vote(
                ip("2.2.2.2"),
                IpVoteSource::Peer(IpAddr::from([82, 0, 0, i])),
            );
        }
        assert_eq!(v.external_ips().0, Some(ip("2.2.2.2")));
        let g = v.families.lock();
        assert_eq!(g[0].votes.len(), MAX_VOTES);
    }
}
