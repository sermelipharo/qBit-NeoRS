//! Piece hashing and metainfo generation for v1 (BEP 3), v2 (BEP 52) and
//! hybrid torrents, mirroring libtorrent's `create_torrent` output.
//!
//! The data is read once, sequentially, in 1 MiB chunks; v1 piece hashes
//! (SHA-1 over the concatenated stream including zero-filled padding files)
//! and v2 merkle trees (SHA-256 of 16 KiB blocks per file) are computed from
//! the same pass. Memory is bounded by the output: one 20-byte hash per v1
//! piece and one 32-byte hash per v2 piece; block hashes are folded into
//! piece hashes as soon as a piece is complete.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use sha1w::ISha1;
use sha2::Digest;

use super::bencode::Value;
use super::layout::{self, ABORTED, Collected, LayoutEntry, SourceFile};
use crate::model::TorrentFormat;

/// BEP 52 block size.
pub const BLOCK_SIZE: u64 = 16 * 1024;
pub const MIN_PIECE_SIZE: u64 = 16 * 1024;
pub const MAX_PIECE_SIZE: u64 = 256 * 1024 * 1024;
const READ_CHUNK: usize = 1024 * 1024;

pub type Hash32 = [u8; 32];

/// Automatic piece size, libtorrent's table (target: hash list about twice
/// the square root of the content size), 16 KiB .. 16 MiB.
pub fn auto_piece_size(total_size: u64) -> u64 {
    const SIZE_TABLE: [u64; 10] = [
        2_684_355,
        10_737_418,
        42_949_673,
        171_798_692,
        687_194_767,
        2_748_779_069,
        10_995_116_278,
        43_980_465_111,
        175_921_860_444,
        703_687_441_777,
    ];
    let i = SIZE_TABLE.iter().take_while(|&&s| s < total_size).count();
    BLOCK_SIZE << i
}

/// Validates an explicit piece size (0 = automatic) like libtorrent: at
/// least 16 KiB and at most 256 MiB; a power of two for v2/hybrid; a power
/// of two or a multiple of 16 KiB for v1.
pub fn resolve_piece_size(requested: i64, total_size: u64, format: TorrentFormat) -> Result<u64, String> {
    if requested == 0 {
        return Ok(auto_piece_size(total_size));
    }
    let invalid = || "invalid piece size".to_string();
    let p = u64::try_from(requested).map_err(|_| invalid())?;
    if !(MIN_PIECE_SIZE..=MAX_PIECE_SIZE).contains(&p) {
        return Err(invalid());
    }
    let ok = match format {
        TorrentFormat::V1 => p.is_power_of_two() || p.is_multiple_of(BLOCK_SIZE),
        TorrentFormat::V2 | TorrentFormat::Hybrid => p.is_power_of_two(),
    };
    if ok { Ok(p) } else { Err(invalid()) }
}

fn sha256(data: &[u8]) -> Hash32 {
    sha2::Sha256::digest(data).into()
}

fn hash_pair(a: &Hash32, b: &Hash32) -> Hash32 {
    let mut h = sha2::Sha256::new();
    h.update(a);
    h.update(b);
    h.finalize().into()
}

/// Root of a merkle tree whose missing nodes (to the next power of two) are
/// `pad`-valued subtrees; `pad` is the value at the level of `nodes`.
pub fn merkle_root(nodes: &[Hash32], mut pad: Hash32) -> Hash32 {
    if nodes.is_empty() {
        return pad;
    }
    let mut level = nodes.to_vec();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(pad);
        }
        level = level.as_chunks::<2>().0.iter().map(|[a, b]| hash_pair(a, b)).collect();
        pad = hash_pair(&pad, &pad);
    }
    level[0]
}

/// Root of a subtree of `leaves` all-zero leaf hashes (power of two).
pub fn merkle_pad(leaves: u64) -> Hash32 {
    let mut h = [0u8; 32];
    let mut n = leaves;
    while n > 1 {
        h = hash_pair(&h, &h);
        n /= 2;
    }
    h
}

/// Builds one file's v2 merkle tree incrementally.
struct V2FileHasher {
    piece_len: u64,
    blocks_per_piece: usize,
    file_size: u64,
    leaves: Vec<Hash32>,
    layer: Vec<Hash32>,
}

impl V2FileHasher {
    fn new(piece_len: u64, file_size: u64) -> Self {
        let blocks_per_piece = (piece_len / BLOCK_SIZE) as usize;
        Self { piece_len, blocks_per_piece, file_size, leaves: Vec::new(), layer: Vec::new() }
    }

    /// `data` starts on a block boundary; only the file's last block may be short.
    fn update(&mut self, data: &[u8]) {
        for block in data.chunks(BLOCK_SIZE as usize) {
            self.leaves.push(sha256(block));
            if self.leaves.len() == self.blocks_per_piece {
                self.layer.push(merkle_root(&self.leaves, [0; 32]));
                self.leaves.clear();
            }
        }
    }

