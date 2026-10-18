//! BitTorrent v2 (BEP 52) metadata: the `file tree`, `piece layers` and the
//! SHA-256 merkle trees pieces are verified with.
//!
//! A pure v2 torrent is mapped onto the v1-shaped layout the rest of the
//! engine works with: files in file-tree order with a virtual padding file
//! after every file that doesn't end on a piece boundary (exactly what hybrid
//! torrents carry as BEP 47 pad files). Every piece then belongs to exactly
//! one real file.
//!
//! All input is attacker controlled (a .torrent, or metadata and hashes from
//! peers): the tree depth, the number of files and the number of pieces are
//! bounded before anything proportional to them is allocated, and nothing in
//! here panics on malformed input.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use bencode::BencodeValue;
use buffers::ByteBuf;

use crate::Error;

/// BEP 52 merkle leaf size.
pub const BLOCK_SIZE: u32 = 16 * 1024;
/// Most files a v2 file tree may have (libtorrent's limit is similar:
/// its default `max_decode_tokens` caps a torrent at a few million tokens).
pub const MAX_FILES: usize = 1 << 20;
/// Deepest path in a file tree.
pub const MAX_PATH_DEPTH: usize = 64;
/// Most pieces of a pure v2 torrent (libtorrent's default `max_pieces`).
/// Bounds the piece hash table (32 bytes per piece): files with identical
/// content share one piece layer, so the metadata size alone doesn't.
pub const MAX_PIECES: u32 = 0x20_0000;

pub type Hash32 = [u8; 32];

/// One entry of the synthesized file list (a real file or a padding file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2Entry {
    pub path: Box<[Box<[u8]>]>,
    pub len: u64,
    pub padding: bool,
    pub executable: bool,
    pub hidden: bool,
    /// Present for every non-empty real file.
    pub pieces_root: Option<Hash32>,
}

/// Pieces of one non-empty real file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2FileSpan {
    pub entry: u32,
    pub first_piece: u32,
    pub num_pieces: u32,
    pub len: u64,
    pub pieces_root: Hash32,
}

/// The v1-shaped layout of a pure v2 torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2Layout {
    pub piece_length: u32,
    pub entries: Box<[V2Entry]>,
    /// Non-empty real files in order (sorted by `first_piece`).
    pub spans: Box<[V2FileSpan]>,
    pub total_length: u64,
    /// One file at the top of the tree (a single-file torrent).
    pub single_file: bool,
}

/// What a piece of a v2 torrent must hash to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2PieceCheck {
    /// Bytes of real file data in the piece (the rest is padding, which is
    /// never hashed).
    pub data_len: u64,
    /// Leaves of the merkle tree the piece hash is the root of: the blocks
    /// per piece, or for a file no longer than a piece the block count
    /// rounded up to a power of two.
    pub leaves: u64,
    pub expected: Hash32,
}

/// Uncle hashes in the `hashes` reply to a BEP 52 hash request of `length`
/// hashes with `proof_layers`, counted like libtorrent (the reference
/// implementation): `proof_layers` are the layers above the base layer, and
/// the first log2(`length`) - 1 of them lie inside the subtree the requested
/// hashes span, so they need no uncle. For one hash that is `proof_layers`
/// + 1 (the hash's sibling first).
pub fn proof_uncles(length: u32, proof_layers: u32) -> u32 {
    proof_layers
        .saturating_add(1)
        .saturating_sub(length.max(1).ilog2())
}

/// `proof_layers` for a request of `length` hashes (a power of two) in a
/// tree `height` levels above the base layer, with a proof up to the root.
pub fn proof_layers_to_root(length: u32, height: u32) -> u32 {
    let base = length.max(1).ilog2();
    if height > base { height - 1 } else { 0 }
}

pub fn validate_piece_length(piece_length: u32) -> crate::Result<()> {
    if piece_length < BLOCK_SIZE || !piece_length.is_power_of_two() {
        return Err(Error::V2InvalidPieceLength(piece_length));
    }
    Ok(())
}

type Dict<'a> = HashMap<ByteBuf<'a>, BencodeValue<ByteBuf<'a>>>;

fn get<'a, 'b>(d: &'b Dict<'a>, key: &[u8]) -> Option<&'b BencodeValue<ByteBuf<'a>>> {
    d.get(key)
}

struct RawFile<'a> {
    path: Vec<&'a [u8]>,
    len: u64,
    pieces_root: Option<Hash32>,
    executable: bool,
    hidden: bool,
}

fn check_component(c: &[u8]) -> crate::Result<()> {
    if c.is_empty() {
        return Err(Error::BadTorrentFileNoName);
    }
    if c == b"." {
        return Err(Error::V2FileTreeDotComponent);
    }
    if c == b".." {
        return Err(Error::BadTorrentPathTraversal);
    }
    if c.contains(&b'/') || c.contains(&b'\\') || c.contains(&0) {
        return Err(Error::BadTorrentSeparatorInName);
    }
    Ok(())
}

fn parse_leaf<'a>(
    leaf: &BencodeValue<ByteBuf<'a>>,
    path: &[&'a [u8]],
) -> crate::Result<RawFile<'a>> {
    let BencodeValue::Dict(d) = leaf else {
        return Err(Error::V2InvalidTorrent);
    };
    let len = match get(d, b"length") {
        Some(BencodeValue::Integer(l)) => {
            u64::try_from(*l).map_err(|_| Error::BadTorrentLengths("negative file length"))?
        }
        _ => return Err(Error::BadTorrentLengths("v2 file without length")),
    };
    let pieces_root = match get(d, b"pieces root") {
        Some(BencodeValue::Bytes(b)) => Some(
            <Hash32>::try_from(b.0)
                .map_err(|_| Error::BadTorrentLengths("pieces root is not 32 bytes"))?,
        ),
        Some(_) => return Err(Error::BadTorrentLengths("pieces root is not a string")),
        None => None,
    };
    if len > 0 && pieces_root.is_none() {
        return Err(Error::V2SmallFileMissingPiecesRoot);
    }
    let (mut executable, mut hidden) = (false, false);
    if let Some(BencodeValue::Bytes(attr)) = get(d, b"attr") {
        // Symlinks ('l') and pad files ('p') have no meaning inside a v2
        // file tree; they are treated as plain files.
        executable = attr.0.contains(&b'x');
        hidden = attr.0.contains(&b'h');
    }
    Ok(RawFile {
        path: path.to_vec(),
        len,
        // Empty files have no merkle tree.
        pieces_root: if len == 0 { None } else { pieces_root },
        executable,
        hidden,
    })
}

fn walk<'a>(
    node: &Dict<'a>,
    path: &mut Vec<&'a [u8]>,
    out: &mut Vec<RawFile<'a>>,
) -> crate::Result<()> {
    // Dictionary keys are sorted in canonical bencode, but a malformed
    // torrent might not be: sort, the order defines the piece layout.
    let mut keys: Vec<&ByteBuf<'a>> = node.keys().collect();
    keys.sort_unstable();
    for key in keys {
        let child = &node[key];
        check_component(key.0)?;
        let BencodeValue::Dict(child) = child else {
            return Err(Error::V2InvalidTorrent);
        };
        path.push(key.0);
        if path.len() > MAX_PATH_DEPTH {
            return Err(Error::BadTorrentLengths("v2 file tree too deep"));
        }
        if let Some(leaf) = get(child, b"") {
            // A file: `name: { "": { length, pieces root } }`, nothing else.
            if child.len() != 1 {
                return Err(Error::V2InvalidTorrent);
            }
            if out.len() >= MAX_FILES {
                return Err(Error::BadTorrentLengths("too many files"));
            }
            out.push(parse_leaf(leaf, path)?);
        } else {
            walk(child, path, out)?;
        }
        path.pop();
    }
    Ok(())
}

