//! BEP 52 hashes of a live torrent: serving `hash request`s of pure v2 and
//! hybrid torrents (piece-layer hashes with their merkle proofs, so that
//! peers that joined through a magnet link can verify pieces), and fetching
//! missing piece layers from a v2 peer: those of a hybrid torrent that has
//! none (added by a magnet link), so that it can serve them too, and those
//! a pure v2 .torrent lacked, file by file, so that each file can be
//! verified (and downloaded) as soon as its layer is known (see
//! `missing_layers`).
//!
//! Requests come from untrusted peers: only the piece layer is served (other
//! layers are rejected), at most 512 hashes per request, and the per-file
//! upper tree that makes a request cheap is built once and cached (about
//! 1/256 of the layer's size). Fetched hashes are only accepted as replies
//! to requests we sent to the one peer fetching, and only when they verify
//! against the file's `pieces root` (see `LayerFetch`). One peer of a
//! torrent fetches at a time and every byte held is charged to the
//! session's `FetchBudget`.

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use librqbit_core::{
    torrent_metainfo::{ValidatedTorrentMetaV1Info, torrent_from_bytes},
    v2::{
        Hash32, LayerTree, V2FileSpan, V2Layout, parse_file_tree, parse_piece_layers,
        zero_subtree,
    },
};
use parking_lot::{Mutex, RwLock};
use peer_binary_protocol::HashRequest;

use crate::{
    ByteBufOwned,
    peer_info_reader::v2_layers::{BudgetExhausted, FetchBudget, LayerFetch, Wanted},
    spawn_utils::BlockingSpawner,
};

/// A fetch peer that hasn't completed in this time is given up.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Peers remembered as having failed a fetch (then forgotten all at once).
const MAX_FAILED_PEERS: usize = 64;

// `fetch_wanted` starts at 0: not known yet.
const FETCH_WANTED: u8 = 1;
const FETCH_NOT_NEEDED: u8 = 2;

/// What a hash-related call needs from the torrent.
pub(crate) struct V2Ctx<'a> {
    pub info: &'a ValidatedTorrentMetaV1Info<ByteBufOwned>,
    pub torrent_bytes: &'a [u8],
    /// Layers fetched earlier in this session (they survive pause/resume).
    pub fetched: Option<&'a [u8]>,
    pub spawner: &'a BlockingSpawner,
    /// The session's budget for layers being fetched.
    pub budget: &'a Arc<FetchBudget>,
}

/// A hybrid torrent's v2 half (it runs on its v1 pieces).
struct Hybrid {
    layout: Arc<V2Layout>,
    /// Expected hash of every piece: from the .torrent's piece layers or a
    /// completed fetch; `None` until known.
    hashes: RwLock<Option<Arc<[Hash32]>>>,
}

#[derive(Default)]
enum Fetch {
    #[default]
    Idle,
    Active {
        peer: SocketAddr,
        since: Instant,
        fetch: LayerFetch,
    },
}

#[derive(Default)]
struct FetchState {
    fetch: Fetch,
    failed: HashSet<SocketAddr>,
}

#[derive(Default)]
pub(crate) struct V2HashServer {
    /// pieces root → index into the layout's spans of a file with a piece
    /// layer (identical files share it).
    files: OnceLock<HashMap<Hash32, usize>>,
    trees: Mutex<HashMap<Hash32, Arc<LayerTree>>>,
    /// Built from the file tree on first use; `None` for v1 and pure v2
    /// torrents and unusable trees.
    hybrid: OnceLock<Option<Hybrid>>,
    /// Whether a hybrid's layers are still to be fetched (FETCH_*).
    fetch_wanted: AtomicU8,
    fetch: Mutex<FetchState>,
}

/// What a `hashes` / `hash reject` from the fetching peer led to.
#[derive(Default)]
pub(crate) struct FetchProgress {
    /// Hash requests to send to the peer.
    pub requests: Vec<HashRequest>,
    /// A hybrid's layers are all known now: the bencoded `piece layers` to
    /// keep.
    pub done: Option<Bytes>,
    /// A pure v2 torrent: the files (indexes into the layout's spans)
    /// whose layer just became known (verified and added to the piece
    /// hashes).
    pub known_spans: Vec<usize>,
}

