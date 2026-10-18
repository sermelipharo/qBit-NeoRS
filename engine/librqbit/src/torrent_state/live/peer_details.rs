//! Detailed per-peer information for peer lists (qBittorrent's Peers tab).

use std::{
    net::SocketAddr,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use serde::Serialize;

use crate::{peer_client::client_name_from_peer_id, stream_connect::ConnectionKind};

use super::{
    TorrentStateLive,
    availability::{count_ones, count_peer_has_we_lack},
};

pub use crate::peer_source::{PeerSource, PeerSources};

/// Peer state flags (libtorrent `peer_info` flags used by qBittorrent).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PeerFlags {
    /// We are interested in the peer ("interesting").
    pub am_interested: bool,
    /// The peer is interested in us.
    pub peer_interested: bool,
    /// We are choking the peer (it can't download from us).
    pub am_choking: bool,
    /// The peer is choking us ("remote choked").
    pub peer_choking: bool,
    /// The peer holds an optimistic unchoke slot.
    pub optimistic_unchoke: bool,
    /// We have outstanding requests and the peer sent no data for 20 s.
    pub snubbed: bool,
    /// The peer connected to us.
    pub incoming: bool,
    /// The connection uses uTP.
    pub utp: bool,
    /// The payload is encrypted with MSE/PE (RC4) — qBittorrent's "E".
    pub encrypted: bool,
    /// The handshake was obfuscated but the payload is plaintext —
    /// qBittorrent's "e". Never set: like qBittorrent (`allowed_enc_level =
    /// pe_rc4`) only RC4 is negotiated.
    pub encrypted_handshake: bool,
    /// The connection was established with NAT hole punching. Always false.
    pub holepunched: bool,
    /// The peer has the whole torrent.
    pub seed: bool,
    /// We are super seeding to this peer.
    pub super_seeding: bool,
}

/// One connected peer.
#[derive(Debug, Clone, Serialize)]
pub struct PeerDetails {
    pub addr: SocketAddr,
    /// TCP, uTP, proxied TCP (`Socks`) or I2P.
    pub connection: ConnectionKind,
    /// Client name: the extended handshake `v` string when the peer sent one,
    /// otherwise decoded from the peer id (e.g. "qBittorrent 5.2.4").
    pub client: String,
    /// The raw 20-byte peer id from the handshake.
    pub peer_id: [u8; 20],
    pub flags: PeerFlags,
    pub sources: PeerSources,
    /// Fraction (0..=1) of pieces the peer has.
    pub progress: f64,
    /// Payload rates in bytes/s (5 second moving average).
    pub download_rate: u64,
    pub upload_rate: u64,
    /// Payload bytes downloaded from / uploaded to this peer (all connections
    /// in this live session of the torrent).
    pub downloaded: u64,
    pub uploaded: u64,
    /// Pieces the peer has that we don't, over the pieces we don't have
    /// (0 when we have everything).
    pub relevance: f64,
    /// Indices of the files covered by the pieces being requested from this
    /// peer right now (sorted).
    pub downloading_files: Vec<usize>,
}

/// A peer with outstanding requests that sent no data for this long is snubbed
/// (libtorrent's `piece_timeout`).
const SNUB_TIMEOUT: Duration = Duration::from_secs(20);

impl TorrentStateLive {
    /// Detailed information about every live (handshaken) peer.
    pub fn peers_detailed(&self) -> Vec<PeerDetails> {
        let total = self.lengths.total_pieces() as usize;
        // Copy our bitfield first: the state lock must not be held while the
        // peer table is locked.
        let have: Vec<u8> = self
            .lock_read("peers_detailed")
            .get_chunks()
            .map(|c| c.get_have_pieces().as_bytes().to_vec())
            .unwrap_or_default();
        let our_missing = total.saturating_sub(count_ones(&have, total) as usize);
        let now = Instant::now();

        self.peers
            .states
            .iter()
            .filter_map(|e| {
                let peer = e.value();
                let live = peer.get_live()?;
                let raw = live.bitfield.as_raw_slice();
                let peer_pieces = count_ones(raw, total);
                let relevance = if our_missing == 0 {
                    0.0
                } else {
                    f64::from(count_peer_has_we_lack(raw, &have, total)) / our_missing as f64
                };
                let counters = &peer.stats.counters;
                let inflight = live.requested_inflight_count();
                Some(PeerDetails {
                    addr: *e.key(),
                    connection: live.connection_kind,
                    client: live
                        .client_name
                        .clone()
                        .unwrap_or_else(|| client_name_from_peer_id(&live.peer_id.0)),
                    peer_id: live.peer_id.0,
                    flags: PeerFlags {
                        am_interested: live.am_interested,
                        peer_interested: live.peer_interested,
                        am_choking: live.am_choking,
                        peer_choking: live.peer_choking,
                        optimistic_unchoke: live.optimistic && !live.am_choking,
                        snubbed: inflight > 0
                            && now.saturating_duration_since(live.last_data_received)
                                > SNUB_TIMEOUT,
                        incoming: live.incoming,
                        utp: matches!(live.connection_kind, ConnectionKind::Utp),
                        encrypted: live.encrypted,
                        encrypted_handshake: false,
                        holepunched: false,
                        seed: live.seeder,
                        super_seeding: live.super_seeding,
                    },
                    sources: peer.sources,
                    progress: if total == 0 {
                        0.0
                    } else {
                        f64::from(peer_pieces) / total as f64
                    },
                    download_rate: live.rates.down_bps,
                    upload_rate: live.rates.up_bps,
                    downloaded: counters.fetched_bytes.load(Ordering::Relaxed),
                    uploaded: counters.uploaded_bytes.load(Ordering::Relaxed),
                    relevance,
                    downloading_files: self
                        .files_of_pieces(live.inflight_requests().map(|r| r.piece_index.get())),
                })
            })
            .collect()
    }

    /// Sorted indices of the (non-padding) files overlapping the given pieces.
    pub(crate) fn files_of_pieces(&self, pieces: impl Iterator<Item = u32>) -> Vec<usize> {
        let mut pieces: Vec<u32> = pieces.collect();
        pieces.sort_unstable();
        pieces.dedup();
        let files = &self.metadata.file_infos;
        let mut out = Vec::new();
        for p in pieces {
            // File piece ranges are ordered, so their ends are non-decreasing.
            let first = files.partition_point(|fi| fi.piece_range.end <= p);
            for (idx, fi) in files.iter().enumerate().skip(first) {
                if fi.piece_range.start > p {
                    break;
                }
                if !fi.attrs.padding && fi.piece_range.contains(&p) {
                    out.push(idx);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}