/// Parses a bencoded `file tree` into the padded, v1-shaped layout.
pub fn parse_file_tree(raw: &[u8], piece_length: u32) -> crate::Result<V2Layout> {
    validate_piece_length(piece_length)?;
    // The bencode parser bounds nesting (DEFAULT_MAX_DEPTH), so the
    // recursive walk below is bounded too.
    let tree = bencode::dyn_from_bytes::<ByteBuf>(raw).map_err(|_| Error::V2MissingFileTree)?;
    let BencodeValue::Dict(root) = tree else {
        return Err(Error::V2FileTreeRootIsFile);
    };
    if get(&root, b"").is_some() {
        return Err(Error::V2FileTreeRootIsFile);
    }
    let mut files = Vec::new();
    walk(&root, &mut Vec::new(), &mut files)?;
    if files.is_empty() {
        return Err(Error::BadTorrentNoFiles);
    }
    let single_file = files.len() == 1 && files[0].path.len() == 1;

    let pl = piece_length as u64;
    let last_nonempty = files.iter().rposition(|f| f.len > 0);
    let mut entries = Vec::with_capacity(files.len() * 2);
    let mut spans = Vec::new();
    let mut offset = 0u64;
    for (i, f) in files.into_iter().enumerate() {
        if f.len > 0 {
            let num_pieces = f.len.div_ceil(pl);
            let first_piece = offset / pl;
            let (Ok(first_piece), Ok(num_pieces)) =
                (u32::try_from(first_piece), u32::try_from(num_pieces))
            else {
                return Err(Error::BadTorrentLengths("too many pieces"));
            };
            if first_piece
                .checked_add(num_pieces)
                .is_none_or(|end| end > MAX_PIECES)
            {
                return Err(Error::BadTorrentLengths("too many pieces"));
            }
            spans.push(V2FileSpan {
                entry: entries.len() as u32,
                first_piece,
                num_pieces,
                len: f.len,
                pieces_root: f.pieces_root.ok_or(Error::V2SmallFileMissingPiecesRoot)?,
            });
        }
        offset = offset
            .checked_add(f.len)
            .ok_or(Error::BadTorrentLengths("total length overflows"))?;
        let tail = offset % pl;
        entries.push(V2Entry {
            path: f.path.iter().map(|c| Box::<[u8]>::from(*c)).collect(),
            len: f.len,
            padding: false,
            executable: f.executable,
            hidden: f.hidden,
            pieces_root: f.pieces_root,
        });
        // Pad to the next piece boundary unless no data follows.
        if f.len > 0 && tail != 0 && last_nonempty.is_some_and(|l| i < l) {
            let pad = pl - tail;
            offset += pad;
            entries.push(V2Entry {
                path: vec![
                    Box::from(&b".pad"[..]),
                    pad.to_string().into_bytes().into_boxed_slice(),
                ]
                .into_boxed_slice(),
                len: pad,
                padding: true,
                executable: false,
                hidden: true,
                pieces_root: None,
            });
        }
    }
    if offset == 0 {
        return Err(Error::BadTorrentZeroLength);
    }
    Ok(V2Layout {
        piece_length,
        entries: entries.into_boxed_slice(),
        spans: spans.into_boxed_slice(),
        total_length: offset,
        single_file,
    })
}

impl V2Layout {
    pub fn total_pieces(&self) -> u32 {
        self.spans
            .last()
            .map(|s| s.first_piece + s.num_pieces)
            .unwrap_or(0)
    }

    /// The file a piece belongs to.
    pub fn span_of_piece(&self, piece: u32) -> Option<&V2FileSpan> {
        self.spans.get(self.span_index_of_piece(piece)?)
    }

    /// The index into [`Self::spans`] of the file a piece belongs to.
    pub fn span_index_of_piece(&self, piece: u32) -> Option<usize> {
        let idx = self
            .spans
            .partition_point(|s| s.first_piece <= piece)
            .checked_sub(1)?;
        let s = &self.spans[idx];
        (piece < s.first_piece + s.num_pieces).then_some(idx)
    }

    /// Bytes of real file data in a piece (the rest of it is padding).
    pub fn piece_data_len(&self, piece: u32) -> Option<u64> {
        let span = self.span_of_piece(piece)?;
        let pl = self.piece_length as u64;
        let k = (piece - span.first_piece) as u64;
        Some((span.len - k * pl).min(pl))
    }

    /// Blocks per piece.
    pub fn blocks_per_piece(&self) -> u64 {
        (self.piece_length / BLOCK_SIZE) as u64
    }

    /// Spans whose files are longer than one piece (they need a piece layer).
    pub fn spans_with_layers(&self) -> impl Iterator<Item = &V2FileSpan> {
        let pl = self.piece_length as u64;
        self.spans.iter().filter(move |s| s.len > pl)
    }

    /// How a piece is verified, given the table of expected piece hashes
    /// (one per piece, see [`parse_piece_layers`]).
    pub fn piece_check(&self, hashes: &[Hash32], piece: u32) -> Option<V2PieceCheck> {
        self.piece_check_with(*hashes.get(piece as usize)?, piece)
    }

    /// How a piece is verified, given its expected hash.
    pub fn piece_check_with(&self, expected: Hash32, piece: u32) -> Option<V2PieceCheck> {
        let span = self.span_of_piece(piece)?;
        let pl = self.piece_length as u64;
        let data_len = self.piece_data_len(piece)?;
        let leaves = if span.len <= pl {
            span.len.div_ceil(BLOCK_SIZE as u64).next_power_of_two()
        } else {
            self.blocks_per_piece()
        };
        Some(V2PieceCheck {
            data_len,
            leaves,
            expected,
        })
    }
}

/// The expected hash of every piece of a pure v2 torrent. A .torrent may
/// lack the piece layers of some files (libtorrent then fetches them from
/// peers with BEP 52 hash requests): the pieces of those files have no hash
/// until [`V2PieceHashes::set_layer`] adds the file's layer, checked against
/// its `pieces root`. Layers are added in place, so every holder of the
/// torrent's info sees one as soon as it is known; a hash, once known, is
/// never changed or removed.
#[derive(Debug)]
pub struct V2PieceHashes {
    /// One hash per piece; zeros for the pieces of files without a layer.
    hashes: parking_lot::RwLock<Box<[Hash32]>>,
    /// One bit per [`V2Layout::spans`] entry: its hashes are known.
    known: Box<[AtomicU64]>,
    /// Spans whose hashes aren't known yet.
    missing: AtomicU32,
    /// Indexes of the spans without a layer at first, sorted by root: a
    /// layer finds its files without walking every file (a torrent may
    /// have 2^20 of them). Built on the first layer added.
    by_root: std::sync::OnceLock<Box<[u32]>>,
}

impl V2PieceHashes {
    /// `hashes`: one per piece; `missing`: indexes into `layout.spans` of
    /// the files whose entries in `hashes` aren't valid (yet).
    pub fn new(layout: &V2Layout, hashes: Box<[Hash32]>, missing: &[usize]) -> Self {
        let known: Box<[AtomicU64]> = (0..layout.spans.len().div_ceil(64))
            .map(|_| AtomicU64::new(u64::MAX))
            .collect();
        let mut count = 0u32;
        for &span in missing {
            if let Some(w) = known.get(span / 64) {
                let bit = 1u64 << (span % 64);
                if w.fetch_and(!bit, Ordering::Relaxed) & bit != 0 {
                    count += 1;
                }
            }
        }
        Self {
            hashes: parking_lot::RwLock::new(hashes),
            known,
            missing: AtomicU32::new(count),
            by_root: std::sync::OnceLock::new(),
        }
    }