impl V2HashServer {
    fn hybrid(&self, ctx: &V2Ctx<'_>) -> Option<&Hybrid> {
        self.hybrid
            .get_or_init(|| {
                let i = ctx.info.info();
                if ctx.info.is_pure_v2() || i.meta_version != Some(2) {
                    self.fetch_wanted.store(FETCH_NOT_NEEDED, Ordering::Relaxed);
                    return None;
                }
                let r = ctx.spawner.block_in_place(|| {
                    let layout =
                        parse_file_tree(i.file_tree.as_ref()?.0.as_ref(), i.piece_length).ok()?;
                    let torrent = torrent_from_bytes(ctx.torrent_bytes).ok();
                    let from_torrent = torrent
                        .as_ref()
                        .and_then(|t| t.piece_layers.as_ref())
                        .map(|l| l.0.as_ref());
                    let hashes = [from_torrent, ctx.fetched]
                        .into_iter()
                        .flatten()
                        .find_map(|l| parse_piece_layers(Some(l), &layout).ok())
                        // Only files no longer than a piece: no layers.
                        .or_else(|| parse_piece_layers(None, &layout).ok());
                    Some(Hybrid {
                        layout: Arc::new(layout),
                        hashes: RwLock::new(hashes.map(Arc::from)),
                    })
                });
                let wanted = r.as_ref().is_some_and(|h| h.hashes.read().is_none());
                self.fetch_wanted.store(
                    if wanted {
                        FETCH_WANTED
                    } else {
                        FETCH_NOT_NEEDED
                    },
                    Ordering::Relaxed,
                );
                r
            })
            .as_ref()
    }

    /// The `hashes` payload for `req`, or `None` to reject it.
    pub fn reply(&self, ctx: &V2Ctx<'_>, req: &HashRequest) -> Option<Bytes> {
        if let (Some(layout), Some(hashes)) = (ctx.info.v2_layout(), ctx.info.v2_piece_hashes()) {
            // Pure v2: only files whose layer is known are served.
            let span = self.span_of_root(layout, req)?;
            return hashes
                .with_span(layout, span, |layer| self.serve(ctx, layout, req, layer))
                .flatten();
        }
        let h = self.hybrid(ctx)?;
        let hashes = h.hashes.read().clone()?;
        let span = self.span_of_root(&h.layout, req)?;
        let s = h.layout.spans.get(span)?;
        let layer =
            hashes.get(s.first_piece as usize..(s.first_piece + s.num_pieces) as usize)?;
        self.serve(ctx, &h.layout, req, layer)
    }

    /// The file a request of the piece layer is for (`None` for another
    /// layer or an unknown root).
    fn span_of_root(&self, layout: &V2Layout, req: &HashRequest) -> Option<usize> {
        if req.base_layer != layout.blocks_per_piece().trailing_zeros() {
            return None;
        }
        let files = self.files.get_or_init(|| {
            let pl = u64::from(layout.piece_length);
            layout
                .spans
                .iter()
                .enumerate()
                .filter(|(_, s)| s.len > pl)
                .map(|(i, s)| (s.pieces_root, i))
                .collect()
        });
        files.get(&req.pieces_root).copied()
    }

    /// The `hashes` payload for `req` from `layer`, the file's piece layer.
    fn serve(
        &self,
        ctx: &V2Ctx<'_>,
        layout: &V2Layout,
        req: &HashRequest,
        layer: &[Hash32],
    ) -> Option<Bytes> {
        let piece_layer = layout.blocks_per_piece().trailing_zeros();
        let cached = self.trees.lock().get(&req.pieces_root).cloned();
        let tree = match cached {
            Some(t) => t,
            None => {
                // Up to one hash per piece of the file: off the async thread.
                let t = Arc::new(
                    ctx.spawner
                        .block_in_place(|| LayerTree::new(layer, zero_subtree(piece_layer))),
                );
                self.trees.lock().insert(req.pieces_root, t.clone());
                t
            }
        };
        tree.hashes(layer, req.index, req.length, req.proof_layers)
            .map(Bytes::from)
    }

    /// Cheap check before [`Self::maybe_start_fetch`]: false once the
    /// layers are known (or for torrents that need none).
    pub fn fetch_wanted(&self) -> bool {
        self.fetch_wanted.load(Ordering::Relaxed) != FETCH_NOT_NEEDED
    }

