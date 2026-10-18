//! Fetching piece layers from a peer (BEP 52 `hash request` / `hashes` /
//! `hash reject`): for a pure v2 torrent added by a `btmh` magnet link right
//! after its metadata arrived over the same connection, and while a torrent
//! runs: for a pure v2 .torrent without (all of) its `piece layers` (each
//! file's layer handed out as soon as it is whole) and for a hybrid torrent
//! added by a magnet link (so that it can answer hash requests).
//!
//! Every reply must match a request we sent and carry a merkle proof up to
//! the file's `pieces root` from the (SHA-256 verified) metadata. Requests
//! are at most 512 hashes, a few are outstanding at a time, and requests
//! are generated as the fetch goes (nothing is allocated per file up
//! front). Memory grows only with hashes that verified, and every byte of
//! it (hashes and per-file bookkeeping) is first reserved from a
//! session-wide [`FetchBudget`]: a fetch that would exceed it fails instead
//! of allocating.

use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context, bail};
use bytes::Bytes;
use librqbit_core::v2::{
    Hash32, V2Layout, layer_tree_height, proof_layers_to_root, verify_layer_hashes,
};
use peer_binary_protocol::{HashRequest, MAX_HASH_REQUEST_LENGTH};

pub(crate) use super::budget::{BudgetExhausted, FetchBudget, FetchReservation};

/// Hash requests in flight to the peer.
const MAX_OUTSTANDING: usize = 4;

/// Bytes charged per file being fetched (map entry and bookkeeping).
const FILE_OVERHEAD: u64 = 128;
/// Bytes charged per verified chunk on top of its hashes.
const CHUNK_OVERHEAD: u64 = 64;

/// Which files' layers to fetch.
#[derive(Clone, Debug)]
pub(crate) enum Wanted {
    All,
    /// These `pieces root`s (sorted).
    Only(Arc<[Hash32]>),
}

impl Wanted {
    fn contains(&self, root: &Hash32) -> bool {
        match self {
            Wanted::All => true,
            Wanted::Only(r) => r.binary_search(root).is_ok(),
        }
    }
}

struct FileLayer {
    num_pieces: u32,
    /// Verified hashes by first index (each chunk at most 512 hashes).
    chunks: BTreeMap<u32, Box<[Hash32]>>,
}

pub(crate) struct LayerFetch {
    layout: Arc<V2Layout>,
    wanted: Wanted,
    base_layer: u32,
    /// Next span to look at, and the next index in the current one.
    span: usize,
    next_index: u32,
    /// Files being (or already) fetched, by root: identical files share a
    /// layer and are fetched once.
    files: BTreeMap<Hash32, FileLayer>,
    outstanding: Vec<HashRequest>,
    reserved: FetchReservation,
}

impl LayerFetch {
    /// Fetches the layers of the files of `layout` longer than a piece
    /// whose `pieces root` is `wanted`, charging `budget`. `None` when there
    /// is nothing to fetch.
    pub fn new(layout: Arc<V2Layout>, wanted: Wanted, budget: Arc<FetchBudget>) -> Option<Self> {
        let pl = u64::from(layout.piece_length);
        let first = layout
            .spans
            .iter()
            .position(|s| s.len > pl && wanted.contains(&s.pieces_root))?;
        Some(Self {
            base_layer: layout.blocks_per_piece().trailing_zeros(),
            layout,
            wanted,
            span: first,
            next_index: 0,
            files: BTreeMap::new(),
            outstanding: Vec::new(),
            reserved: FetchReservation::new(budget),
        })
    }

    /// Moves the cursor past files with nothing left to request (no
    /// layer, not wanted, already fetched, every request out).
    fn advance(&mut self) {
        let pl = u64::from(self.layout.piece_length);
        while let Some(span) = self.layout.spans.get(self.span) {
            let skip = span.len <= pl
                || !self.wanted.contains(&span.pieces_root)
                || (self.next_index == 0 && self.files.contains_key(&span.pieces_root));
            if !skip && self.next_index < span.num_pieces {
                return;
            }
            self.span += 1;
            self.next_index = 0;
        }
    }

