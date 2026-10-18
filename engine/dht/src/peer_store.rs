use std::{collections::VecDeque, net::SocketAddr, str::FromStr, sync::atomic::AtomicU32};

use bencode::ByteBufOwned;
use chrono::{DateTime, Utc};
use librqbit_core::{compact_ip::CompactSocketAddr, hash_id::Id20};
use parking_lot::RwLock;
use rand::Rng;
use serde::{
    Deserialize, Serialize,
    ser::{SerializeMap, SerializeStruct},
};
use tracing::trace;

use crate::bprotocol::{AnnouncePeer, Want};

#[derive(Serialize, Deserialize)]
struct StoredToken {
    token: [u8; 4],
    #[serde(serialize_with = "crate::utils::serialize_id20")]
    node_id: Id20,
    addr: SocketAddr,
}

/// Peers per `get_peers` reply (libtorrent's `max_peers_reply`).
pub const MAX_PEERS_REPLY: usize = 100;
/// Announced peers expire unless they re-announce (BEP 5 suggests 30 min).
const PEER_TTL: chrono::TimeDelta = chrono::TimeDelta::minutes(30);

#[derive(Serialize, Deserialize)]
struct StoredPeer {
    addr: SocketAddr,
    time: DateTime<Utc>,
}

pub struct PeerStore {
    self_id: Id20,
    max_remembered_tokens: u32,
    max_remembered_peers: u32,
    max_distance: Id20,
    tokens: RwLock<VecDeque<StoredToken>>,
    peers: dashmap::DashMap<Id20, Vec<StoredPeer>>,
    peers_len: AtomicU32,
}

impl Serialize for PeerStore {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        struct SerializePeers<'a> {
            peers: &'a dashmap::DashMap<Id20, Vec<StoredPeer>>,
        }

        impl Serialize for SerializePeers<'_> {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                let mut m = serializer.serialize_map(None)?;
                for entry in self.peers.iter() {
                    m.serialize_entry(&entry.key().as_string(), &entry.value())?;
                }
                m.end()
            }
        }

        let mut s = serializer.serialize_struct("PeerStore", 7)?;
        s.serialize_field("self_id", &self.self_id.as_string())?;
        s.serialize_field("max_remembered_tokens", &self.max_remembered_tokens)?;
        s.serialize_field("max_remembered_peers", &self.max_remembered_peers)?;
        s.serialize_field("max_distance", &self.max_distance.as_string())?;
        s.serialize_field("tokens", &*self.tokens.read())?;
        s.serialize_field("peers", &SerializePeers { peers: &self.peers })?;
        s.serialize_field(
            "peers_len",
            &self.peers_len.load(std::sync::atomic::Ordering::SeqCst),
        )?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for PeerStore {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Tmp {
            self_id: Id20,
            max_remembered_tokens: u32,
            max_remembered_peers: u32,
            max_distance: Id20,
            tokens: VecDeque<StoredToken>,
            peers: dashmap::DashMap<Id20, Vec<StoredPeer>>,
        }

        Tmp::deserialize(deserializer).map(|tmp| Self {
            self_id: tmp.self_id,
            max_remembered_tokens: tmp.max_remembered_tokens,
            max_remembered_peers: tmp.max_remembered_peers,
            max_distance: tmp.max_distance,
            tokens: RwLock::new(tmp.tokens),
            peers_len: AtomicU32::new(tmp.peers.iter().map(|e| e.value().len() as u32).sum()),
            peers: tmp.peers,
        })
    }
}

impl PeerStore {
    pub fn new(self_id: Id20) -> Self {
        Self {
            self_id,
            max_remembered_tokens: 1000,
            max_remembered_peers: 1000,
            max_distance: Id20::from_str("00000fffffffffffffffffffffffffffffffffff").unwrap(),
            tokens: RwLock::new(VecDeque::new()),
            peers: dashmap::DashMap::new(),
            peers_len: AtomicU32::new(0),
        }
    }

    pub fn gen_token_for(&self, node_id: Id20, addr: SocketAddr) -> [u8; 4] {
        let mut token = [0u8; 4];
        rand::rng().fill_bytes(&mut token);
        let mut tokens = self.tokens.write();
        tokens.push_back(StoredToken {
            token,
            addr,
            node_id,
        });
        if tokens.len() > self.max_remembered_tokens as usize {
            tokens.pop_front();
        }
        token
    }

