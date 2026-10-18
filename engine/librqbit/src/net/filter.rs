//! Peer IP filtering: a predicate set by the core (qBittorrent's IP filter
//! ranges), banned addresses ("Ban peer permanently", `banned_IPs`) and a
//! callback reporting blocked connection attempts (the "Blocked IPs" log).

use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use parking_lot::RwLock;
use serde::Serialize;
use tracker_comms::IpFilterFn;

/// Why a peer connection was refused or closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub enum PeerBlockReason {
    /// The IP filter predicate matched (libtorrent `ip_filter`).
    IpFilter,
    /// The address is banned.
    Banned,
    /// A non-I2P peer of an I2P torrent while mixed mode is off
    /// (libtorrent `i2p_mixed`).
    I2pMixedMode,
}

/// One blocked connection attempt (or a connection closed because the filter
/// changed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BlockedPeer {
    pub addr: SocketAddr,
    pub reason: PeerBlockReason,
    /// The peer connected to us.
    pub incoming: bool,
}

pub type BlockedPeerCallback = Arc<dyn Fn(BlockedPeer) + Send + Sync>;

/// IPv4-mapped IPv6 addresses (from dual-stack sockets) are checked as IPv4.
pub(crate) fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

#[derive(Default)]
pub(crate) struct PeerFilter {
    filter: RwLock<Option<IpFilterFn>>,
    banned: RwLock<HashSet<IpAddr>>,
    callback: RwLock<Option<BlockedPeerCallback>>,
}

impl PeerFilter {
    pub fn set_filter(&self, filter: Option<IpFilterFn>) {
        *self.filter.write() = filter;
    }

    pub fn set_banned(&self, ips: impl IntoIterator<Item = IpAddr>) {
        *self.banned.write() = ips.into_iter().map(canonical_ip).collect();
    }

    pub fn ban(&self, ip: IpAddr) -> bool {
        self.banned.write().insert(canonical_ip(ip))
    }

    pub fn unban(&self, ip: IpAddr) -> bool {
        self.banned.write().remove(&canonical_ip(ip))
    }

    pub fn banned(&self) -> Vec<IpAddr> {
        let mut v: Vec<IpAddr> = self.banned.read().iter().copied().collect();
        v.sort_unstable();
        v
    }

    pub fn set_callback(&self, cb: Option<BlockedPeerCallback>) {
        *self.callback.write() = cb;
    }

    /// Why `ip` is not allowed, if it isn't.
    pub fn check(&self, ip: IpAddr) -> Option<PeerBlockReason> {
        let ip = canonical_ip(ip);
        {
            let banned = self.banned.read();
            if !banned.is_empty() && banned.contains(&ip) {
                return Some(PeerBlockReason::Banned);
            }
        }
        let filter = self.filter.read().clone();
        if filter.is_some_and(|f| f(ip)) {
            return Some(PeerBlockReason::IpFilter);
        }
        None
    }

    /// Report a blocked attempt to the callback (if any).
    pub fn report(&self, addr: SocketAddr, reason: PeerBlockReason, incoming: bool) {
        let cb = self.callback.read().clone();
        if let Some(cb) = cb {
            cb(BlockedPeer {
                addr: SocketAddr::new(canonical_ip(addr.ip()), addr.port()),
                reason,
                incoming,
            });
        }
    }

    /// `check` + `report`. Returns true when the peer is blocked.
    pub fn check_and_report(&self, addr: SocketAddr, incoming: bool) -> bool {
        match self.check(addr.ip()) {
            Some(reason) => {
                self.report(addr, reason, incoming);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn filter_bans_and_callback() {
        let f = PeerFilter::default();
        let a: SocketAddr = "10.0.0.1:5000".parse().unwrap();
        let mapped: SocketAddr = "[::ffff:10.0.0.1]:5000".parse().unwrap();
        assert_eq!(f.check(a.ip()), None);

        f.set_filter(Some(Arc::new(
            |ip: IpAddr| matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 10),
        )));
        assert_eq!(f.check(a.ip()), Some(PeerBlockReason::IpFilter));
        // Mapped addresses are checked as IPv4.
        assert_eq!(f.check(mapped.ip()), Some(PeerBlockReason::IpFilter));
        assert_eq!(f.check("11.0.0.1".parse().unwrap()), None);

        assert!(f.ban(mapped.ip()));
        assert_eq!(f.check(a.ip()), Some(PeerBlockReason::Banned));
        assert_eq!(f.banned(), vec!["10.0.0.1".parse::<IpAddr>().unwrap()]);
        assert!(f.unban(a.ip()));
        f.set_filter(None);
        assert_eq!(f.check(a.ip()), None);

        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        f.set_callback(Some(Arc::new(move |b: BlockedPeer| {
            assert_eq!(b.addr, "10.0.0.1:5000".parse().unwrap());
            assert_eq!(b.reason, PeerBlockReason::Banned);
            c.fetch_add(1, Ordering::Relaxed);
        })));
        f.set_banned(["10.0.0.1".parse().unwrap()]);
        assert!(f.check_and_report(mapped, true));
        assert!(!f.check_and_report("1.2.3.4:1".parse().unwrap(), true));
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }
}