    /// The next request, starting the next wanted file when the current
    /// one has every request out.
    fn next_request(&mut self) -> anyhow::Result<Option<HashRequest>> {
        self.advance();
        {
            let Some(span) = self.layout.spans.get(self.span).copied() else {
                return Ok(None);
            };
            if self.next_index == 0 {
                self.reserved.grow(FILE_OVERHEAD)?;
                self.files.insert(
                    span.pieces_root,
                    FileLayer {
                        num_pieces: span.num_pieces,
                        chunks: BTreeMap::new(),
                    },
                );
            }
            let height = layer_tree_height(span.num_pieces);
            let length = (1u32 << height).min(MAX_HASH_REQUEST_LENGTH);
            let r = HashRequest {
                pieces_root: span.pieces_root,
                base_layer: self.base_layer,
                index: self.next_index,
                length,
                proof_layers: proof_layers_to_root(length, height),
            };
            self.next_index += length;
            Ok(Some(r))
        }
    }

    /// Requests to send now (keeps up to MAX_OUTSTANDING in flight). Fails
    /// when the budget can't take another file.
    pub fn next_requests(&mut self) -> anyhow::Result<Vec<HashRequest>> {
        let mut out = Vec::new();
        while self.outstanding.len() < MAX_OUTSTANDING {
            let Some(r) = self.next_request()? else {
                break;
            };
            out.push(r);
            self.outstanding.push(r);
        }
        Ok(out)
    }

    pub fn is_done(&mut self) -> bool {
        self.advance();
        self.span >= self.layout.spans.len() && self.outstanding.is_empty()
    }

    fn take_outstanding(&mut self, req: &HashRequest) -> anyhow::Result<()> {
        let pos = self
            .outstanding
            .iter()
            .position(|r| r == req)
            .context("hashes or hash reject for a request we didn't send")?;
        self.outstanding.swap_remove(pos);
        Ok(())
    }

    /// A `hashes` reply: verified against the file's pieces root, then
    /// kept (charged to the budget).
    pub fn on_hashes(&mut self, req: &HashRequest, data: &[u8]) -> anyhow::Result<()> {
        self.take_outstanding(req)?;
        if data.len() as u64 != req.reply_hashes() * 32 {
            bail!("hashes reply has the wrong number of hashes");
        }
        let all: &[Hash32] = data.as_chunks::<32>().0;
        let (base, uncles) = all.split_at(req.length as usize);
        if !verify_layer_hashes(&req.pieces_root, req.index, base, uncles) {
            bail!("hashes reply doesn't verify against the pieces root");
        }
        let f = self
            .files
            .get_mut(&req.pieces_root)
            .context("bug: request for a file not started")?;
        // Only the file's own hashes are kept (not the padding).
        let n = base
            .len()
            .min(f.num_pieces.saturating_sub(req.index) as usize);
        self.reserved.grow(n as u64 * 32 + CHUNK_OVERHEAD)?;
        f.chunks.insert(req.index, base[..n].into());
        Ok(())
    }

    /// The whole layer of `root` once every chunk of it verified, handed
    /// out once (its memory goes back to the budget). Used by pure v2
    /// torrents to unblock each file as soon as its layer is known.
    pub fn take_complete(&mut self, root: &Hash32) -> Option<Vec<Hash32>> {
        let f = self.files.get_mut(root)?;
        let have: usize = f.chunks.values().map(|c| c.len()).sum();
        if f.chunks.is_empty() || have < f.num_pieces as usize {
            return None;
        }
        let chunks = std::mem::take(&mut f.chunks);
        let charged = have as u64 * 32 + chunks.len() as u64 * CHUNK_OVERHEAD;
        let layer: Vec<Hash32> = chunks.into_values().flat_map(|c| c.into_vec()).collect();
        self.reserved.release(charged);
        Some(layer)
    }

    pub fn on_reject(&mut self, req: &HashRequest) -> anyhow::Result<()> {
        self.take_outstanding(req)?;
        bail!("peer rejected our hash request")
    }