    /// Returns (pieces root, piece layer) — `None` for empty files; the
    /// layer only for files spanning more than one piece.
    fn finish(mut self) -> Option<(Hash32, Option<Vec<u8>>)> {
        if self.file_size == 0 {
            return None;
        }
        if self.file_size <= self.piece_len {
            // Smaller than a piece: pad to the next power of two only.
            let root = if self.layer.is_empty() { merkle_root(&self.leaves, [0; 32]) } else { self.layer[0] };
            return Some((root, None));
        }
        if !self.leaves.is_empty() {
            self.leaves.resize(self.blocks_per_piece, [0; 32]);
            self.layer.push(merkle_root(&self.leaves, [0; 32]));
        }
        let root = merkle_root(&self.layer, merkle_pad(self.blocks_per_piece as u64));
        let bytes = self.layer.concat();
        Some((root, Some(bytes)))
    }
}

pub struct BuildParams<'a> {
    pub source: &'a Path,
    pub format: TorrentFormat,
    /// Bytes, 0 = automatic.
    pub piece_size: i64,
    pub optimize_alignment: bool,
    /// Bytes, -1 = disabled (v1 + optimize alignment only).
    pub padding_file_limit: i64,
    pub private: bool,
    /// Tracker URLs; an empty (or blank) entry starts a new tier.
    pub trackers: &'a [String],
    pub url_seeds: &'a [String],
    pub comment: &'a str,
    pub source_tag: &'a str,
    pub created_by: &'a str,
    pub creation_date: i64,
    /// Refuse to create a .torrent larger than this many bytes (estimated
    /// before hashing so the user does not wait for nothing).
    pub max_torrent_size: u64,
}

#[derive(Debug)]
pub struct BuildResult {
    pub data: Vec<u8>,
    pub name: String,
    pub piece_size: u64,
    pub info_hash_v1: Option<[u8; 20]>,
    pub info_hash_v2: Option<Hash32>,
    /// Symlinks that were not followed (for the log).
    pub skipped_links: Vec<String>,
}