    /// Every file's hashes are known.
    pub fn is_complete(&self) -> bool {
        self.missing_count() == 0
    }

    /// Files (spans) whose layer isn't known yet.
    pub fn missing_count(&self) -> u32 {
        self.missing.load(Ordering::Acquire)
    }

    /// Whether the hashes of `layout.spans[span]` are known.
    pub fn span_known(&self, span: usize) -> bool {
        self.known
            .get(span / 64)
            .is_some_and(|w| w.load(Ordering::Acquire) & (1 << (span % 64)) != 0)
    }

    /// The expected hash of `piece`; `None` while its file's layer is
    /// unknown (and for an invalid index).
    pub fn get(&self, layout: &V2Layout, piece: u32) -> Option<Hash32> {
        let span = layout.span_index_of_piece(piece)?;
        if !self.span_known(span) {
            return None;
        }
        self.hashes.read().get(piece as usize).copied()
    }

    /// Runs `f` on the whole table (zeros where unknown).
    pub fn with_hashes<R>(&self, f: impl FnOnce(&[Hash32]) -> R) -> R {
        f(&self.hashes.read())
    }

    /// Runs `f` on the layer of `layout.spans[span]`; `None` while it is
    /// unknown.
    pub fn with_span<R>(
        &self,
        layout: &V2Layout,
        span: usize,
        f: impl FnOnce(&[Hash32]) -> R,
    ) -> Option<R> {
        let s = layout.spans.get(span)?;
        if !self.span_known(span) {
            return None;
        }
        let g = self.hashes.read();
        let start = s.first_piece as usize;
        Some(f(g.get(start..start + s.num_pieces as usize)?))
    }

    /// The `pieces root`s of the files whose layer is unknown (sorted, each
    /// once).
    pub fn missing_roots(&self, layout: &V2Layout) -> Vec<Hash32> {
        if self.is_complete() {
            return Vec::new();
        }
        let mut roots: Vec<Hash32> = (0..layout.spans.len())
            .filter(|&i| !self.span_known(i))
            .map(|i| layout.spans[i].pieces_root)
            .collect();
        roots.sort_unstable();
        roots.dedup();
        roots
    }

    /// Writes `layer` (already verified against `root`) for every unknown
    /// file with that root; returns their span indexes.
    /// The spans that had no layer at first and whose root is `root`.
    fn spans_with_root(&self, layout: &V2Layout, root: &Hash32) -> &[u32] {
        let by_root = self.by_root.get_or_init(|| {
            #[allow(clippy::cast_possible_truncation)] // ≤ MAX_FILES spans
            let mut v: Vec<u32> = (0..layout.spans.len())
                .filter(|&i| !self.span_known(i))
                .map(|i| i as u32)
                .collect();
            v.sort_unstable_by_key(|&i| layout.spans[i as usize].pieces_root);
            v.into_boxed_slice()
        });
        let root_of = |i: u32| layout.spans.get(i as usize).map(|s| s.pieces_root);
        let start = by_root.partition_point(|&i| root_of(i).is_some_and(|r| r < *root));
        let end = start
            + by_root[start..]
                .iter()
                .take_while(|&&i| root_of(i) == Some(*root))
                .count();
        &by_root[start..end]
    }

    fn store_layer(&self, layout: &V2Layout, root: &Hash32, layer: &[Hash32]) -> Vec<usize> {
        let spans = self.spans_with_root(layout, root);
        let mut g = self.hashes.write();
        let mut set = Vec::new();
        for &i in spans {
            let i = i as usize;
            let s = &layout.spans[i];
            if s.num_pieces as usize != layer.len() || self.span_known(i) {
                continue;
            }
            let start = s.first_piece as usize;
            if let Some(dst) = g.get_mut(start..start + layer.len()) {
                dst.copy_from_slice(layer);
                set.push(i);
            }
        }
        // Known only once the hashes are in place (still under the lock).
        for &i in &set {
            self.known[i / 64].fetch_or(1 << (i % 64), Ordering::Release);
        }
        #[allow(clippy::cast_possible_truncation)]
        self.missing.fetch_sub(set.len() as u32, Ordering::AcqRel);
        set
    }
}

#[cfg(any(feature = "sha1-ring", feature = "sha1-crypto-hash"))]
pub use merkle::*;

#[cfg(any(feature = "sha1-ring", feature = "sha1-crypto-hash"))]
mod merkle {
    use std::collections::HashSet;

    use bencode::BencodeValue;
    use buffers::ByteBuf;
    use sha1w::ISha256;

    use super::{BLOCK_SIZE, Hash32, V2Layout, V2PieceHashes, proof_uncles};
    use crate::Error;

    pub fn sha256(data: &[u8]) -> Hash32 {
        let mut h = sha1w::Sha256::new();
        h.update(data);
        h.finish()
    }

    pub fn hash_pair(a: &Hash32, b: &Hash32) -> Hash32 {
        let mut h = sha1w::Sha256::new();
        h.update(a);
        h.update(b);
        h.finish()
    }

    /// Root of a subtree of `2^level` all-zero leaves.
    pub fn zero_subtree(level: u32) -> Hash32 {
        let mut h = [0u8; 32];
        for _ in 0..level {
            h = hash_pair(&h, &h);
        }
        h
    }

    /// Root of a merkle tree over `nodes`, padded to the next power of two
    /// (but at least `min_width`) with `pad`, the value of an empty node at
    /// the level of `nodes`.
    pub fn merkle_root(nodes: &[Hash32], min_width: u64, mut pad: Hash32) -> Hash32 {
        let width = (nodes.len() as u64)
            .max(min_width)
            .max(1)
            .next_power_of_two();
        let mut level: Vec<Hash32> = nodes.to_vec();
        let mut w = width;
        while w > 1 {
            if level.is_empty() {
                level.push(pad);
            }
            if level.len() % 2 == 1 {
                level.push(pad);
            }
            level = level
                .as_chunks::<2>()
                .0
                .iter()
                .map(|[a, b]| hash_pair(a, b))
                .collect();
            pad = hash_pair(&pad, &pad);
            w /= 2;
        }
        level.first().copied().unwrap_or(pad)
    }

    /// Streaming merkle hasher over 16 KiB blocks: memory is one pending
    /// hash per tree level, whatever the piece size.
    pub struct MerkleHasher {
        block: sha1w::Sha256,
        block_fill: u32,
        /// Completed subtrees, (level, hash), levels strictly decreasing.
        stack: Vec<(u32, Hash32)>,
    }

    impl Default for MerkleHasher {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MerkleHasher {
        pub fn new() -> Self {
            Self {
                block: sha1w::Sha256::new(),
                block_fill: 0,
                stack: Vec::new(),
            }
        }

        fn push(&mut self, mut level: u32, mut h: Hash32) {
            while let Some(&(l, top)) = self.stack.last() {
                if l != level {
                    break;
                }
                self.stack.pop();
                h = hash_pair(&top, &h);
                level += 1;
            }
            self.stack.push((level, h));
        }

        pub fn update(&mut self, mut data: &[u8]) {
            while !data.is_empty() {
                let take = ((BLOCK_SIZE - self.block_fill) as usize).min(data.len());
                self.block.update(&data[..take]);
                self.block_fill += take as u32;
                data = &data[take..];
                if self.block_fill == BLOCK_SIZE {
                    let block = std::mem::replace(&mut self.block, sha1w::Sha256::new());
                    self.block_fill = 0;
                    self.push(0, block.finish());
                }
            }
        }