    /// The bencoded `piece layers` dictionary (keys sorted).
    pub fn into_piece_layers(self) -> Bytes {
        let mut out = vec![b'd'];
        for (root, f) in &self.files {
            let len: usize = f.chunks.values().map(|c| c.len()).sum();
            out.extend_from_slice(b"32:");
            out.extend_from_slice(root);
            out.extend_from_slice(format!("{}:", len * 32).as_bytes());
            for c in f.chunks.values() {
                for h in c.iter() {
                    out.extend_from_slice(h);
                }
            }
        }
        out.push(b'e');
        out.into()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use buffers::ByteBuf;
    use librqbit_core::{
        torrent_metainfo::TorrentMetaV1Info,
        v2::{LayerTree, merkle_root, sha256, zero_subtree},
    };

    use super::*;

    const PIECE: u64 = 16 * 1024;

    /// A pure v2 info dict with one file of `pieces` pieces whose layer is
    /// made up, plus a file smaller than a piece (no layer).
    pub(crate) fn torrent(pieces: u32) -> (Vec<u8>, Vec<Hash32>, Hash32) {
        let layer: Vec<Hash32> = (0..pieces).map(|i| sha256(&i.to_be_bytes())).collect();
        let pad = zero_subtree(0);
        let root = merkle_root(&layer, 1, pad);
        let mut info = b"d9:file treed3:bigd0:d6:lengthi".to_vec();
        info.extend_from_slice((u64::from(pieces) * PIECE).to_string().as_bytes());
        info.extend_from_slice(b"e11:pieces root32:");
        info.extend_from_slice(&root);
        info.extend_from_slice(b"ee5:smalld0:d6:lengthi5e11:pieces root32:");
        info.extend_from_slice(&[7; 32]);
        info.extend_from_slice(b"eee12:meta versioni2e4:name1:x12:piece lengthi16384ee");
        (info, layer, root)
    }

    fn layout_of(info: &[u8]) -> Arc<V2Layout> {
        let i: TorrentMetaV1Info<ByteBuf> = bencode::from_bytes(info).unwrap();
        i.validate().unwrap().v2_layout_arc().unwrap()
    }

    fn unlimited() -> Arc<FetchBudget> {
        FetchBudget::new(u64::MAX)
    }

    fn fetch_for(info: &[u8]) -> LayerFetch {
        LayerFetch::new(layout_of(info), Wanted::All, unlimited()).unwrap()
    }

    fn honest_reply(tree: &LayerTree, layer: &[Hash32], r: &HashRequest) -> Vec<u8> {
        tree.hashes(layer, r.index, r.length, r.proof_layers)
            .unwrap()
    }

    #[test]
    fn fetches_and_verifies_layers() {
        for pieces in [2u32, 300, 1500] {
            let (info, layer, root) = torrent(pieces);
            let tree = LayerTree::new(&layer, zero_subtree(0));
            let mut f = fetch_for(&info);
            let mut rounds = 0;
            while !f.is_done() {
                let reqs = f.next_requests().unwrap();
                assert!(!reqs.is_empty() && reqs.len() <= MAX_OUTSTANDING);
                // Replies may come in any order.
                for r in reqs.iter().rev() {
                    assert_eq!(r.pieces_root, root);
                    assert!(r.length <= MAX_HASH_REQUEST_LENGTH);
                    f.on_hashes(r, &honest_reply(&tree, &layer, r)).unwrap();
                }
                rounds += 1;
                assert!(rounds < 100);
            }
            let layers = f.into_piece_layers();
            let mut expected = b"d32:".to_vec();
            expected.extend_from_slice(&root);
            expected.extend_from_slice(format!("{}:", pieces * 32).as_bytes());
            expected.extend(layer.concat());
            expected.push(b'e');
            assert_eq!(&layers[..], &expected[..], "pieces {pieces}");
        }
    }

    #[test]
    fn hostile_replies_are_refused() {
        let (info, layer, _) = torrent(1500);
        let tree = LayerTree::new(&layer, zero_subtree(0));
        let mut f = fetch_for(&info);
        let reqs = f.next_requests().unwrap();
        let r = reqs[0];
        let good = honest_reply(&tree, &layer, &r);

        // Unsolicited (a request we never sent).
        let mut other = r;
        other.index += 512 * 8;
        assert!(f.on_hashes(&other, &good).is_err());
        // Wrong count.
        let mut g = fetch_for(&info);
        g.next_requests().unwrap();
        assert!(g.on_hashes(&r, &good[32..]).is_err());
        // A tampered hash or proof.
        for at in [0, good.len() - 1] {
            let mut bad = good.clone();
            bad[at] ^= 1;
            let mut f = fetch_for(&info);
            let r = f.next_requests().unwrap()[0];
            assert!(f.on_hashes(&r, &bad).is_err());
        }
        // Hashes of another part of the layer.
        let mut f2 = fetch_for(&info);
        let reqs2 = f2.next_requests().unwrap();
        let wrong = honest_reply(&tree, &layer, &reqs2[1]);
        assert!(f2.on_hashes(&reqs2[0], &wrong).is_err());
        // A reject fails the peer; a duplicate reply is unsolicited.
        assert!(f.on_reject(&reqs[1]).is_err());
        f.on_hashes(&r, &good).unwrap();
        assert!(f.on_hashes(&r, &good).is_err());

        // Only small files: nothing to fetch.
        let small = b"d9:file treed5:smalld0:d6:lengthi5e11:pieces root32:\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07eee12:meta versioni2e4:name1:x12:piece lengthi16384ee";
        assert!(LayerFetch::new(layout_of(small), Wanted::All, unlimited()).is_none());
        // Files not wanted (their layer is known) aren't fetched.
        let (info, _, root) = torrent(1500);
        let other: Arc<[Hash32]> = Arc::from(vec![[1u8; 32]]);
        assert!(LayerFetch::new(layout_of(&info), Wanted::Only(other), unlimited()).is_none());
        let only: Arc<[Hash32]> = Arc::from(vec![root]);
        assert!(LayerFetch::new(layout_of(&info), Wanted::Only(only), unlimited()).is_some());
    }

    /// Many peers fetching the same large layer at once, with honest
    /// replies: the budget caps what they hold together; fetches that
    /// would exceed it fail instead of allocating, and everything is
    /// returned when they end.
    #[test]
    fn many_fetches_stay_within_the_budget() {
        let (info, layer, _) = torrent(4000);
        let tree = LayerTree::new(&layer, zero_subtree(0));
        let layout = layout_of(&info);
        // Room for about 1.5 complete layers.
        let limit = 4000 * 32 * 3 / 2;
        let budget = FetchBudget::new(limit);
        let mut fetches: Vec<Option<LayerFetch>> = (0..50)
            .map(|_| LayerFetch::new(layout.clone(), Wanted::All, budget.clone()))
            .collect();
        let (mut completed, mut failed) = (0, 0);
        // Round robin: each fetch gets one request answered per round.
        loop {
            let mut active = false;
            for slot in fetches.iter_mut() {
                let Some(f) = slot.as_mut() else { continue };
                active = true;
                if f.next_requests().is_err() {
                    failed += 1;
                    *slot = None;
                    continue;
                }
                if f.is_done() {
                    completed += 1;
                    let layers = slot.take().unwrap().into_piece_layers();
                    assert_eq!(layers.len(), 4000 * 32 + 44);
                    continue;
                }
                let req = f.outstanding[0];
                if f.on_hashes(&req, &honest_reply(&tree, &layer, &req))
                    .is_err()
                {
                    failed += 1;
                    *slot = None;
                    continue;
                }
                assert!(budget.used() <= limit);
            }
            if !active {
                break;
            }
        }
        assert!(completed >= 1, "at least one fetch completes");
        assert!(failed > 0, "the others hit the budget");
        assert_eq!(completed + failed, 50);
        assert_eq!(budget.used(), 0);
    }

    /// A file's layer is handed out once, only when every chunk of it
    /// verified, and its memory goes back to the budget.
    #[test]
    fn complete_layers_are_taken_once() {
        let (info, layer, root) = torrent(1500);
        let tree = LayerTree::new(&layer, zero_subtree(0));
        let budget = FetchBudget::new(u64::MAX);
        let mut f = LayerFetch::new(layout_of(&info), Wanted::All, budget.clone()).unwrap();
        let reqs = f.next_requests().unwrap();
        assert_eq!(reqs.len(), 3);
        for r in &reqs[..2] {
            f.on_hashes(r, &honest_reply(&tree, &layer, r)).unwrap();
            assert!(f.take_complete(&root).is_none(), "not whole yet");
        }
        f.on_hashes(&reqs[2], &honest_reply(&tree, &layer, &reqs[2]))
            .unwrap();
        assert!(f.take_complete(&[9; 32]).is_none());
        assert_eq!(f.take_complete(&root).unwrap(), layer);
        assert!(f.take_complete(&root).is_none(), "once");
        assert_eq!(budget.used(), FILE_OVERHEAD, "only the file's bookkeeping left");
        assert!(f.is_done());
        drop(f);
        assert_eq!(budget.used(), 0);
    }

    /// A hostile peer can't make us hold more than the verified hashes of
    /// the layers we asked for: a peer answering with garbage is refused
    /// before anything is charged.
    #[test]
    fn bad_replies_are_never_charged() {
        let (info, _, _) = torrent(1500);
        let budget = FetchBudget::new(u64::MAX);
        let mut f = LayerFetch::new(layout_of(&info), Wanted::All, budget.clone()).unwrap();
        let reqs = f.next_requests().unwrap();
        let before = budget.used();
        assert_eq!(before, FILE_OVERHEAD);
        let junk = vec![0u8; usize::try_from(reqs[0].reply_hashes()).unwrap() * 32];
        assert!(f.on_hashes(&reqs[0], &junk).is_err());
        assert_eq!(budget.used(), before);
        drop(f);
        assert_eq!(budget.used(), 0);
    }
}
