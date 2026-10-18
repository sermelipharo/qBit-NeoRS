//! Per-torrent settings that can change at runtime and survive pause/unpause
//! (they live in `ManagedTorrentShared`, not in the live state).

use std::{
    net::SocketAddr,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use parking_lot::Mutex;

use crate::{
    limits::{Limits, LimitsConfig},
    torrent_state::live::peer_details::PeerSource,
};

const NONE_U32: u32 = u32::MAX;
const NONE_USIZE: usize = usize::MAX;

/// Upper bound of peers queued by `add_peers` while the torrent isn't live.
const MAX_PENDING_PEERS: usize = 500;

pub(crate) struct TorrentRuntime {
    pub ratelimits: Limits,
    sequential: AtomicBool,
    first_last_piece_priority: AtomicBool,
    super_seeding: AtomicBool,
    // NONE_U32 = unlimited
    max_uploads: AtomicU32,
    // NONE_USIZE = unlimited
    peer_limit: AtomicUsize,
    // Bytes of pieces that failed the hash check.
    pub wasted_failed_bytes: AtomicU64,
    // Bytes received that we didn't need (duplicates, late or stolen blocks).
    pub wasted_redundant_bytes: AtomicU64,
    pending_peers: Mutex<Vec<(SocketAddr, PeerSource)>>,
}

impl TorrentRuntime {
    pub fn new(
        ratelimits: LimitsConfig,
        max_uploads: Option<u32>,
        peer_limit: Option<usize>,
    ) -> Self {
        Self {
            ratelimits: Limits::new(ratelimits),
            sequential: AtomicBool::new(false),
            first_last_piece_priority: AtomicBool::new(false),
            super_seeding: AtomicBool::new(false),
            max_uploads: AtomicU32::new(max_uploads.unwrap_or(NONE_U32)),
            peer_limit: AtomicUsize::new(peer_limit.unwrap_or(NONE_USIZE)),
            wasted_failed_bytes: AtomicU64::new(0),
            wasted_redundant_bytes: AtomicU64::new(0),
            pending_peers: Mutex::new(Vec::new()),
        }
    }

    pub fn sequential(&self) -> bool {
        self.sequential.load(Ordering::Relaxed)
    }

    pub fn set_sequential(&self, v: bool) {
        self.sequential.store(v, Ordering::Relaxed)
    }

    pub fn first_last_piece_priority(&self) -> bool {
        self.first_last_piece_priority.load(Ordering::Relaxed)
    }

    pub fn set_first_last_piece_priority(&self, v: bool) -> bool {
        self.first_last_piece_priority.swap(v, Ordering::Relaxed)
    }

    pub fn super_seeding(&self) -> bool {
        self.super_seeding.load(Ordering::Relaxed)
    }

    pub fn set_super_seeding(&self, v: bool) -> bool {
        self.super_seeding.swap(v, Ordering::Relaxed)
    }

    pub fn max_uploads(&self) -> Option<u32> {
        match self.max_uploads.load(Ordering::Relaxed) {
            NONE_U32 => None,
            v => Some(v),
        }
    }

    pub fn set_max_uploads(&self, v: Option<u32>) {
        self.max_uploads
            .store(v.unwrap_or(NONE_U32), Ordering::Relaxed)
    }

    pub fn peer_limit(&self) -> Option<usize> {
        match self.peer_limit.load(Ordering::Relaxed) {
            NONE_USIZE => None,
            v => Some(v),
        }
    }

    pub fn set_peer_limit(&self, v: Option<usize>) {
        self.peer_limit
            .store(v.unwrap_or(NONE_USIZE), Ordering::Relaxed)
    }

    pub fn wasted_bytes(&self) -> u64 {
        self.wasted_failed_bytes.load(Ordering::Relaxed)
            + self.wasted_redundant_bytes.load(Ordering::Relaxed)
    }

    pub fn queue_pending_peer(&self, addr: SocketAddr, source: PeerSource) -> bool {
        let mut g = self.pending_peers.lock();
        if g.len() >= MAX_PENDING_PEERS || g.iter().any(|(a, _)| *a == addr) {
            return false;
        }
        g.push((addr, source));
        true
    }

    pub fn take_pending_peers(&self) -> Vec<(SocketAddr, PeerSource)> {
        std::mem::take(&mut *self.pending_peers.lock())
    }
}