        /// Root over `leaves` leaves (a power of two); missing leaves are
        /// zero hashes and a short last block is hashed as is. `None` if
        /// more data than `leaves` blocks was fed.
        pub fn finish(mut self, leaves: u64) -> Option<Hash32> {
            if !leaves.is_power_of_two() {
                return None;
            }
            let target = leaves.trailing_zeros();
            if self.block_fill > 0 {
                let block = std::mem::replace(&mut self.block, sha1w::Sha256::new());
                self.push(0, block.finish());
            }
            let mut zero = [0u8; 32];
            let mut zero_level = 0u32;
            loop {
                let &(l, h) = match self.stack.as_slice() {
                    [] => return Some(zero_subtree(target)),
                    [only] if only.0 == target => return Some(only.1),
                    s => s.last()?,
                };
                if l >= target {
                    return None;
                }
                // Complete the rightmost (lowest) subtree with an empty sibling.
                self.stack.pop();
                while zero_level < l {
                    zero = hash_pair(&zero, &zero);
                    zero_level += 1;
                }
                self.push(l + 1, hash_pair(&h, &zero));
            }
        }
    }

    /// Hash of one piece of real data `data` (no padding) as a v2 piece
    /// with `leaves` leaves.
    pub fn piece_root(data: &[u8], leaves: u64) -> Option<Hash32> {
        let mut h = MerkleHasher::new();
        h.update(data);
        h.finish(leaves)
    }

    /// Validates the `piece layers` dictionary of a pure v2 torrent and
    /// returns the expected hash of every piece: the layer entry for files
    /// longer than a piece, the `pieces root` for the others.
    pub fn parse_piece_layers(
        raw: Option<&[u8]>,
        layout: &V2Layout,
    ) -> crate::Result<Box<[Hash32]>> {
        let PartialPieceLayers { hashes, missing } = parse_piece_layers_partial(raw, layout)?;
        match missing.first() {
            None => Ok(hashes),
            Some(_) if raw.is_none() => Err(Error::V2MissingPieceLayers),
            Some(&span) => {
                let e = &layout.entries[layout.spans[span].entry as usize];
                let path = e
                    .path
                    .iter()
                    .map(|c| String::from_utf8_lossy(c).into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                Err(Error::V2MissingPieceLayersEntry(path))
            }
        }
    }

    /// Piece hashes from a possibly incomplete `piece layers` dictionary.
    #[derive(Debug)]
    pub struct PartialPieceLayers {
        /// One expected hash per piece; zeros for the pieces of `missing`.
        pub hashes: Box<[Hash32]>,
        /// Indexes into [`V2Layout::spans`] of the files longer than a piece
        /// whose layer isn't in the dictionary.
        pub missing: Vec<usize>,
    }

    /// Like [`parse_piece_layers`], but a file without a layer entry (or no
    /// dictionary at all) is reported in `missing` instead of failing:
    /// libtorrent then fetches the layer from peers (BEP 52 hash requests).
    /// Entries that are present must still be well-formed and hash up to
    /// their file's `pieces root`.
    pub fn parse_piece_layers_partial(
        raw: Option<&[u8]>,
        layout: &V2Layout,
    ) -> crate::Result<PartialPieceLayers> {
        let total = layout.total_pieces() as usize;
        let mut hashes = vec![[0u8; 32]; total];
        let blocks_per_piece = layout.blocks_per_piece();
        let pad = zero_subtree(blocks_per_piece.trailing_zeros());
        let pl = layout.piece_length as u64;

        let parsed = match raw {
            Some(raw) => Some(
                bencode::dyn_from_bytes::<ByteBuf>(raw).map_err(|_| Error::V2MissingPieceLayers)?,
            ),
            None => None,
        };
        let dict = match &parsed {
            Some(BencodeValue::Dict(d)) => Some(d),
            Some(_) => return Err(Error::V2MissingPieceLayers),
            None => None,
        };
        let mut missing = Vec::new();
        // Identical files share one layer: verify each root once.
        let mut verified: HashSet<Hash32> = HashSet::new();
        for (idx, span) in layout.spans.iter().enumerate() {
            let first = span.first_piece as usize;
            if span.len <= pl {
                hashes[first] = span.pieces_root;
                continue;
            }
            let layer = match dict.and_then(|d| d.get(&span.pieces_root[..])) {
                Some(BencodeValue::Bytes(b)) => b.0,
                Some(_) => return Err(Error::V2PieceLayersWrongSize {
                    expected: span.num_pieces as usize * 32,
                    actual: 0,
                }),
                None => {
                    missing.push(idx);
                    continue;
                }
            };
            let expected = span.num_pieces as usize * 32;
            if layer.len() != expected {
                return Err(Error::V2PieceLayersWrongSize {
                    expected,
                    actual: layer.len(),
                });
            }
            let nodes: &[Hash32] = layer.as_chunks::<32>().0;
            if !verified.contains(&span.pieces_root) {
                if merkle_root(nodes, 1, pad) != span.pieces_root {
                    return Err(Error::V2PieceLayersRootMismatch);
                }
                verified.insert(span.pieces_root);
            }
            hashes[first..first + nodes.len()].copy_from_slice(nodes);
        }
        Ok(PartialPieceLayers {
            hashes: hashes.into_boxed_slice(),
            missing,
        })
    }

    impl V2PieceHashes {
        /// The piece hashes of a possibly incomplete `piece layers`
        /// dictionary (see [`parse_piece_layers_partial`]): files without
        /// an entry have no hashes yet.
        pub fn from_piece_layers(raw: Option<&[u8]>, layout: &V2Layout) -> crate::Result<Self> {
            let PartialPieceLayers { hashes, missing } = parse_piece_layers_partial(raw, layout)?;
            Ok(Self::new(layout, hashes, &missing))
        }

        /// Adds the piece layer of the files whose `pieces root` is `root`
        /// once it hashes up to that root (an error otherwise, or for a
        /// root no file that lacked its layer has). Returns the spans whose
        /// hashes became known (none when they already were).
        pub fn set_layer(
            &self,
            layout: &V2Layout,
            root: &Hash32,
            layer: &[Hash32],
        ) -> crate::Result<Vec<usize>> {
            let span = self
                .spans_with_root(layout, root)
                .first()
                .and_then(|&i| layout.spans.get(i as usize))
                .ok_or(Error::V2PieceLayersRootMismatch)?;
            if layer.len() != span.num_pieces as usize {
                return Err(Error::V2PieceLayersWrongSize {
                    expected: span.num_pieces as usize * 32,
                    actual: layer.len() * 32,
                });
            }
            let pad = zero_subtree(layout.blocks_per_piece().trailing_zeros());
            if merkle_root(layer, 1, pad) != *root {
                return Err(Error::V2PieceLayersRootMismatch);
            }
            Ok(self.store_layer(layout, root, layer))
        }

        /// The bencoded `piece layers` of the files whose layer is known
        /// (keys sorted, each root once).
        pub fn piece_layers_bencode(&self, layout: &V2Layout) -> Vec<u8> {
            let pl = u64::from(layout.piece_length);
            let mut spans: Vec<usize> = (0..layout.spans.len())
                .filter(|&i| layout.spans[i].len > pl && self.span_known(i))
                .collect();
            spans.sort_unstable_by_key(|&i| layout.spans[i].pieces_root);
            spans.dedup_by_key(|i| layout.spans[*i].pieces_root);
            self.with_hashes(|hashes| {
                let mut out = vec![b'd'];
                for i in spans {
                    let s = &layout.spans[i];
                    let start = s.first_piece as usize;
                    let Some(layer) = hashes.get(start..start + s.num_pieces as usize) else {
                        continue;
                    };
                    out.extend_from_slice(b"32:");
                    out.extend_from_slice(&s.pieces_root);
                    out.extend_from_slice(format!("{}:", layer.len() * 32).as_bytes());
                    for h in layer {
                        out.extend_from_slice(h);
                    }
                }
                out.push(b'e');
                out
            })
        }
    }

    /// A `piece layers` dictionary with the entries of `extra` added to
    /// those of `base` (keys sorted, `extra` wins on a duplicate root). Both
    /// must be dictionaries of byte strings; the result is validated by
    /// [`parse_piece_layers`] like any .torrent's.
    pub fn merge_piece_layers(base: Option<&[u8]>, extra: &[u8]) -> crate::Result<Vec<u8>> {
        fn entries<'a>(
            v: &'a BencodeValue<ByteBuf<'a>>,
            out: &mut std::collections::BTreeMap<&'a [u8], &'a [u8]>,
        ) -> crate::Result<()> {
            let BencodeValue::Dict(d) = v else {
                return Err(Error::V2MissingPieceLayers);
            };
            for (k, v) in d {
                let BencodeValue::Bytes(b) = v else {
                    return Err(Error::V2MissingPieceLayers);
                };
                out.insert(k.0, b.0);
            }
            Ok(())
        }
        let base = base
            .map(bencode::dyn_from_bytes::<ByteBuf>)
            .transpose()
            .map_err(|_| Error::V2MissingPieceLayers)?;
        let extra =
            bencode::dyn_from_bytes::<ByteBuf>(extra).map_err(|_| Error::V2MissingPieceLayers)?;
        let mut all = std::collections::BTreeMap::new();
        if let Some(b) = &base {
            entries(b, &mut all)?;
        }
        entries(&extra, &mut all)?;
        let mut out = vec![b'd'];
        for (k, v) in all {
            out.extend_from_slice(format!("{}:", k.len()).as_bytes());
            out.extend_from_slice(k);
            out.extend_from_slice(format!("{}:", v.len()).as_bytes());
            out.extend_from_slice(v);
        }
        out.push(b'e');
        Ok(out)
    }