    /// What to fetch now: the layout and the files, or `None` when nothing
    /// is missing (then no fetch is wanted any more). A pure v2 torrent
    /// fetches the layers of the files `wanted` first (priority above 0),
    /// the others once those are known.
    fn fetch_target(
        &self,
        ctx: &V2Ctx<'_>,
        wanted: &dyn Fn(&V2FileSpan) -> bool,
    ) -> Option<(Arc<V2Layout>, Wanted)> {
        if ctx.info.is_pure_v2() {
            let layout = ctx.info.v2_layout_arc()?;
            let hashes = ctx.info.v2_piece_hashes();
            if hashes.is_none_or(|h| h.is_complete()) {
                self.fetch_wanted.store(FETCH_NOT_NEEDED, Ordering::Relaxed);
                return None;
            }
            let hashes = hashes?;
            let (mut first, mut rest) = (Vec::new(), Vec::new());
            for (i, s) in layout.spans.iter().enumerate() {
                if !hashes.span_known(i) {
                    if wanted(s) { &mut first } else { &mut rest }.push(s.pieces_root);
                }
            }
            let mut roots = if first.is_empty() { rest } else { first };
            roots.sort_unstable();
            roots.dedup();
            return Some((layout, Wanted::Only(roots.into())));
        }
        let hybrid = self.hybrid(ctx)?;
        if hybrid.hashes.read().is_some() {
            return None;
        }
        Some((hybrid.layout.clone(), Wanted::All))
    }

    /// A message from `peer`, a v2 connection of a hybrid torrent or of a
    /// pure v2 torrent: if layers are missing and no other peer is fetching
    /// them (or that one timed out), this peer fetches them. Returns the
    /// requests to send.
    pub fn maybe_start_fetch(
        &self,
        ctx: &V2Ctx<'_>,
        peer: SocketAddr,
        wanted: &dyn Fn(&V2FileSpan) -> bool,
    ) -> Vec<HashRequest> {
        if self.fetch_wanted.load(Ordering::Relaxed) == FETCH_NOT_NEEDED {
            return Vec::new();
        }
        if ctx.info.is_pure_v2() && ctx.info.has_piece_hashes() {
            self.fetch_wanted.store(FETCH_NOT_NEEDED, Ordering::Relaxed);
            return Vec::new();
        }
        let mut st = self.fetch.lock();
        if let Fetch::Active { peer: p, since, .. } = &st.fetch {
            if since.elapsed() < FETCH_TIMEOUT {
                return Vec::new();
            }
            let p = *p;
            st.fetch = Fetch::Idle;
            Self::mark_failed(&mut st, p);
        }
        if st.failed.contains(&peer) {
            return Vec::new();
        }
        let Some((layout, what)) = self.fetch_target(ctx, wanted) else {
            return Vec::new();
        };
        let Some(mut fetch) = LayerFetch::new(layout, what, ctx.budget.clone()) else {
            return Vec::new();
        };
        let Ok(requests) = fetch.next_requests() else {
            // Budget exhausted: try again with a later message.
            return Vec::new();
        };
        st.fetch = Fetch::Active {
            peer,
            since: Instant::now(),
            fetch,
        };
        requests
    }

    fn mark_failed(st: &mut FetchState, peer: SocketAddr) {
        if st.failed.len() >= MAX_FAILED_PEERS {
            st.failed.clear();
        }
        st.failed.insert(peer);
    }