impl BuildResult {
    /// The torrent id in hex (qBittorrent's `TorrentID`): the v2 info hash
    /// truncated to 20 bytes for v2 and hybrid torrents, the v1 hash
    /// otherwise.
    pub fn info_hash_hex(&self) -> String {
        match (&self.info_hash_v1, &self.info_hash_v2) {
            (_, Some(v2)) => hex(&v2[..20]),
            (Some(v1), None) => hex(v1),
            (None, None) => String::new(),
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Groups tracker URLs into tiers: blank entries start a new tier,
/// duplicates are dropped.
pub fn tracker_tiers(trackers: &[String]) -> Vec<Vec<String>> {
    let mut tiers: Vec<Vec<String>> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut new_tier = true;
    for t in trackers {
        let t = t.trim();
        if t.is_empty() {
            new_tier = true;
            continue;
        }
        if !seen.insert(t.to_string()) {
            continue;
        }
        if new_tier || tiers.is_empty() {
            tiers.push(Vec::new());
            new_tier = false;
        }
        tiers.last_mut().expect("tier").push(t.to_string());
    }
    tiers
}

fn open_source(f: &SourceFile) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    // O_NONBLOCK: never hang on a FIFO swapped in after collection.
    let mut flags = libc::O_NONBLOCK;
    if !f.resolved_link {
        flags |= libc::O_NOFOLLOW;
    }
    opts.custom_flags(flags);
    let file = opts.open(&f.disk_path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    Ok(file)
}

/// Reads until `buf` is full or EOF; returns the number of bytes read.
fn read_full(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

struct V1Hasher {
    piece_len: u64,
    fill: u64,
    current: sha1w::Sha1,
    pieces: Vec<u8>,
}

impl V1Hasher {
    fn new(piece_len: u64) -> Self {
        Self { piece_len, fill: 0, current: sha1w::Sha1::new(), pieces: Vec::new() }
    }

    fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let take = ((self.piece_len - self.fill) as usize).min(data.len());
            self.current.update(&data[..take]);
            self.fill += take as u64;
            data = &data[take..];
            if self.fill == self.piece_len {
                let done = std::mem::replace(&mut self.current, sha1w::Sha1::new());
                self.pieces.extend_from_slice(&done.finish());
                self.fill = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.fill > 0 {
            self.pieces.extend_from_slice(&self.current.finish());
        }
        self.pieces
    }
}

struct Hashed {
    pieces_v1: Vec<u8>,
    /// Per source file: v2 (root, layer).
    v2: Vec<Option<(Hash32, Option<Vec<u8>>)>>,
}

fn hash_content(
    files: &[SourceFile],
    entries: &[LayoutEntry],
    piece_len: u64,
    want_v1: bool,
    want_v2: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<Hashed, String> {
    let total: u64 = entries.iter().filter(|e| want_v1 || !e.is_pad()).map(|e| e.size).sum();
    let mut done = 0u64;
    let mut v1 = want_v1.then(|| V1Hasher::new(piece_len));
    let mut v2: Vec<Option<(Hash32, Option<Vec<u8>>)>> = vec![None; files.len()];
    let mut buf = vec![0u8; READ_CHUNK];
    let zeros = [0u8; 64 * 1024];

    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            return Err(ABORTED.to_string());
        }
        let Some(index) = entry.file else {
            if let Some(v1) = v1.as_mut() {
                let mut left = entry.size;
                while left > 0 {
                    let n = left.min(zeros.len() as u64) as usize;
                    v1.update(&zeros[..n]);
                    left -= n as u64;
                }
                done += entry.size;
            }
            continue;
        };
        let f = &files[index];
        let mut v2h = want_v2.then(|| V2FileHasher::new(piece_len, f.size));
        if f.size > 0 {
            let shown = f.path.join("/");
            let mut file = open_source(f).map_err(|e| format!("{shown}: {e}"))?;
            let mut left = f.size;
            while left > 0 {
                if cancel.load(Ordering::Relaxed) {
                    return Err(ABORTED.to_string());
                }
                let want = left.min(buf.len() as u64) as usize;
                let n = read_full(&mut file, &mut buf[..want]).map_err(|e| format!("{shown}: {e}"))?;
                if n < want {
                    return Err(format!("{shown}: the file was modified during torrent creation"));
                }
                let chunk = &buf[..n];
                if let Some(v1) = v1.as_mut() {
                    v1.update(chunk);
                }
                if let Some(h) = v2h.as_mut() {
                    h.update(chunk);
                }
                left -= n as u64;
                done += n as u64;
                if total > 0 {
                    progress(done as f64 * 100.0 / total as f64);
                }
            }
        }
        if let Some(h) = v2h {
            v2[index] = h.finish();
        }
    }
    Ok(Hashed { pieces_v1: v1.map(V1Hasher::finish).unwrap_or_default(), v2 })
}

/// Rough size of the generated .torrent, computed before hashing.
fn estimate_size(collected: &Collected, entries: &[LayoutEntry], piece_len: u64, v1: bool, v2: bool) -> u64 {
    let mut est = 1024u64;
    let total: u64 = entries.iter().map(|e| e.size).sum();
    if v1 {
        est += total.div_ceil(piece_len) * 20;
    }
    for e in entries {
        let path_len: u64 = match e.file {
            Some(i) => collected.files[i].path.iter().map(|c| c.len() as u64 + 4).sum(),
            None => 16,
        };
        est += 40 + path_len * if v1 && v2 { 2 } else { 1 };
        if v2 && e.size > piece_len {
            est += e.size.div_ceil(piece_len) * 32 + 64;
        }
    }
    est
}

/// Creates the torrent. Errors are plain reasons (the caller formats them as
/// "Create new torrent file failed. Reason: %1.").
pub fn build_torrent(
    p: &BuildParams<'_>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<BuildResult, String> {
    let collected = layout::collect_files(p.source, cancel)?;
    if collected.files.is_empty() {
        return Err("no files in torrent".to_string());
    }
    let total_size: u64 = collected.files.iter().map(|f| f.size).sum();
    if total_size == 0 {
        return Err("invalid length of torrent".to_string());
    }
    let piece_len = resolve_piece_size(p.piece_size, total_size, p.format)?;
    let want_v1 = p.format != TorrentFormat::V2;
    let want_v2 = p.format != TorrentFormat::V1;

    let entries = match p.format {
        TorrentFormat::V1 if p.optimize_alignment && collected.multi_file => {
            layout::optimized_v1_layout(&collected.files, piece_len, p.padding_file_limit)
        }
        TorrentFormat::V1 => layout::plain_layout(&collected.files),
        TorrentFormat::V2 | TorrentFormat::Hybrid => layout::canonical_layout(&collected.files, piece_len),
    };

    let estimate = estimate_size(&collected, &entries, piece_len, want_v1, want_v2);
    if estimate > p.max_torrent_size {
        return Err(format!(
            "the torrent file would exceed the size limit ({}); choose a larger piece size",
            crate::utils::misc::friendly_unit(p.max_torrent_size as i64, false)
        ));
    }

    let Hashed { pieces_v1, v2: v2_hashes } =
        hash_content(&collected.files, &entries, piece_len, want_v1, want_v2, cancel, progress)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(ABORTED.to_string());
    }

    // ---- info dictionary -------------------------------------------------
    let mut info = Value::dict();
    info.insert("name", Value::str(&collected.name));
    info.insert("piece length", Value::Int(piece_len as i64));
    if p.private {
        info.insert("private", Value::Int(1));
    }
    if !p.source_tag.is_empty() {
        info.insert("source", Value::str(p.source_tag));
    }
    if want_v1 {
        if collected.multi_file {
            let list = entries
                .iter()
                .map(|e| {
                    let mut fe = Value::dict();
                    fe.insert("length", Value::Int(e.size as i64));
                    let comps = match e.file {
                        Some(i) => &collected.files[i].path,
                        None => &e.pad_path,
                    };
                    fe.insert("path", Value::List(comps.iter().map(|c| Value::str(c)).collect()));
                    if e.is_pad() {
                        fe.insert("attr", Value::str("p"));
                    }
                    fe
                })
                .collect();
            info.insert("files", Value::List(list));
        } else {
            info.insert("length", Value::Int(collected.files[0].size as i64));
        }
        info.insert("pieces", Value::Bytes(pieces_v1));
    }
    let mut piece_layers = Value::dict();
    if want_v2 {
        let mut tree = Value::dict();
        for e in entries.iter().filter(|e| !e.is_pad()) {
            let i = e.file.expect("real file");
            let f = &collected.files[i];
            let mut leaf = Value::dict();
            leaf.insert("length", Value::Int(f.size as i64));
            if let Some((root, layer)) = &v2_hashes[i] {
                leaf.insert("pieces root", Value::Bytes(root.to_vec()));
                if let (Some(layer), Value::Dict(d)) = (layer, &mut piece_layers) {
                    d.insert(root.to_vec(), Value::Bytes(layer.clone()));
                }
            }
            insert_tree_path(&mut tree, &f.path, leaf)?;
        }
        info.insert("file tree", tree);
        info.insert("meta version", Value::Int(2));
    }
    let info_bytes = info.encode();

    // ---- top-level dictionary -----------------------------------------------
    let mut root = Value::dict();
    let tiers = tracker_tiers(p.trackers);
    if let Some(first) = tiers.first().and_then(|t| t.first()) {
        root.insert("announce", Value::str(first));
    }
    if tiers.iter().map(Vec::len).sum::<usize>() > 1 {
        let list = tiers.iter().map(|t| Value::List(t.iter().map(|u| Value::str(u)).collect())).collect();
        root.insert("announce-list", Value::List(list));
    }
    if !p.comment.is_empty() {
        root.insert("comment", Value::str(p.comment));
    }
    if !p.created_by.is_empty() {
        root.insert("created by", Value::str(p.created_by));
    }
    if p.creation_date != 0 {
        root.insert("creation date", Value::Int(p.creation_date));
    }
    if want_v2 {
        root.insert("piece layers", piece_layers);
    }
    let mut seeds: Vec<&str> = Vec::new();
    for s in p.url_seeds.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if !seeds.contains(&s) {
            seeds.push(s);
        }
    }
    match seeds.as_slice() {
        [] => {}
        [one] => root.insert("url-list", Value::str(one)),
        many => root.insert("url-list", Value::List(many.iter().map(|s| Value::str(s)).collect())),
    }

    // Splice the already encoded info dict in (identical bytes to hash).
    let mut data = Vec::with_capacity(info_bytes.len() + 4096);
    let Value::Dict(top) = root else { unreachable!() };
    data.push(b'd');
    let mut info_written = false;
    let write_info = |data: &mut Vec<u8>| {
        Value::Bytes(b"info".to_vec()).encode_into(data);
        data.extend_from_slice(&info_bytes);
    };
    for (k, v) in &top {
        if !info_written && k.as_slice() > b"info".as_slice() {
            write_info(&mut data);
            info_written = true;
        }
        Value::Bytes(k.clone()).encode_into(&mut data);
        v.encode_into(&mut data);
    }
    if !info_written {
        write_info(&mut data);
    }
    data.push(b'e');

    let info_hash_v1 = want_v1.then(|| {
        let mut h = sha1w::Sha1::new();
        h.update(&info_bytes);
        h.finish()
    });
    let info_hash_v2 = want_v2.then(|| sha256(&info_bytes));
    progress(100.0);
    Ok(BuildResult {
        data,
        name: collected.name,
        piece_size: piece_len,
        info_hash_v1,
        info_hash_v2,
        skipped_links: collected.skipped_links,
    })
}

fn insert_tree_path(tree: &mut Value, path: &[String], leaf: Value) -> Result<(), String> {
    let conflict = || format!("inconsistent file paths in torrent: {}", path.join("/"));
    let mut node = tree;
    for comp in path {
        let Value::Dict(d) = node else { return Err(conflict()) };
        if d.contains_key(b"".as_slice()) {
            return Err(conflict());
        }
        node = d.entry(comp.as_bytes().to_vec()).or_insert_with(Value::dict);
    }
    let Value::Dict(d) = node else { return Err(conflict()) };
    if !d.is_empty() {
        return Err(conflict());
    }
    d.insert(Vec::new(), leaf);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha1w::ISha256;

    #[test]
    fn piece_size_selection() {
        assert_eq!(auto_piece_size(0), 16 * 1024);
        assert_eq!(auto_piece_size(2_684_355), 16 * 1024);
        assert_eq!(auto_piece_size(2_684_356), 32 * 1024);
        assert_eq!(auto_piece_size(700 * 1024 * 1024), 512 * 1024);
        assert_eq!(auto_piece_size(4_700_000_000), 1024 * 1024);
        assert_eq!(auto_piece_size(50 * (1u64 << 30)), 4 * 1024 * 1024);
        assert_eq!(auto_piece_size(u64::MAX), 16 * 1024 * 1024);

        assert_eq!(resolve_piece_size(0, 1 << 20, TorrentFormat::Hybrid), Ok(16 * 1024));
        assert_eq!(resolve_piece_size(1 << 20, 1, TorrentFormat::V2), Ok(1 << 20));
        assert_eq!(resolve_piece_size(256 << 20, 1, TorrentFormat::Hybrid), Ok(256 << 20));
        assert!(resolve_piece_size(512 << 20, 1, TorrentFormat::V1).is_err());
        assert!(resolve_piece_size(8 * 1024, 1, TorrentFormat::V1).is_err());
        assert!(resolve_piece_size(-1, 1, TorrentFormat::V1).is_err());
        assert!(resolve_piece_size(48 * 1024, 1, TorrentFormat::Hybrid).is_err());
        assert_eq!(resolve_piece_size(48 * 1024, 1, TorrentFormat::V1), Ok(48 * 1024));
    }

    #[test]
    fn tiers() {
        let t: Vec<String> = ["a", "b", "", "", "c", " a ", "  ", "d"].iter().map(|s| s.to_string()).collect();
        assert_eq!(tracker_tiers(&t), vec![vec!["a", "b"], vec!["c"], vec!["d"]]);
        assert!(tracker_tiers(&[String::new()]).is_empty());
    }

    /// Independent BEP 52 reference: full tree over next_pow2 leaves using
    /// the engine's SHA-256 implementation.
    fn reference_root(data: &[u8], piece_len: u64) -> Hash32 {
        fn h(d: &[u8]) -> Hash32 {
            let mut s = sha1w::Sha256::new();
            s.update(d);
            s.finish()
        }
        let mut leaves: Vec<Hash32> = data.chunks(BLOCK_SIZE as usize).map(h).collect();
        let bpp = (piece_len / BLOCK_SIZE) as usize;
        let min = if data.len() as u64 > piece_len { bpp } else { 1 };
        let n = leaves.len().next_power_of_two().max(min);
        leaves.resize(n, [0; 32]);
        while leaves.len() > 1 {
            leaves = leaves
                .chunks(2)
                .map(|p| {
                    let mut c = p[0].to_vec();
                    c.extend_from_slice(&p[1]);
                    h(&c)
                })
                .collect();
        }
        leaves[0]
    }

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
    fn merkle_matches_reference() {
        let piece = 64 * 1024;
        for len in [1usize, 16 * 1024, 16 * 1024 + 1, 40_000, 64 * 1024, 64 * 1024 + 5, 300_000] {
            let data = pseudo_random(len, len as u32);
            let mut h = V2FileHasher::new(piece, len as u64);
            for c in data.chunks(32 * 1024) {
                h.update(c);
            }
            let (root, layer) = h.finish().unwrap();
            assert_eq!(root, reference_root(&data, piece), "len {len}");
            assert_eq!(layer.is_some(), len as u64 > piece, "len {len}");
            if let Some(layer) = layer {
                assert_eq!(layer.len() as u64, (len as u64).div_ceil(piece) * 32);
            }
        }
        // A single block's root is the block hash itself.
        let data = pseudo_random(1000, 7);
        let mut h = V2FileHasher::new(piece, 1000);
        h.update(&data);
        assert_eq!(h.finish().unwrap().0, sha256(&data));
        assert!(V2FileHasher::new(piece, 0).finish().is_none());
    }

    fn params<'a>(source: &'a Path, format: TorrentFormat, trackers: &'a [String], seeds: &'a [String]) -> BuildParams<'a> {
        BuildParams {
            source,
            format,
            piece_size: 32 * 1024,
            optimize_alignment: false,
            padding_file_limit: -1,
            private: true,
            trackers,
            url_seeds: seeds,
            comment: "hello",
            source_tag: "SRC",
            created_by: "qBit NeoRS v0.2.2",
            creation_date: 1_700_000_000,
            max_torrent_size: 100 << 20,
        }
    }

    fn tree(dir: &Path) -> std::path::PathBuf {
        let root = dir.join("content");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.bin"), pseudo_random(50_000, 1)).unwrap();
        std::fs::write(root.join("sub/b.bin"), pseudo_random(100_000, 2)).unwrap();
        std::fs::write(root.join("sub/empty"), b"").unwrap();
        std::fs::write(root.join("c.txt"), b"tiny").unwrap();
        root
    }

    #[test]
    fn v1_torrent_parses_with_engine() {
        let dir = tempfile::tempdir().unwrap();
        let root = tree(dir.path());
        let trackers: Vec<String> = ["udp://t1/a", "", "http://t2/b", "http://t3/c"].iter().map(|s| s.to_string()).collect();
        let seeds = vec!["http://seed/".to_string()];
        let r = build_torrent(&params(&root, TorrentFormat::V1, &trackers, &seeds), &AtomicBool::new(false), &mut |_| {})
            .unwrap();
        let parsed = librqbit::torrent_from_bytes(&r.data).unwrap();
        assert_eq!(hex(&parsed.info_hash.0), r.info_hash_hex());
        assert_eq!(parsed.info.data.piece_length, 32 * 1024);
        assert!(parsed.info.data.private);
        let files = parsed.info.data.files.as_ref().unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|f| f.path.iter().map(|c| String::from_utf8_lossy(c.as_ref()).into_owned()).collect::<Vec<_>>().join("/"))
            .collect();
        assert_eq!(names, ["a.bin", "c.txt", "sub/b.bin", "sub/empty"]);
        assert_eq!(parsed.announce.as_ref().map(|a| a.as_ref().to_vec()), Some(b"udp://t1/a".to_vec()));
        assert_eq!(parsed.announce_list.len(), 2);
        assert_eq!(parsed.announce_list[1].len(), 2);

        // Piece hashes over the concatenated content.
        let mut all = Vec::new();
        for n in ["a.bin", "c.txt", "sub/b.bin", "sub/empty"] {
            all.extend(std::fs::read(root.join(n)).unwrap());
        }
        let expected: Vec<u8> = all
            .chunks(32 * 1024)
            .flat_map(|c| {
                let mut h = sha1w::Sha1::new();
                h.update(c);
                h.finish()
            })
            .collect();
        assert_eq!(parsed.info.data.pieces.as_ref(), expected.as_slice());
        let text = String::from_utf8_lossy(&r.data);
        assert!(text.contains("6:source3:SRC"));
        assert!(text.contains("8:url-list12:http://seed/"));
        assert!(!text.contains("meta version"));
    }

    #[test]
    fn hybrid_torrent_layout_and_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let root = tree(dir.path());
        let r = build_torrent(&params(&root, TorrentFormat::Hybrid, &[], &[]), &AtomicBool::new(false), &mut |_| {})
            .unwrap();
        let parsed = librqbit::torrent_from_bytes(&r.data).unwrap();
        // The engine runs a hybrid on its v1 hash; its id is the truncated v2 hash.
        assert_eq!(hex(&parsed.info_hash.0), hex(&r.info_hash_v1.unwrap()));
        assert_eq!(r.info_hash_hex(), hex(&r.info_hash_v2.unwrap()[..20]));
        let files = parsed.info.data.files.as_ref().unwrap();
        let desc: Vec<(String, u64, bool)> = files
            .iter()
            .map(|f| {
                let p = f.path.iter().map(|c| String::from_utf8_lossy(c.as_ref()).into_owned()).collect::<Vec<_>>().join("/");
                (p, f.length, f.attr.as_ref().is_some_and(|a| a.as_ref() == b"p"))
            })
            .collect();
        let piece = 32 * 1024u64;
        let pad = |n: u64| piece - n % piece;
        assert_eq!(
            desc,
            vec![
                ("a.bin".to_string(), 50_000, false),
                (format!(".pad/{}", pad(50_000)), pad(50_000), true),
                ("c.txt".to_string(), 4, false),
                (format!(".pad/{}", pad(4)), pad(4), true),
                ("sub/b.bin".to_string(), 100_000, false),
                (format!(".pad/{}", pad(100_000)), pad(100_000), true),
                ("sub/empty".to_string(), 0, false),
            ]
        );
        // Every real file starts on a piece boundary.
        let mut off = 0;
        for (_, len, is_pad) in &desc {
            if !is_pad {
                assert_eq!(off % piece, 0);
            }
            off += len;
        }
        // v1 pieces: file data zero-padded per file.
        let mut stream = Vec::new();
        for n in ["a.bin", "c.txt", "sub/b.bin"] {
            let d = std::fs::read(root.join(n)).unwrap();
            let padded = (d.len() as u64).div_ceil(piece) * piece;
            stream.extend(&d);
            stream.resize(stream.len() + (padded as usize - d.len()), 0);
        }
        let expected: Vec<u8> = stream
            .chunks(piece as usize)
            .flat_map(|c| {
                let mut h = sha1w::Sha1::new();
                h.update(c);
                h.finish()
            })
            .collect();
        assert_eq!(parsed.info.data.pieces.as_ref(), expected.as_slice());

        // v2 parts: pieces roots match the independent reference, piece
        // layers only for files larger than a piece.
        let text = r.data.clone();
        let b_root = reference_root(&std::fs::read(root.join("sub/b.bin")).unwrap(), piece);
        let a_root = reference_root(&std::fs::read(root.join("a.bin")).unwrap(), piece);
        let c_root = reference_root(b"tiny", piece);
        for r in [b_root, a_root, c_root] {
            assert!(text.windows(32).any(|w| w == r));
        }
        assert!(contains(&text, b"12:meta versioni2e"));
        assert!(contains(&text, b"12:piece layersd32:"));
        assert!(contains(&text, b"5:emptyd0:d6:lengthi0eee"));
        assert!(r.info_hash_v2.is_some());
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn pure_v2_and_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("movie.mkv");
        let data = pseudo_random(200_000, 9);
        std::fs::write(&file, &data).unwrap();
        let r = build_torrent(&params(&file, TorrentFormat::V2, &[], &[]), &AtomicBool::new(false), &mut |_| {})
            .unwrap();
        assert!(r.info_hash_v1.is_none());
        let v2 = r.info_hash_v2.unwrap();
        assert_eq!(r.info_hash_hex(), hex(&v2[..20]));
        assert!(!contains(&r.data, b"6:pieces"));
        assert!(!contains(&r.data, b"5:files"));
        let root = reference_root(&data, 32 * 1024);
        let mut leaf = b"9:movie.mkvd0:d6:lengthi200000e11:pieces root32:".to_vec();
        leaf.extend_from_slice(&root);
        assert!(contains(&r.data, &leaf));
        // Info hash = SHA-256 of the exact info dict bytes in the file.
        let start = r.data.windows(6).position(|w| w == b"4:info").unwrap() + 6;
        let end = r.data.windows(15).position(|w| w == b"12:piece layers").unwrap();
        let info = &r.data[start..end];
        assert_eq!(sha256(info), v2);

        // Single-file hybrid: no padding, v1 `length`.
        let r = build_torrent(&params(&file, TorrentFormat::Hybrid, &[], &[]), &AtomicBool::new(false), &mut |_| {})
            .unwrap();
        let parsed = librqbit::torrent_from_bytes(&r.data).unwrap();
        assert_eq!(parsed.info.data.length, Some(200_000));
        assert!(parsed.info.data.files.is_none());
    }

    /// The engine parses the creator's pure v2 torrents: identity, layout
    /// (virtual padding between files) and every piece verifies against the
    /// merkle hashes; damaged data or piece layers are caught.
    #[test]
    fn pure_v2_parses_and_verifies_with_engine() {
        use librqbit::v2::{BLOCK_SIZE as V2_BLOCK, piece_root};
        let dir = tempfile::tempdir().unwrap();
        let root = tree(dir.path());
        std::fs::write(root.join("sub/one_piece.bin"), pseudo_random(32 * 1024, 3)).unwrap();
        let r = build_torrent(&params(&root, TorrentFormat::V2, &[], &[]), &AtomicBool::new(false), &mut |_| {})
            .unwrap();
        let v2 = r.info_hash_v2.unwrap();
        let parsed = librqbit::torrent_from_bytes(&r.data).unwrap();
        assert_eq!(parsed.info_hash.0[..], v2[..20]);
        assert_eq!(parsed.info_hash_v2.unwrap().0, v2);
        assert!(parsed.info.data.pieces.as_ref().is_empty());
        let layers = parsed.piece_layers.as_ref().map(|l| l.0.as_ref());
        let info = parsed.info.data.clone().validate().unwrap();
        assert!(info.is_pure_v2() && !info.is_single_file());
        assert!(!info.has_piece_hashes());
        assert!(info.piece_check(0).is_none());
        let info = info.with_piece_layers(layers).unwrap();

        let piece = 32 * 1024u64;
        let pad = |n: u64| piece - n % piece;
        let desc: Vec<(String, u64, bool)> = info
            .iter_file_details()
            .map(|f| (f.filename.to_vec().join("/"), f.len, f.attrs().padding))
            .collect();
        assert_eq!(
            desc,
            vec![
                ("a.bin".to_string(), 50_000, false),
                (format!(".pad/{}", pad(50_000)), pad(50_000), true),
                ("c.txt".to_string(), 4, false),
                (format!(".pad/{}", pad(4)), pad(4), true),
                ("sub/b.bin".to_string(), 100_000, false),
                (format!(".pad/{}", pad(100_000)), pad(100_000), true),
                ("sub/empty".to_string(), 0, false),
                ("sub/one_piece.bin".to_string(), piece, false),
            ]
        );
        let names = ["a.bin", "c.txt", "sub/b.bin", "sub/one_piece.bin"];
        let datas: Vec<Vec<u8>> = names.iter().map(|n| std::fs::read(root.join(n)).unwrap()).collect();
        let total: u32 = datas.iter().map(|d| (d.len() as u64).div_ceil(piece) as u32).sum();
        assert_eq!(info.lengths().total_pieces(), total);
        // Every piece verifies over its file's real data only.
        let mut p = 0u32;
        for d in &datas {
            for chunk in d.chunks(piece as usize) {
                let Some(librqbit::PieceCheck::V2(c)) = info.piece_check(p) else { panic!("piece {p}") };
                assert_eq!(c.data_len, chunk.len() as u64);
                assert_eq!(piece_root(chunk, c.leaves), Some(c.expected), "piece {p}");
                let mut bad = chunk.to_vec();
                bad[chunk.len() / 2] ^= 1;
                assert_ne!(piece_root(&bad, c.leaves), Some(c.expected));
                p += 1;
            }
        }
        // c.txt (4 bytes) is a single-block tree: its root is the block hash.
        let Some(librqbit::PieceCheck::V2(c)) = info.piece_check(2) else { panic!() };
        assert_eq!(c.leaves, 1);
        assert_eq!(c.expected, sha256(b"tiny"));
        assert_eq!(V2_BLOCK as u64 * 2, piece);

        // A flipped byte in the piece layers is refused.
        let mut evil = r.data.clone();
        let at = evil.windows(15).position(|w| w == b"12:piece layers").unwrap() + 60;
        evil[at] ^= 1;
        let p = librqbit::torrent_from_bytes(&evil).unwrap();
        let l = p.piece_layers.as_ref().map(|l| l.0.as_ref());
        assert!(p.info.data.clone().validate().unwrap().with_piece_layers(l).is_err());
        assert!(crate::bittorrent::metainfo::parse_torrent(&evil, Default::default()).is_err());
        // So is a pure v2 torrent without piece layers.
        let cut = r.data.windows(15).position(|w| w == b"12:piece layers").unwrap();
        let mut stripped = r.data[..cut].to_vec();
        stripped.push(b'e');
        let p = librqbit::torrent_from_bytes(&stripped).unwrap();
        assert!(p.info.data.clone().validate().unwrap().with_piece_layers(None).is_err());
        // The core takes it (the layers are fetched from peers).
        let partial = crate::bittorrent::metainfo::parse_torrent(&stripped, Default::default()).unwrap().meta;
        assert!(partial.missing_piece_layers);
        assert!(!crate::bittorrent::metainfo::parse_torrent(&r.data, Default::default()).unwrap().meta.missing_piece_layers);

        // The core's parser: id = truncated v2 hash, files under the
        // torrent's folder, pad files hidden.
        let core = crate::bittorrent::metainfo::parse_torrent(&r.data, Default::default()).unwrap().meta;
        assert_eq!(core.id(), hex(&v2[..20]));
        assert!(core.is_pure_v2());
        assert_eq!(core.info_hash_v1_hex(), "");
        assert_eq!(core.info_hash_v2_hex(), hex(&v2));
        let paths: Vec<&str> = core.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            ["content/a.bin", "content/c.txt", "content/sub/b.bin", "content/sub/empty", "content/sub/one_piece.bin"]
        );
        assert_eq!(core.engine_file_count, 8);
        assert_eq!(core.files[2].first_piece, 3);
        assert_eq!(core.files[2].last_piece, 6);
        assert_eq!(core.pieces, total);

        // Hybrid: id = truncated v2 hash (qBittorrent's TorrentID), the
        // engine runs it on the v1 hash, which is also its alternative id.
        let h = build_torrent(&params(&root, TorrentFormat::Hybrid, &[], &[]), &AtomicBool::new(false), &mut |_| {})
            .unwrap();
        let core = crate::bittorrent::metainfo::parse_torrent(&h.data, Default::default()).unwrap().meta;
        let v2 = h.info_hash_v2.unwrap();
        assert_eq!(core.id(), hex(&v2[..20]));
        assert_eq!(core.engine_hash_hex(), hex(&h.info_hash_v1.unwrap()));
        assert_eq!(core.alt_id(), Some(hex(&h.info_hash_v1.unwrap())));
        assert_eq!(core.info_hash_v1_hex(), hex(&h.info_hash_v1.unwrap()));
        assert_eq!(core.info_hash_v2_hex(), hex(&v2));
        assert!(!core.is_pure_v2() && core.is_hybrid());
    }

    #[test]
    fn errors() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let p = params(&empty, TorrentFormat::V1, &[], &[]);
        assert_eq!(build_torrent(&p, &AtomicBool::new(false), &mut |_| {}).unwrap_err(), "no files in torrent");
        std::fs::write(empty.join("zero"), b"").unwrap();
        assert_eq!(build_torrent(&p, &AtomicBool::new(false), &mut |_| {}).unwrap_err(), "invalid length of torrent");
        let root = tree(dir.path());
        let p = params(&root, TorrentFormat::V1, &[], &[]);
        assert_eq!(build_torrent(&p, &AtomicBool::new(true), &mut |_| {}).unwrap_err(), ABORTED);
        let mut p = params(&root, TorrentFormat::Hybrid, &[], &[]);
        p.max_torrent_size = 100;
        assert!(build_torrent(&p, &AtomicBool::new(false), &mut |_| {}).unwrap_err().contains("size limit"));
    }

    #[test]
    fn progress_reaches_100() {
        let dir = tempfile::tempdir().unwrap();
        let root = tree(dir.path());
        let mut last = 0.0;
        let mut calls = 0;
        build_torrent(&params(&root, TorrentFormat::Hybrid, &[], &[]), &AtomicBool::new(false), &mut |p| {
            assert!(p >= last);
            last = p;
            calls += 1;
        })
        .unwrap();
        assert_eq!(last, 100.0);
        assert!(calls >= 2);
    }
}