    /// Levels above the piece layer from which [`LayerTree`] keeps nodes:
    /// subtrees below are at most 512 hashes, the BEP 52 request limit.
    const CACHED_LEVEL: u32 = 9;

    /// Height of a file's tree above its piece layer: log2 of the layer
    /// width padded to a power of two.
    pub fn layer_tree_height(num_pieces: u32) -> u32 {
        u64::from(num_pieces.max(1))
            .next_power_of_two()
            .trailing_zeros()
    }

    /// The part of a file's merkle tree above its piece layer, for answering
    /// BEP 52 hash requests: the nodes from level 9 (512-hash subtrees) up
    /// are kept (about 1/256 of the layer's size); nodes below are hashed on
    /// demand from at most 512 layer hashes. A request thus costs at most a
    /// few hundred hashes whatever the file size.
    #[derive(Debug)]
    pub struct LayerTree {
        height: u32,
        /// Value of an empty node at each level (level 0 = a piece of zeros).
        pads: Vec<Hash32>,
        /// `upper[k]`: nodes at level `CACHED_LEVEL + k` (empty when the tree
        /// is low enough to hash directly).
        upper: Vec<Vec<Hash32>>,
    }

    impl LayerTree {
        /// `pad0`: hash of an all-zero piece (`zero_subtree(log2(blocks per piece))`).
        pub fn new(layer: &[Hash32], pad0: Hash32) -> Self {
            let height = layer_tree_height(layer.len() as u32);
            let mut pads = vec![pad0];
            for l in 0..height as usize {
                pads.push(hash_pair(&pads[l], &pads[l]));
            }
            let mut upper = Vec::new();
            if height > CACHED_LEVEL {
                let width = 1u64 << CACHED_LEVEL;
                let mut level: Vec<Hash32> = layer
                    .chunks(width as usize)
                    .map(|c| merkle_root(c, width, pad0))
                    .collect();
                for l in CACHED_LEVEL..height {
                    let next = level
                        .chunks(2)
                        .map(|p| hash_pair(&p[0], p.get(1).unwrap_or(&pads[l as usize])))
                        .collect();
                    upper.push(std::mem::replace(&mut level, next));
                }
                upper.push(level);
            }
            Self {
                height,
                pads,
                upper,
            }
        }

        pub fn height(&self) -> u32 {
            self.height
        }

        fn node(&self, layer: &[Hash32], level: u32, idx: u64) -> Hash32 {
            if self.height > CACHED_LEVEL && level >= CACHED_LEVEL {
                return self.upper[(level - CACHED_LEVEL) as usize]
                    .get(idx as usize)
                    .copied()
                    .unwrap_or(self.pads[level as usize]);
            }
            let width = 1u64 << level;
            let start = (idx << level).min(layer.len() as u64) as usize;
            let end = ((idx + 1) << level).min(layer.len() as u64) as usize;
            merkle_root(&layer[start..end], width, self.pads[0])
        }

        pub fn root(&self, layer: &[Hash32]) -> Hash32 {
            self.node(layer, self.height, 0)
        }

        /// The `hashes` payload answering a request for `length` piece-layer
        /// hashes from `index` with `proof_layers` (see [`proof_uncles`]),
        /// or `None` if the request is invalid (BEP 52 rules as libtorrent
        /// checks them, plus our 512 limit).
        pub fn hashes(
            &self,
            layer: &[Hash32],
            index: u32,
            length: u32,
            proof_layers: u32,
        ) -> Option<Vec<u8>> {
            let width = 1u64 << self.height;
            let (index, len) = (u64::from(index), u64::from(length));
            if !(1..=512).contains(&len)
                || !len.is_power_of_two()
                || !index.is_multiple_of(len)
                || index + len > width
                || proof_layers >= self.height.max(1)
            {
                return None;
            }
            let base_level = len.trailing_zeros();
            // At most up to the root: proof_layers < height.
            let uncles = proof_uncles(length, proof_layers);
            let mut out = Vec::with_capacity((len as usize + uncles as usize) * 32);
            for i in index..index + len {
                out.extend_from_slice(layer.get(i as usize).unwrap_or(&self.pads[0]));
            }
            let mut pos = index >> base_level;
            for l in base_level..base_level + uncles {
                out.extend_from_slice(&self.node(layer, l, pos ^ 1));
                pos >>= 1;
            }
            Some(out)
        }
    }

    /// Checks a BEP 52 `hashes` reply: `base` (`length` hashes from `index`
    /// of a file's piece layer) plus the uncle hashes up to the file's
    /// `pieces root`.
    pub fn verify_layer_hashes(
        root: &Hash32,
        index: u32,
        base: &[Hash32],
        uncles: &[Hash32],
    ) -> bool {
        let len = base.len() as u64;
        if len == 0 || !len.is_power_of_two() || !u64::from(index).is_multiple_of(len) {
            return false;
        }
        let mut node = merkle_root(base, len, [0; 32]);
        let mut pos = u64::from(index) / len;
        for u in uncles {
            node = if pos & 1 == 0 {
                hash_pair(&node, u)
            } else {
                hash_pair(u, &node)
            };
            pos >>= 1;
        }
        pos == 0 && node == *root
    }