    /// `hashes` (`Some(data)`) or `hash reject` (`None`) from `peer`.
    /// Replies from peers that aren't fetching are ignored; a reply that
    /// doesn't match a request we sent or doesn't verify is an error (the
    /// peer is then dropped), a reject ends this peer's fetch.
    pub fn on_fetch_reply(
        &self,
        ctx: &V2Ctx<'_>,
        peer: SocketAddr,
        req: &HashRequest,
        data: Option<&[u8]>,
    ) -> anyhow::Result<FetchProgress> {
        let mut st = self.fetch.lock();
        let Fetch::Active { peer: p, fetch, .. } = &mut st.fetch else {
            return Ok(FetchProgress::default());
        };
        if *p != peer {
            return Ok(FetchProgress::default());
        }
        let r = match data {
            Some(d) => fetch.on_hashes(req, d),
            None => fetch.on_reject(req),
        };
        if let Err(e) = r {
            st.fetch = Fetch::Idle;
            if e.is::<BudgetExhausted>() {
                // Not the peer's fault: start over later.
                return Ok(FetchProgress::default());
            }
            Self::mark_failed(&mut st, peer);
            // A peer without the layers may reject; anything else is a
            // protocol violation.
            return match data {
                None => Ok(FetchProgress::default()),
                Some(_) => Err(e),
            };
        }
        // A pure v2 torrent takes each file's layer as soon as it is whole.
        let mut known_spans = Vec::new();
        if let (Some(layout), Some(hashes), Some(_)) =
            (ctx.info.v2_layout(), ctx.info.v2_piece_hashes(), data)
            && let Some(layer) = fetch.take_complete(&req.pieces_root)
        {
            // Up to one hash per piece of the file: off the async thread.
            match ctx
                .spawner
                .block_in_place(|| hashes.set_layer(layout, &req.pieces_root, &layer))
            {
                Ok(spans) => known_spans = spans,
                Err(e) => {
                    // Every chunk verified on its own: can't happen.
                    st.fetch = Fetch::Idle;
                    Self::mark_failed(&mut st, peer);
                    return Err(e.into());
                }
            }
        }
        let requests = match fetch.next_requests() {
            Ok(r) => r,
            Err(_) => {
                // Out of budget: not this peer's fault; what it fetched is
                // released, and a later message starts over.
                st.fetch = Fetch::Idle;
                return Ok(FetchProgress {
                    known_spans,
                    ..Default::default()
                });
            }
        };
        if !fetch.is_done() {
            return Ok(FetchProgress {
                requests,
                done: None,
                known_spans,
            });
        }
        let Fetch::Active { fetch, .. } = std::mem::take(&mut st.fetch) else {
            return Ok(FetchProgress::default());
        };
        drop(st);
        if ctx.info.is_pure_v2() {
            // Files left (not wanted so far) are fetched by a later message.
            if ctx.info.has_piece_hashes() {
                self.fetch_wanted.store(FETCH_NOT_NEEDED, Ordering::Relaxed);
            }
            return Ok(FetchProgress {
                known_spans,
                ..Default::default()
            });
        }
        let layers = fetch.into_piece_layers();
        let Some(hybrid) = self.hybrid(ctx) else {
            return Ok(FetchProgress::default());
        };
        // Every reply verified against its root; this checks the whole
        // dictionary once more and builds the piece hash table.
        let hashes = ctx
            .spawner
            .block_in_place(|| parse_piece_layers(Some(&layers), &hybrid.layout))?;
        *hybrid.hashes.write() = Some(Arc::from(hashes));
        self.fetch_wanted.store(FETCH_NOT_NEEDED, Ordering::Relaxed);
        Ok(FetchProgress {
            requests: Vec::new(),
            done: Some(layers),
            known_spans: Vec::new(),
        })
    }

    /// `peer` disconnected: another peer may fetch.
    pub fn on_peer_gone(&self, peer: SocketAddr) {
        if self.fetch_wanted.load(Ordering::Relaxed) == FETCH_NOT_NEEDED {
            return;
        }
        let mut st = self.fetch.lock();
        if matches!(&st.fetch, Fetch::Active { peer: p, .. } if *p == peer) {
            st.fetch = Fetch::Idle;
        }
    }
}

#[cfg(test)]
mod tests {
    use buffers::ByteBuf;
    use clone_to_owned::CloneToOwned;
    use librqbit_core::{
        torrent_metainfo::TorrentMetaV1Info,
        v2::{merkle_root, sha256},
    };

    use super::*;

    const PIECE: u64 = 16 * 1024;

    /// A single-file hybrid info dict of `pieces` pieces (made-up hashes).
    fn hybrid(
        pieces: u32,
    ) -> (
        ValidatedTorrentMetaV1Info<ByteBufOwned>,
        Vec<Hash32>,
        Hash32,
    ) {
        let layer: Vec<Hash32> = (0..pieces).map(|i| sha256(&i.to_be_bytes())).collect();
        let root = merkle_root(&layer, 1, zero_subtree(0));
        let len = (u64::from(pieces) * PIECE).to_string();
        let mut info = b"d9:file treed1:xd0:d6:lengthi".to_vec();
        info.extend_from_slice(len.as_bytes());
        info.extend_from_slice(b"e11:pieces root32:");
        info.extend_from_slice(&root);
        info.extend_from_slice(b"eee6:lengthi");
        info.extend_from_slice(len.as_bytes());
        info.extend_from_slice(b"e12:meta versioni2e4:name1:x12:piece lengthi16384e6:pieces");
        info.extend_from_slice(format!("{}:", pieces * 20).as_bytes());
        info.extend(std::iter::repeat_n(7u8, pieces as usize * 20));
        info.push(b'e');
        let bytes = bytes::Bytes::from(info);
        let i: TorrentMetaV1Info<ByteBuf> = bencode::from_bytes(&bytes).unwrap();
        let v = i.clone_to_owned(Some(&bytes)).validate().unwrap();
        assert!(!v.is_pure_v2());
        (v, layer, root)
    }

