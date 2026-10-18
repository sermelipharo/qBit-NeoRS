use std::collections::HashMap;
use std::net::IpAddr;

/// Same caps as qBittorrent's tracker.
pub const MAX_TORRENTS: usize = 10_000;
pub const MAX_PEERS_PER_TORRENT: usize = 200;
pub const ANNOUNCE_INTERVAL: i64 = 1800;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// Address as reported (claimed or socket), for non-compact replies.
    pub address: String,
    pub ip: IpAddr,
    pub port: u16,
    pub peer_id: Vec<u8>,
    pub is_seeder: bool,
}

impl Peer {
    /// qBittorrent identifies peers by `address:port`.
    fn key(&self) -> (String, u16) {
        (self.address.clone(), self.port)
    }

    /// Compact form: big-endian IP followed by the port (6 or 18 bytes).
    pub fn compact(&self) -> Vec<u8> {
        let mut v = match self.ip {
            IpAddr::V4(a) => a.octets().to_vec(),
            IpAddr::V6(a) => a.octets().to_vec(),
        };
        v.extend_from_slice(&self.port.to_be_bytes());
        v
    }
}

#[derive(Default, Debug)]
pub struct TorrentPeers {
    /// Insertion-ordered so eviction is deterministic.
    peers: Vec<Peer>,
    seeders: usize,
}

impl TorrentPeers {
    pub fn set_peer(&mut self, peer: Peer) {
        if !self.remove_peer(&peer) && self.peers.len() >= MAX_PEERS_PER_TORRENT {
            let first = self.peers[0].clone();
            self.remove_peer(&first);
        }
        if peer.is_seeder {
            self.seeders += 1;
        }
        self.peers.push(peer);
    }

    pub fn remove_peer(&mut self, peer: &Peer) -> bool {
        let key = peer.key();
        let Some(pos) = self.peers.iter().position(|p| p.key() == key) else {
            return false;
        };
        let old = self.peers.remove(pos);
        if old.is_seeder {
            self.seeders -= 1;
        }
        true
    }

    pub fn peers(&self) -> &[Peer] {
        &self.peers
    }

    pub fn seeders(&self) -> usize {
        self.seeders
    }

    pub fn leechers(&self) -> usize {
        self.peers.len() - self.seeders
    }
}

#[derive(Default, Debug)]
pub struct TrackerState {
    torrents: HashMap<[u8; 20], TorrentPeers>,
}

impl TrackerState {
    pub fn register(&mut self, info_hash: [u8; 20], peer: Peer) {
        if !self.torrents.contains_key(&info_hash) && self.torrents.len() >= MAX_TORRENTS {
            // Upstream removes an arbitrary torrent when full.
            if let Some(k) = self.torrents.keys().next().copied() {
                self.torrents.remove(&k);
            }
        }
        self.torrents.entry(info_hash).or_default().set_peer(peer);
    }

    pub fn unregister(&mut self, info_hash: &[u8; 20], peer: &Peer) {
        if let Some(t) = self.torrents.get_mut(info_hash) {
            t.remove_peer(peer);
            if t.peers.is_empty() {
                self.torrents.remove(info_hash);
            }
        }
    }

    pub fn torrent(&self, info_hash: &[u8; 20]) -> Option<&TorrentPeers> {
        self.torrents.get(info_hash)
    }

    pub fn torrent_count(&self) -> usize {
        self.torrents.len()
    }

    pub fn clear(&mut self) {
        self.torrents.clear();
    }
}