    /// Hashes of the `piece layers` for every file longer than a piece,
    /// rebuilt from the piece hash table (for writing a .torrent).
    pub fn piece_layers_bencode(layout: &V2Layout, hashes: &[Hash32]) -> Vec<u8> {
        let mut layers: Vec<(&Hash32, &[Hash32])> = layout
            .spans_with_layers()
            .filter_map(|s| {
                let start = s.first_piece as usize;
                Some((
                    &s.pieces_root,
                    hashes.get(start..start + s.num_pieces as usize)?,
                ))
            })
            .collect();
        layers.sort_unstable_by_key(|(root, _)| *root);
        layers.dedup_by_key(|(root, _)| *root);
        let mut out = Vec::new();
        out.push(b'd');
        for (root, layer) in layers {
            out.extend_from_slice(b"32:");
            out.extend_from_slice(root);
            out.extend_from_slice(format!("{}:", layer.len() * 32).as_bytes());
            for h in layer {
                out.extend_from_slice(h);
            }
        }
        out.push(b'e');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `{ "": { length, pieces root } }` built by hand.
    fn leaf(len: i64, root: Option<[u8; 32]>) -> Vec<u8> {
        let mut v = b"d0:d6:lengthi".to_vec();
        v.extend_from_slice(len.to_string().as_bytes());
        v.push(b'e');
        if let Some(r) = root {
            v.extend_from_slice(b"11:pieces root32:");
            v.extend_from_slice(&r);
        }
        v.extend_from_slice(b"ee");
        v
    }

    fn key(k: &[u8]) -> Vec<u8> {
        let mut v = k.len().to_string().into_bytes();
        v.push(b':');
        v.extend_from_slice(k);
        v
    }

    fn dict(entries: &[(&[u8], Vec<u8>)]) -> Vec<u8> {
        let mut v = vec![b'd'];
        for (k, val) in entries {
            v.extend(key(k));
            v.extend_from_slice(val);
        }
        v.push(b'e');
        v
    }

    #[test]
    fn layout_pads_between_files_only() {
        let pl = 32 * 1024;
        let tree = dict(&[
            (b"a", leaf(50_000, Some([1; 32]))),
            (
                b"b",
                dict(&[(b"c", leaf(4, Some([2; 32]))), (b"d", leaf(0, None))]),
            ),
            (b"e", leaf(70_000, Some([3; 32]))),
        ]);
        let l = parse_file_tree(&tree, pl).unwrap();
        let desc: Vec<(String, u64, bool)> = l
            .entries
            .iter()
            .map(|e| {
                let p = e
                    .path
                    .iter()
                    .map(|c| String::from_utf8_lossy(c).into_owned())
                    .collect::<Vec<_>>();
                (p.join("/"), e.len, e.padding)
            })
            .collect();
        let pad = |n: u64| 32_768 - n % 32_768;
        assert_eq!(
            desc,
            vec![
                ("a".into(), 50_000, false),
                (format!(".pad/{}", pad(50_000)), pad(50_000), true),
                ("b/c".into(), 4, false),
                (format!(".pad/{}", pad(4)), pad(4), true),
                ("b/d".into(), 0, false),
                ("e".into(), 70_000, false),
            ]
        );
        assert!(!l.single_file);
        assert_eq!(l.total_pieces(), 2 + 1 + 3);
        assert_eq!(l.total_length, 3 * 32_768 + 70_000);
        let spans: Vec<(u32, u32)> = l
            .spans
            .iter()
            .map(|s| (s.first_piece, s.num_pieces))
            .collect();
        assert_eq!(spans, [(0, 2), (2, 1), (3, 3)]);
        assert_eq!(l.span_of_piece(2).unwrap().len, 4);
        assert_eq!(l.span_of_piece(5).unwrap().len, 70_000);
        assert!(l.span_of_piece(6).is_none());

        let single = dict(&[(b"movie.mkv", leaf(10, Some([0; 32])))]);
        assert!(parse_file_tree(&single, pl).unwrap().single_file);
        let nested = dict(&[(b"dir", dict(&[(b"x", leaf(10, Some([0; 32])))]))]);
        assert!(!parse_file_tree(&nested, pl).unwrap().single_file);
    }

    #[test]
    fn malformed_trees_are_rejected() {
        let pl = 16 * 1024;
        let ok = leaf(10, Some([0; 32]));
        let bad = |t: Vec<u8>| parse_file_tree(&t, pl).unwrap_err();
        assert!(matches!(
            bad(dict(&[(b"..", ok.clone())])),
            Error::BadTorrentPathTraversal
        ));
        assert!(matches!(
            bad(dict(&[(b".", ok.clone())])),
            Error::V2FileTreeDotComponent
        ));
        assert!(matches!(
            bad(dict(&[(b"a/b", ok.clone())])),
            Error::BadTorrentSeparatorInName
        ));
        assert!(matches!(
            bad(dict(&[(b"a\\b", ok.clone())])),
            Error::BadTorrentSeparatorInName
        ));
        assert!(matches!(
            bad(dict(&[(b"", ok.clone())])),
            Error::V2FileTreeRootIsFile
        ));
        assert!(matches!(bad(dict(&[])), Error::BadTorrentNoFiles));
        assert!(matches!(
            bad(dict(&[(b"a", leaf(10, None))])),
            Error::V2SmallFileMissingPiecesRoot
        ));
        assert!(matches!(
            bad(dict(&[(b"a", leaf(-1, Some([0; 32])))])),
            Error::BadTorrentLengths(_)
        ));
        assert!(matches!(
            bad(dict(&[(b"a", leaf(0, None))])),
            Error::BadTorrentZeroLength
        ));
        assert!(matches!(
            bad(dict(&[(b"a", b"i1e".to_vec())])),
            Error::V2InvalidTorrent
        ));
        assert!(matches!(bad(b"le".to_vec()), Error::V2FileTreeRootIsFile));
        assert!(matches!(bad(b"d1:a".to_vec()), Error::V2MissingFileTree));
        // A file node with other keys next to "".
        let mut mixed = b"d0:d6:lengthi1e11:pieces root32:".to_vec();
        mixed.extend_from_slice(&[0; 32]);
        mixed.extend_from_slice(b"e1:xd0:d6:lengthi1e11:pieces root32:");
        mixed.extend_from_slice(&[0; 32]);
        mixed.extend_from_slice(b"eee");
        assert!(matches!(
            bad(dict(&[(b"a", mixed)])),
            Error::V2InvalidTorrent
        ));
        // Short pieces root.
        let short = b"d0:d6:lengthi1e11:pieces root3:abcee".to_vec();
        assert!(matches!(
            bad(dict(&[(b"a", short)])),
            Error::BadTorrentLengths(_)
        ));

        // Piece length: a power of two of at least 16 KiB.
        let t = dict(&[(b"a", ok.clone())]);
        for pl in [0, 8192, 48 * 1024, 16 * 1024 + 1] {
            assert!(matches!(
                parse_file_tree(&t, pl),
                Err(Error::V2InvalidPieceLength(_))
            ));
        }

        // Too deep: more than MAX_PATH_DEPTH nested directories.
        let mut deep = ok.clone();
        for _ in 0..MAX_PATH_DEPTH + 1 {
            deep = dict(&[(b"d", deep)]);
        }
        assert!(parse_file_tree(&deep, pl).is_err());
        // Deeper than the bencode parser allows: an error, not a stack overflow.
        let mut very_deep = ok;
        for _ in 0..10_000 {
            very_deep = dict(&[(b"d", very_deep)]);
        }
        assert!(parse_file_tree(&very_deep, pl).is_err());

        // Too many pieces: refused before the piece table is allocated.
        let huge = dict(&[(
            b"a",
            leaf(i64::from(MAX_PIECES) * 16 * 1024 + 1, Some([0; 32])),
        )]);
        assert!(matches!(bad(huge), Error::BadTorrentLengths(_)));
        let overflow = dict(&[
            (b"a", leaf(i64::MAX, Some([0; 32]))),
            (b"b", leaf(i64::MAX, Some([0; 32]))),
        ]);
        assert!(parse_file_tree(&overflow, 1 << 30).is_err());
    }

    #[cfg(any(feature = "sha1-ring", feature = "sha1-crypto-hash"))]
    fn pseudo_random(len: usize, seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    #[test]
    #[cfg(any(feature = "sha1-ring", feature = "sha1-crypto-hash"))]
    fn streaming_merkle_matches_full_tree() {
        let block = BLOCK_SIZE as usize;
        for (len, leaves) in [
            (1usize, 1u64),
            (block, 1),
            (block + 1, 2),
            (3 * block, 4),
            (3 * block + 7, 4),
            (5 * block, 8),
            (5 * block, 64),
            (64 * block, 64),
        ] {
            let data = pseudo_random(len, len as u32);
            let blocks: Vec<Hash32> = data.chunks(block).map(sha256).collect();
            let expected = merkle_root(&blocks, leaves, [0; 32]);
            // Fed in odd-sized parts.
            let mut h = MerkleHasher::new();
            for c in data.chunks(5000) {
                h.update(c);
            }
            assert_eq!(
                h.finish(leaves),
                Some(expected),
                "len {len} leaves {leaves}"
            );
            assert_eq!(piece_root(&data, leaves), Some(expected));
        }
        // One block: the root is the block's hash.
        assert_eq!(piece_root(b"abc", 1), Some(sha256(b"abc")));
        // More data than leaves, or a leaf count that isn't a power of two.
        assert_eq!(piece_root(&vec![0; 3 * block], 2), None);
        assert_eq!(piece_root(b"abc", 3), None);
        // No data at all: an all-zero subtree.
        assert_eq!(MerkleHasher::new().finish(4), Some(zero_subtree(2)));
        assert_eq!(merkle_root(&[], 4, [0; 32]), zero_subtree(2));
    }

    #[test]
    #[cfg(any(feature = "sha1-ring", feature = "sha1-crypto-hash"))]
    fn piece_layers_are_validated() {
        let pl = 32 * 1024u32;
        let bpp = (pl / BLOCK_SIZE) as u64;
        let big = pseudo_random(100_000, 1);
        let small = pseudo_random(20_000, 2);
        let layer: Vec<Hash32> = big
            .chunks(pl as usize)
            .map(|p| piece_root(p, bpp).unwrap())
            .collect();
        let big_root = merkle_root(&layer, 1, zero_subtree(bpp.trailing_zeros()));
        let small_root = piece_root(&small, 2).unwrap();
        let tree = dict(&[
            (b"big", leaf(100_000, Some(big_root))),
            (b"small", leaf(20_000, Some(small_root))),
        ]);
        let l = parse_file_tree(&tree, pl).unwrap();
        let layers = |root: &[u8; 32], hashes: &[u8]| {
            let mut v = b"d32:".to_vec();
            v.extend_from_slice(root);
            v.extend(key(hashes));
            v.push(b'e');
            v
        };
        let good = layers(&big_root, &layer.concat());
        let hashes = parse_piece_layers(Some(&good), &l).unwrap();
        assert_eq!(hashes.len(), 5);
        assert_eq!(&hashes[..4], &layer[..]);
        assert_eq!(hashes[4], small_root);
        // Every piece checks out against its data.
        for piece in 0..5u32 {
            let c = l.piece_check(&hashes, piece).unwrap();
            let data = if piece < 4 {
                &big[piece as usize * pl as usize..][..c.data_len as usize]
            } else {
                &small[..]
            };
            assert_eq!(
                piece_root(data, c.leaves),
                Some(c.expected),
                "piece {piece}"
            );
        }
        assert_eq!(
            l.piece_check(&hashes, 3).unwrap().data_len,
            100_000 - 3 * 32_768
        );
        assert_eq!(l.piece_check(&hashes, 4).unwrap().leaves, 2);
        assert!(l.piece_check(&hashes, 5).is_none());
        // Round trip through the writer.
        assert_eq!(piece_layers_bencode(&l, &hashes), good);

        // Tampered hash, wrong length, missing entry, missing dict.
        let mut tampered = layer.concat();
        tampered[40] ^= 1;
        assert!(matches!(
            parse_piece_layers(Some(&layers(&big_root, &tampered)), &l),
            Err(Error::V2PieceLayersRootMismatch)
        ));
        assert!(matches!(
            parse_piece_layers(Some(&layers(&big_root, &layer.concat()[..96])), &l),
            Err(Error::V2PieceLayersWrongSize { .. })
        ));
        assert!(matches!(
            parse_piece_layers(Some(&layers(&[9; 32], &layer.concat())), &l),
            Err(Error::V2MissingPieceLayersEntry(_))
        ));
        assert!(matches!(
            parse_piece_layers(None, &l),
            Err(Error::V2MissingPieceLayers)
        ));
        assert!(parse_piece_layers(Some(b"i1e"), &l).is_err());
        assert!(parse_piece_layers(Some(b"d32:"), &l).is_err());

        // Partial: a missing layer is reported, present ones still checked.
        for raw in [None, Some(&b"de"[..]), Some(&layers(&[9; 32], &layer.concat())[..])] {
            let p = parse_piece_layers_partial(raw, &l).unwrap();
            assert_eq!(p.missing, vec![0]);
            assert_eq!(p.hashes[4], small_root);
            assert_eq!(p.hashes[0], [0; 32]);
        }
        let p = parse_piece_layers_partial(Some(&good), &l).unwrap();
        assert!(p.missing.is_empty());
        assert_eq!(&p.hashes[..], &hashes[..]);
        assert!(parse_piece_layers_partial(Some(&layers(&big_root, &tampered)), &l).is_err());
        assert!(parse_piece_layers_partial(Some(b"d32:"), &l).is_err());
        let not_bytes = {
            let mut v = b"d32:".to_vec();
            v.extend_from_slice(&big_root);
            v.extend_from_slice(b"i1ee");
            v
        };
        assert!(parse_piece_layers_partial(Some(&not_bytes), &l).is_err());

        // Merging fetched layers into an incomplete dictionary.
        let other = layers(&[9; 32], b"x");
        let merged = merge_piece_layers(Some(&other), &good).unwrap();
        assert_eq!(parse_piece_layers(Some(&merged), &l).unwrap(), hashes);
        assert!(merged.starts_with(b"d32:"));
        let (first, second) = if big_root < [9; 32] { (&good, &other) } else { (&other, &good) };
        let mut expected = first[..first.len() - 1].to_vec();
        expected.extend_from_slice(&second[1..]);
        assert_eq!(merged, expected, "keys sorted");
        assert_eq!(merge_piece_layers(None, &good).unwrap(), good);
        assert!(merge_piece_layers(Some(b"i1e"), &good).is_err());
        assert!(merge_piece_layers(None, &not_bytes).is_err());

        // Only small files: no layers needed.
        let tree = dict(&[(b"small", leaf(20_000, Some(small_root)))]);
        let l = parse_file_tree(&tree, pl).unwrap();
        assert_eq!(&parse_piece_layers(None, &l).unwrap()[..], &[small_root]);
    }

    /// Layers missing from a .torrent are added one file at a time: only a
    /// layer that hashes up to the root is taken, identical files get it
    /// together, and nothing known changes afterwards.
    #[test]
    #[cfg(any(feature = "sha1-ring", feature = "sha1-crypto-hash"))]
    fn piece_hashes_take_missing_layers_one_file_at_a_time() {
        let pl = 32 * 1024u32;
        let bpp = (pl / BLOCK_SIZE) as u64;
        let pad = zero_subtree(bpp.trailing_zeros());
        let big = pseudo_random(100_000, 1);
        let other = pseudo_random(70_000, 3);
        let small = pseudo_random(20_000, 2);
        let layer_of = |d: &[u8]| -> Vec<Hash32> {
            d.chunks(pl as usize)
                .map(|p| piece_root(p, bpp).unwrap())
                .collect()
        };
        let (big_layer, other_layer) = (layer_of(&big), layer_of(&other));
        let big_root = merkle_root(&big_layer, 1, pad);
        let other_root = merkle_root(&other_layer, 1, pad);
        let small_root = piece_root(&small, 2).unwrap();
        // a: big (4 pieces), b: same content (4), c: other (3), d: small (1).
        let tree = dict(&[
            (b"a", leaf(100_000, Some(big_root))),
            (b"b", leaf(100_000, Some(big_root))),
            (b"c", leaf(70_000, Some(other_root))),
            (b"d", leaf(20_000, Some(small_root))),
        ]);
        let l = parse_file_tree(&tree, pl).unwrap();
        assert_eq!(l.total_pieces(), 12);
        let mut only_other = b"d32:".to_vec();
        only_other.extend_from_slice(&other_root);
        only_other.extend(key(&other_layer.concat()));
        only_other.push(b'e');

        let h = V2PieceHashes::from_piece_layers(Some(&only_other), &l).unwrap();
        assert_eq!(h.missing_count(), 2);
        assert!(!h.is_complete());
        assert_eq!(h.missing_roots(&l), vec![big_root]);
        assert!(!h.span_known(0) && !h.span_known(1));
        assert!(h.span_known(2) && h.span_known(3));
        assert_eq!(h.get(&l, 0), None);
        assert_eq!(h.get(&l, 8), Some(other_layer[0]));
        assert_eq!(h.get(&l, 11), Some(small_root));
        assert_eq!(h.get(&l, 12), None);
        assert!(h.with_span(&l, 0, |_| ()).is_none());
        assert_eq!(h.piece_layers_bencode(&l), only_other);

        // A wrong layer, a wrong size and an unknown root change nothing.
        let mut tampered = big_layer.clone();
        tampered[1][0] ^= 1;
        assert!(h.set_layer(&l, &big_root, &tampered).is_err());
        assert!(h.set_layer(&l, &big_root, &big_layer[..3]).is_err());
        assert!(h.set_layer(&l, &[9; 32], &big_layer).is_err());
        assert!(h.set_layer(&l, &small_root, &[small_root]).is_err(), "no layer");
        assert_eq!(h.missing_count(), 2);
        assert_eq!(h.get(&l, 1), None);

        // The right one: both identical files at once.
        assert_eq!(h.set_layer(&l, &big_root, &big_layer).unwrap(), vec![0, 1]);
        assert!(h.is_complete());
        assert!(h.missing_roots(&l).is_empty());
        assert_eq!(h.get(&l, 1), Some(big_layer[1]));
        assert_eq!(h.get(&l, 5), Some(big_layer[1]));
        assert_eq!(h.with_span(&l, 1, |s| s.to_vec()), Some(big_layer.clone()));
        // Again: nothing new.
        assert!(h.set_layer(&l, &big_root, &big_layer).unwrap().is_empty());
        assert_eq!(h.missing_count(), 0);
        // Now the same as a complete .torrent's.
        let full = h.piece_layers_bencode(&l);
        let complete = parse_piece_layers(Some(&full), &l).unwrap();
        h.with_hashes(|t| assert_eq!(t, &complete[..]));
    }

    #[test]
    #[cfg(any(feature = "sha1-ring", feature = "sha1-crypto-hash"))]
    fn layer_proofs_verify_against_the_root() {
        let pad0 = zero_subtree(2);
        // Layers small enough to hash directly and large enough to use the
        // cached upper levels, with and without padding to a power of two.
        for pieces in [2u32, 3, 5, 512, 513, 1000, 2048, 3000] {
            let layer: Vec<Hash32> = (0..pieces).map(|i| sha256(&i.to_be_bytes())).collect();
            let root = merkle_root(&layer, 1, pad0);
            let tree = LayerTree::new(&layer, pad0);
            assert_eq!(tree.root(&layer), root, "pieces {pieces}");
            let height = tree.height();
            assert_eq!(1u64 << height, u64::from(pieces).next_power_of_two());
            let width = 1u32 << height;
            for len in [1u32, 2, 4, 512] {
                if len > width {
                    continue;
                }
                let proofs = proof_layers_to_root(len, height);
                let uncles = proof_uncles(len, proofs);
                assert_eq!(uncles, height - len.trailing_zeros(), "pieces {pieces} len {len}");
                for index in (0..width).step_by(len as usize).take(5) {
                    let out = tree.hashes(&layer, index, len, proofs).unwrap();
                    let hashes: &[Hash32] = out.as_chunks::<32>().0;
                    assert_eq!(hashes.len(), (len + uncles) as usize);
                    let (base, uncles) = hashes.split_at(len as usize);
                    for (i, h) in base.iter().enumerate() {
                        let want = layer.get(index as usize + i).unwrap_or(&pad0);
                        assert_eq!(h, want);
                    }
                    assert!(
                        verify_layer_hashes(&root, index, base, uncles),
                        "pieces {pieces} len {len} index {index}"
                    );
                    // Any change breaks the proof.
                    let mut bad = base.to_vec();
                    bad[0][0] ^= 1;
                    assert!(!verify_layer_hashes(&root, index, &bad, uncles));
                    if let Some((_, rest)) = uncles.split_first() {
                        assert!(!verify_layer_hashes(&root, index, base, rest));
                    }
                    assert!(!verify_layer_hashes(&root, index + len, base, uncles) || width == len);
                }
            }
            // Invalid requests are refused.
            assert!(tree.hashes(&layer, 0, 3, 0).is_none());
            assert!(tree.hashes(&layer, 0, 1024, 0).is_none());
            assert!(tree.hashes(&layer, 1, 2, 0).is_none());
            assert!(tree.hashes(&layer, width, 2, 0).is_none());
            assert!(tree.hashes(&layer, 0, 2, height).is_none());
            assert!(tree.hashes(&layer, 0, 2, u32::MAX).is_none());
            assert!(tree.hashes(&layer, u32::MAX - 1, 2, 0).is_none());
        }
        // libtorrent's counting: proof layers inside the requested subtree
        // carry no uncle hash.
        assert_eq!(proof_uncles(512, 10), 2);
        assert_eq!(proof_uncles(512, 8), 0);
        assert_eq!(proof_uncles(2, 3), 3);
        assert_eq!(proof_uncles(1, 3), 4);
        assert_eq!(proof_uncles(1, u32::MAX), u32::MAX);
        assert_eq!(proof_layers_to_root(512, 9), 0);
        assert_eq!(proof_layers_to_root(512, 11), 10);
        assert!(!verify_layer_hashes(&[0; 32], 0, &[], &[]));
        assert!(!verify_layer_hashes(&[0; 32], 1, &[[0; 32], [0; 32]], &[]));
    }
}