    fn reply(layer: &[Hash32], r: &HashRequest) -> Vec<u8> {
        LayerTree::new(layer, zero_subtree(0))
            .hashes(layer, r.index, r.length, r.proof_layers)
            .unwrap()
    }

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_hybrid_fetches_its_layers_from_one_peer_and_serves_them() {
        let (info, layer, root) = hybrid(1500);
        let spawner = BlockingSpawner::new(1);
        let ctx = V2Ctx {
            info: &info,
            torrent_bytes: b"de",
            fetched: None,
            spawner: &spawner,
            budget: &FetchBudget::new(u64::MAX),
        };
        let s = V2HashServer::default();
        let probe = HashRequest {
            pieces_root: root,
            base_layer: 0,
            index: 0,
            length: 2,
            proof_layers: 0,
        };
        // No layers yet: requests are rejected.
        assert!(s.reply(&ctx, &probe).is_none());
        assert!(s.fetch_wanted());

        let reqs = s.maybe_start_fetch(&ctx, addr(1), &|_| true);
        assert!(!reqs.is_empty());
        // One peer at a time.
        assert!(s.maybe_start_fetch(&ctx, addr(2), &|_| true).is_empty());
        // Replies from another peer are ignored; an unsolicited one from the
        // fetching peer fails it.
        let good = reply(&layer, &reqs[0]);
        let p = s
            .on_fetch_reply(&ctx, addr(2), &reqs[0], Some(&good))
            .unwrap();
        assert!(p.requests.is_empty() && p.done.is_none());
        let mut other = reqs[0];
        other.index += 512 * 2;
        assert!(
            s.on_fetch_reply(&ctx, addr(1), &other, Some(&good))
                .is_err()
        );
        // Peer 1 failed: it doesn't fetch again; peer 2 can.
        assert!(s.maybe_start_fetch(&ctx, addr(1), &|_| true).is_empty());
        let reqs = s.maybe_start_fetch(&ctx, addr(2), &|_| true);
        assert!(!reqs.is_empty());
        // A tampered reply fails peer 2 too.
        let mut bad = reply(&layer, &reqs[0]);
        bad[5] ^= 1;
        assert!(
            s.on_fetch_reply(&ctx, addr(2), &reqs[0], Some(&bad))
                .is_err()
        );
        // A reject ends peer 3's fetch quietly; a disconnect frees the slot.
        let reqs = s.maybe_start_fetch(&ctx, addr(3), &|_| true);
        let p = s.on_fetch_reply(&ctx, addr(3), &reqs[0], None).unwrap();
        assert!(p.done.is_none());
        assert!(s.maybe_start_fetch(&ctx, addr(3), &|_| true).is_empty());
        assert!(!s.maybe_start_fetch(&ctx, addr(4), &|_| true).is_empty());
        s.on_peer_gone(addr(4));

        // An honest peer completes the fetch (replies in any order).
        let mut pending = s.maybe_start_fetch(&ctx, addr(5), &|_| true);
        let mut done = None;
        while let Some(r) = pending.pop() {
            let p = s
                .on_fetch_reply(&ctx, addr(5), &r, Some(&reply(&layer, &r)))
                .unwrap();
            pending.extend(p.requests);
            if p.done.is_some() {
                done = p.done;
            }
        }
        let layers = done.expect("fetch completed");
        let mut expected = b"d32:".to_vec();
        expected.extend_from_slice(&root);
        expected.extend_from_slice(format!("{}:", 1500 * 32).as_bytes());
        expected.extend(layer.concat());
        expected.push(b'e');
        assert_eq!(&layers[..], &expected[..]);
        assert!(!s.fetch_wanted());
        assert!(s.maybe_start_fetch(&ctx, addr(4), &|_| true).is_empty());
        // Served now.
        assert_eq!(
            &s.reply(&ctx, &probe).unwrap()[..],
            &reply(&layer, &probe)[..]
        );

        // After a pause the layers come from the fetched copy.
        let ctx2 = V2Ctx {
            fetched: Some(&layers),
            ..ctx
        };
        let s2 = V2HashServer::default();
        assert!(s2.reply(&ctx2, &probe).is_some());
        assert!(s2.maybe_start_fetch(&ctx2, addr(1), &|_| true).is_empty());
    }

