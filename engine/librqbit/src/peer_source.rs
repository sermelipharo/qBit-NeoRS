//! Where peer addresses come from (trackers, DHT, PeX, LSD, ...).
//!
//! Shared between the tracker/peer-discovery plumbing and the live torrent
//! engine. Discovery streams yield `(SocketAddr, PeerSource)`; each torrent
//! keeps a bounded [`PeerSourceRegistry`] of the sources every address was
//! seen from, used for per-peer source flags and the DHT/PeX/LSD rows of the
//! Trackers tab.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;

use futures::stream::BoxStream;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// A way a peer address was learned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PeerSource {
    Tracker,
    Dht,
    Pex,
    Lsd,
    /// The peer connected to us.
    Incoming,
    /// Added explicitly (initial peers, "Add peers", resume data).
    Manual,
}

impl PeerSource {
    pub const ALL: [PeerSource; 6] = [
        PeerSource::Tracker,
        PeerSource::Dht,
        PeerSource::Pex,
        PeerSource::Lsd,
        PeerSource::Incoming,
        PeerSource::Manual,
    ];

    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// A set of [`PeerSource`]s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
pub struct PeerSources(u8);

impl PeerSources {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn from_source(source: PeerSource) -> Self {
        Self(source.bit())
    }

    pub const fn contains(self, source: PeerSource) -> bool {
        self.0 & source.bit() != 0
    }

    pub fn insert(&mut self, source: PeerSource) {
        self.0 |= source.bit();
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = PeerSource> {
        PeerSource::ALL
            .into_iter()
            .filter(move |s| self.contains(*s))
    }
}

impl From<PeerSource> for PeerSources {
    fn from(s: PeerSource) -> Self {
        Self(s.bit())
    }
}

/// Peer discovery switched off for one torrent (libtorrent's `disable_dht`,
/// `disable_pex` and `disable_lsd` torrent flags). Private torrents never use
/// DHT, PeX or LSD whatever these say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DisabledDiscovery {
    #[serde(default)]
    pub dht: bool,
    #[serde(default)]
    pub pex: bool,
    #[serde(default)]
    pub lsd: bool,
}

impl DisabledDiscovery {
    /// Whether peers from `source` are not looked for.
    pub const fn blocks(self, source: PeerSource) -> bool {
        match source {
            PeerSource::Dht => self.dht,
            PeerSource::Pex => self.pex,
            PeerSource::Lsd => self.lsd,
            _ => false,
        }
    }
}

/// Connected peers of a torrent, counted per source. A peer learned from
/// several sources counts for each of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PeerSourceCounts {
    pub tracker: u32,
    pub dht: u32,
    pub pex: u32,
    pub lsd: u32,
}

/// A stream of discovered peers tagged with their source.
pub type SourcedPeerStream = BoxStream<'static, (SocketAddr, PeerSource)>;

/// Maximum number of addresses remembered per torrent.
pub const MAX_TRACKED_PEERS: usize = 16 * 1024;

fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// Per-torrent map from peer address to the sources it was seen from.
/// Bounded by [`MAX_TRACKED_PEERS`]; cleared when the torrent starts.
#[derive(Default)]
pub struct PeerSourceRegistry {
    map: Mutex<HashMap<SocketAddr, PeerSources>>,
    /// Addresses learned under the truncated v2 hash of a hybrid torrent
    /// (libtorrent's `protocol_v2` peers): we connect to them with that hash.
    /// Same bound as `map`.
    v2: Mutex<HashSet<SocketAddr>>,
}

impl PeerSourceRegistry {
    pub fn record(&self, addr: SocketAddr, source: PeerSource) {
        let addr = canonical(addr);
        let mut g = self.map.lock();
        if let Some(s) = g.get_mut(&addr) {
            s.insert(source);
        } else if g.len() < MAX_TRACKED_PEERS {
            g.insert(addr, source.into());
        }
    }

    /// `addr` was announced under the torrent's v2 hash.
    pub fn mark_v2(&self, addr: SocketAddr) {
        let mut g = self.v2.lock();
        if g.len() < MAX_TRACKED_PEERS {
            g.insert(canonical(addr));
        }
    }

    pub fn learned_v2(&self, addr: SocketAddr) -> bool {
        self.v2.lock().contains(&canonical(addr))
    }

    pub fn sources(&self, addr: SocketAddr) -> PeerSources {
        self.map
            .lock()
            .get(&canonical(addr))
            .copied()
            .unwrap_or_default()
    }

    pub fn clear(&self) {
        let mut g = self.map.lock();
        g.clear();
        g.shrink_to(64);
        let mut v2 = self.v2.lock();
        v2.clear();
        v2.shrink_to(0);
    }

    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Count the given (connected) peers per source.
    pub fn count(&self, addrs: impl IntoIterator<Item = SocketAddr>) -> PeerSourceCounts {
        let g = self.map.lock();
        let mut c = PeerSourceCounts::default();
        for addr in addrs {
            let Some(s) = g.get(&canonical(addr)) else {
                continue;
            };
            c.tracker += u32::from(s.contains(PeerSource::Tracker));
            c.dht += u32::from(s.contains(PeerSource::Dht));
            c.pex += u32::from(s.contains(PeerSource::Pex));
            c.lsd += u32::from(s.contains(PeerSource::Lsd));
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_set() {
        let mut s = PeerSources::empty();
        assert!(s.is_empty());
        s.insert(PeerSource::Dht);
        s.insert(PeerSource::Lsd);
        assert!(s.contains(PeerSource::Dht));
        assert!(!s.contains(PeerSource::Tracker));
        assert_eq!(
            s.iter().collect::<Vec<_>>(),
            vec![PeerSource::Dht, PeerSource::Lsd]
        );
    }

    #[test]
    fn registry_counts_and_bounds() {
        let r = PeerSourceRegistry::default();
        let a: SocketAddr = "1.2.3.4:5".parse().unwrap();
        let b: SocketAddr = "[::ffff:5.6.7.8]:9".parse().unwrap();
        r.record(a, PeerSource::Tracker);
        r.record(a, PeerSource::Dht);
        r.record(b, PeerSource::Lsd);
        assert!(
            r.sources("5.6.7.8:9".parse().unwrap())
                .contains(PeerSource::Lsd)
        );
        let c = r.count([
            a,
            "5.6.7.8:9".parse().unwrap(),
            "9.9.9.9:1".parse().unwrap(),
        ]);
        assert_eq!(
            c,
            PeerSourceCounts {
                tracker: 1,
                dht: 1,
                pex: 0,
                lsd: 1
            }
        );
        for i in 0..(u32::try_from(MAX_TRACKED_PEERS).unwrap() + 100) {
            r.record(
                SocketAddr::new(std::net::Ipv4Addr::from(i).into(), 1),
                PeerSource::Pex,
            );
        }
        assert_eq!(r.len(), MAX_TRACKED_PEERS);
        r.mark_v2("[::ffff:1.2.3.4]:5".parse().unwrap());
        assert!(r.learned_v2(a));
        assert!(!r.learned_v2(b));
        for i in 0..(u32::try_from(MAX_TRACKED_PEERS).unwrap() + 100) {
            r.mark_v2(SocketAddr::new(std::net::Ipv4Addr::from(i).into(), 2));
        }
        assert_eq!(r.v2.lock().len(), MAX_TRACKED_PEERS);
        r.clear();
        assert!(r.is_empty());
        assert!(!r.learned_v2(a));
    }
}