    pub fn store_peer(&self, announce: &AnnouncePeer<ByteBufOwned>, mut addr: SocketAddr) -> bool {
        // If the info_hash in announce is too far away from us, don't store it.
        // If the token doesn't match, don't store it.
        // If we are out of capacity, don't store it.
        // Otherwise, store it.
        if announce.info_hash.distance(&self.self_id) > self.max_distance {
            trace!("peer store: info_hash too far to store");
            return false;
        }
        if !self.tokens.read().iter().any(|t| {
            t.token[..] == announce.token.as_ref()[..] && t.addr == addr && t.node_id == announce.id
        }) {
            trace!("peer store: can't find this token / addr combination");
            return false;
        }

        if announce.implied_port == 0 {
            addr.set_port(announce.port);
        }

        // Full: make room by dropping expired peers first (before taking an
        // entry lock, which GC would otherwise deadlock on).
        if self.peers_len.load(std::sync::atomic::Ordering::SeqCst) >= self.max_remembered_peers {
            self.garbage_collect_peers();
        }

        use dashmap::mapref::entry::Entry;
        let peers_entry = self.peers.entry(announce.info_hash);
        let peers_len = self.peers_len.load(std::sync::atomic::Ordering::SeqCst);
        match peers_entry {
            Entry::Occupied(mut occ) => {
                // One entry per IP and info hash (like libtorrent): a node
                // announcing many ports must not fill the store.
                if let Some(s) = occ.get_mut().iter_mut().find(|s| s.addr.ip() == addr.ip()) {
                    s.addr = addr;
                    s.time = Utc::now();
                    return true;
                }
                if peers_len >= self.max_remembered_peers {
                    trace!("peer store: out of capacity");
                    return false;
                }
                occ.get_mut().push(StoredPeer {
                    addr,
                    time: Utc::now(),
                });
            }
            Entry::Vacant(vac) => {
                if peers_len >= self.max_remembered_peers {
                    trace!("peer store: out of capacity");
                    return false;
                }
                vac.insert(vec![StoredPeer {
                    addr,
                    time: Utc::now(),
                }]);
            }
        }

        self.peers_len
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        true
    }

    /// Stored peers for a `get_peers` reply: at most [`MAX_PEERS_REPLY`]
    /// fresh ones, so replies to (possibly spoofed) tiny queries stay small
    /// and the node cannot be used as a reflection amplifier.
    pub fn get_for_info_hash(&self, info_hash: Id20, want: Want) -> Vec<CompactSocketAddr> {
        if let Some(stored_peers) = self.peers.get(&info_hash) {
            let cutoff = Utc::now() - PEER_TTL;
            let mut fresh: Vec<&StoredPeer> = stored_peers
                .iter()
                .filter(|p| p.time >= cutoff)
                .filter(|p| {
                    matches!(
                        (p.addr, want),
                        (SocketAddr::V6(..), Want::V6 | Want::Both)
                            | (SocketAddr::V4(..), Want::V4 | Want::Both)
                    )
                })
                .collect();
            if fresh.len() > MAX_PEERS_REPLY {
                use rand::seq::SliceRandom;
                fresh.shuffle(&mut rand::rng());
                fresh.truncate(MAX_PEERS_REPLY);
            }
            return fresh.into_iter().map(|p| p.addr.into()).collect();
        }
        Vec::new()
    }

    /// Drops peers that have not re-announced within [`PEER_TTL`].
    pub fn garbage_collect_peers(&self) {
        let cutoff = Utc::now() - PEER_TTL;
        let mut removed = 0u32;
        self.peers.retain(|_, peers| {
            let before = peers.len();
            peers.retain(|p| p.time >= cutoff);
            removed += (before - peers.len()) as u32;
            !peers.is_empty()
        });
        let _ = self.peers_len.try_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |n| Some(n.saturating_sub(removed)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announce(store: &PeerStore, node: SocketAddr, info_hash: Id20, port: u16) -> bool {
        let id = Id20::new([7; 20]);
        let token = store.gen_token_for(id, node);
        let a = AnnouncePeer { id, implied_port: 0, info_hash, port, token: ByteBufOwned::from(token.to_vec()) };
        store.store_peer(&a, node)
    }

    #[test]
    fn replies_are_capped_and_one_entry_per_ip() {
        let me = Id20::new([0; 20]);
        let store = PeerStore::new(me);
        let ih = Id20::new([0; 20]);
        // One node announcing hundreds of ports keeps a single entry.
        let node: SocketAddr = "1.2.3.4:6881".parse().unwrap();
        for port in 1..=500 {
            assert!(announce(&store, node, ih, port));
        }
        assert_eq!(store.get_for_info_hash(ih, Want::Both).len(), 1);
        assert_eq!(store.peers_len.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Many distinct IPs: replies stay bounded.
        for i in 0..500u32 {
            let addr = SocketAddr::from((std::net::Ipv4Addr::from(0x0a00_0000 + i), 6881));
            announce(&store, addr, ih, 1);
        }
        assert_eq!(store.get_for_info_hash(ih, Want::Both).len(), MAX_PEERS_REPLY);
    }

    #[test]
    fn expired_peers_are_collected() {
        let store = PeerStore::new(Id20::new([0; 20]));
        let ih = Id20::new([0; 20]);
        assert!(announce(&store, "1.2.3.4:1".parse().unwrap(), ih, 1));
        store.peers.get_mut(&ih).unwrap()[0].time = Utc::now() - PEER_TTL - chrono::TimeDelta::seconds(1);
        assert!(store.get_for_info_hash(ih, Want::Both).is_empty());
        store.garbage_collect_peers();
        assert!(store.peers.is_empty());
        assert_eq!(store.peers_len.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