    /// A fetch that would go over the session's budget stops without
    /// allocating more; the peer isn't blamed and the memory is returned.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_hybrid_fetch_stays_within_the_budget() {
        let (info, layer, _) = hybrid(1500);
        let spawner = BlockingSpawner::new(1);
        let budget = FetchBudget::new(512 * 32);
        let ctx = V2Ctx {
            info: &info,
            torrent_bytes: b"de",
            fetched: None,
            spawner: &spawner,
            budget: &budget,
        };
        let s = V2HashServer::default();
        let reqs = s.maybe_start_fetch(&ctx, addr(1), &|_| true);
        assert!(!reqs.is_empty());
        let r = s
            .on_fetch_reply(&ctx, addr(1), &reqs[0], Some(&reply(&layer, &reqs[0])))
            .unwrap();
        assert!(r.requests.is_empty() && r.done.is_none());
        assert!(budget.used() <= 512 * 32);
        assert_eq!(budget.used(), 0, "the stopped fetch released its memory");
        // Not marked as failed: it may try again.
        assert!(!s.maybe_start_fetch(&ctx, addr(1), &|_| true).is_empty());
    }

    /// A pure v2 info dict with files `a` (layer in the .torrent), `b`
    /// (700 pieces) and `c` (3 pieces) without their layers, and `d`
    /// (smaller than a piece); made-up hashes.
    fn pure_v2_partial() -> (
        ValidatedTorrentMetaV1Info<ByteBufOwned>,
        [(Vec<Hash32>, Hash32); 3],
    ) {
        let file = |pieces: u32, seed: u8| {
            let layer: Vec<Hash32> = (0..pieces)
                .map(|i| sha256(&[&i.to_be_bytes()[..], &[seed]].concat()))
                .collect();
            let root = merkle_root(&layer, 1, zero_subtree(0));
            (layer, root)
        };
        let files = [file(5, 1), file(700, 2), file(3, 3)];
        let mut info = b"d9:file treed".to_vec();
        for (name, (layer, root)) in ["a", "b", "c"].iter().zip(&files) {
            info.extend_from_slice(format!("1:{name}d0:d6:lengthi{}e", layer.len() as u64 * PIECE).as_bytes());
            info.extend_from_slice(b"11:pieces root32:");
            info.extend_from_slice(root);
            info.extend_from_slice(b"ee");
        }
        info.extend_from_slice(b"1:dd0:d6:lengthi5e11:pieces root32:");
        info.extend_from_slice(&[7; 32]);
        info.extend_from_slice(b"eee12:meta versioni2e4:name1:x12:piece lengthi16384ee");
        let mut layers = b"d32:".to_vec();
        layers.extend_from_slice(&files[0].1);
        layers.extend_from_slice(format!("{}:", 5 * 32).as_bytes());
        layers.extend(files[0].0.concat());
        layers.push(b'e');
        let bytes = bytes::Bytes::from(info);
        let i: TorrentMetaV1Info<ByteBuf> = bencode::from_bytes(&bytes).unwrap();
        let v = i
            .clone_to_owned(Some(&bytes))
            .validate()
            .unwrap()
            .with_partial_piece_layers(Some(&layers))
            .unwrap();
        assert!(v.is_pure_v2() && !v.has_piece_hashes());
        (v, files)
    }

    /// A pure v2 torrent fetches the layers its .torrent lacked file by
    /// file, those of wanted files first; each is usable (served,
    /// verifiable) as soon as it is whole, and the budget is returned.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pure_v2_torrent_fetches_missing_layers_file_by_file() {
        let (info, files) = pure_v2_partial();
        let [(a, a_root), (b, b_root), (c, c_root)] = &files;
        let layout = info.v2_layout().unwrap();
        let hashes = info.v2_piece_hashes().unwrap();
        let spawner = BlockingSpawner::new(1);
        let budget = FetchBudget::new(u64::MAX);
        let ctx = V2Ctx {
            info: &info,
            torrent_bytes: b"de",
            fetched: None,
            spawner: &spawner,
            budget: &budget,
        };
        let s = V2HashServer::default();
        let probe = |root: &Hash32| HashRequest {
            pieces_root: *root,
            base_layer: 0,
            index: 0,
            length: 2,
            proof_layers: 0,
        };
        // Only known layers are served.
        assert_eq!(
            &s.reply(&ctx, &probe(a_root)).unwrap()[..],
            &reply(a, &probe(a_root))[..]
        );
        assert!(s.reply(&ctx, &probe(b_root)).is_none());
        assert!(s.fetch_wanted());

        // `c` is wanted, `b` isn't (priority 0): `c` comes first.
        let c_wanted = |sp: &V2FileSpan| sp.pieces_root == *c_root;
        let reqs = s.maybe_start_fetch(&ctx, addr(1), &c_wanted);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].pieces_root, *c_root);
        let p = s
            .on_fetch_reply(&ctx, addr(1), &reqs[0], Some(&reply(c, &reqs[0])))
            .unwrap();
        assert_eq!(p.known_spans, vec![2]);
        assert!(p.requests.is_empty() && p.done.is_none());
        assert!(hashes.span_known(2) && !hashes.span_known(1));
        assert!(info.piece_check(layout.spans[2].first_piece).is_some());
        assert!(info.piece_check(layout.spans[1].first_piece).is_none());
        assert_eq!(budget.used(), 0, "the finished fetch held nothing");
        assert!(s.fetch_wanted(), "b is still missing");

        // Then `b`, though unwanted (several requests, any order).
        let mut pending = s.maybe_start_fetch(&ctx, addr(2), &c_wanted);
        assert!(pending.iter().all(|r| r.pieces_root == *b_root));
        let mut known = Vec::new();
        while let Some(r) = pending.pop() {
            assert!(!hashes.span_known(1), "not before the whole layer");
            let p = s
                .on_fetch_reply(&ctx, addr(2), &r, Some(&reply(b, &r)))
                .unwrap();
            pending.extend(p.requests);
            known.extend(p.known_spans);
        }
        assert_eq!(known, vec![1]);
        assert!(info.has_piece_hashes());
        assert!(!s.fetch_wanted());
        assert!(s.maybe_start_fetch(&ctx, addr(3), &|_| true).is_empty());
        assert_eq!(
            &s.reply(&ctx, &probe(b_root)).unwrap()[..],
            &reply(b, &probe(b_root))[..]
        );
        assert_eq!(budget.used(), 0);
        // What the owner saves: every layer.
        let all = hashes.piece_layers_bencode(layout);
        assert!(parse_piece_layers(Some(&all), layout).is_ok());
    }

    /// A peer whose fetch fails keeps what it already completed: files
    /// whose layer arrived stay known, the next peer fetches the rest.
    #[tokio::test(flavor = "multi_thread")]
    async fn completed_files_survive_a_failed_fetch() {
        let (info, files) = pure_v2_partial();
        let [_, (b, b_root), (c, c_root)] = &files;
        let hashes = info.v2_piece_hashes().unwrap();
        let spawner = BlockingSpawner::new(1);
        let budget = FetchBudget::new(u64::MAX);
        let ctx = V2Ctx {
            info: &info,
            torrent_bytes: b"de",
            fetched: None,
            spawner: &spawner,
            budget: &budget,
        };
        let s = V2HashServer::default();
        let mut reqs = s.maybe_start_fetch(&ctx, addr(1), &|_| true);
        // b's chunks (2) and c's (1) are requested in file order.
        assert_eq!(reqs.len(), 3);
        let rc = reqs.pop().unwrap();
        assert_eq!(rc.pieces_root, *c_root);
        let p = s
            .on_fetch_reply(&ctx, addr(1), &rc, Some(&reply(c, &rc)))
            .unwrap();
        assert_eq!(p.known_spans, vec![2]);
        // A tampered reply for b: the peer is dropped, c stays known.
        let mut bad = reply(b, &reqs[0]);
        bad[0] ^= 1;
        assert!(
            s.on_fetch_reply(&ctx, addr(1), &reqs[0], Some(&bad))
                .is_err()
        );
        assert!(hashes.span_known(2) && !hashes.span_known(1));
        assert_eq!(budget.used(), 0);
        // Peer 2 fetches only b now.
        let reqs = s.maybe_start_fetch(&ctx, addr(2), &|_| true);
        assert!(!reqs.is_empty());
        assert!(reqs.iter().all(|r| r.pieces_root == *b_root));
    }
}
