//! Pieces shared by a wanted file and a skipped ("Do not download") file.
//!
//! libtorrent writes the bytes of skipped files into a part file
//! (`<save path>/.<id>.parts`) instead of the files themselves, so a piece
//! it reports as had may have part of its data there. Our engine has no
//! part file: a had piece must be complete in the files, otherwise it
//! reports the skipped file as missing. For every such piece this module
//! rebuilds the piece from the files on disk plus the part file, checks it
//! against the piece hash, and plans the bytes to write into the (new)
//! skipped file. A piece that can't be rebuilt and verified is unclaimed
//! (downloaded again later) instead.
//!
//! Part file layout (libtorrent `part_file`): big-endian `u32` piece
//! count, `u32` piece size, one `u32` slot per piece (`0xFFFFFFFF` = none),
//! the header padded to a multiple of 1024 bytes, then one piece-sized
//! slot after the other; a piece's bytes sit at their offset in its slot.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use sha1w::ISha1;

use crate::bittorrent::metainfo::TorrentMeta;

use super::libtorrent::QB_EXT;

/// Bytes to write into a skipped file that doesn't exist yet.
#[derive(Clone, Debug, PartialEq)]
pub struct SkippedFileWrite {
    /// Index in `TorrentMeta::files`.
    pub file: usize,
    /// Absolute target path (with the incomplete suffix when partial).
    pub path: PathBuf,
    /// (offset in the file, bytes)
    pub regions: Vec<(u64, Vec<u8>)>,
    /// The regions cover the whole file.
    pub complete: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recovery {
    /// Pieces rebuilt and verified.
    pub recovered: Vec<u32>,
    /// Pieces that had to be unclaimed, with the reason.
    pub unclaimed: Vec<(u32, String)>,
    pub writes: Vec<SkippedFileWrite>,
}

impl Recovery {
    pub fn bytes(&self) -> u64 {
        self.writes.iter().flat_map(|w| w.regions.iter()).map(|(_, b)| b.len() as u64).sum()
    }
}

/// Where a file's data is on disk now: plain name or with qBittorrent's
/// incomplete suffix.
pub fn existing_file(location: &Path, rel: &str) -> Option<PathBuf> {
    let plain = location.join(rel);
    if plain.is_file() {
        return Some(plain);
    }
    let suffixed = location.join(format!("{rel}{QB_EXT}"));
    suffixed.is_file().then_some(suffixed)
}

struct PartFile {
    file: File,
    header: u64,
    piece_size: u64,
    slots: Vec<u32>,
}

impl PartFile {
    fn open(path: &Path, pieces: u32, piece_size: u32) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("no part file ({e})"))?;
        let mut head = [0u8; 8];
        file.read_exact(&mut head).map_err(|e| format!("part file unreadable ({e})"))?;
        let n = u32::from_be_bytes(head[0..4].try_into().unwrap_or_default());
        let size = u32::from_be_bytes(head[4..8].try_into().unwrap_or_default());
        if n != pieces || size != piece_size {
            return Err(format!("part file is for {n} pieces of {size} bytes"));
        }
        let mut raw = vec![0u8; pieces as usize * 4];
        file.read_exact(&mut raw).map_err(|e| format!("part file header truncated ({e})"))?;
        let slots = raw.as_chunks::<4>().0.iter().map(|c| u32::from_be_bytes(*c)).collect();
        let header = (u64::from(pieces) * 4 + 8 + 1023) & !1023;
        Ok(Self { file, header, piece_size: u64::from(piece_size), slots })
    }

    fn read(&self, piece: u32, offset_in_piece: u64, len: usize) -> Result<Vec<u8>, String> {
        let slot = *self.slots.get(piece as usize).ok_or("piece out of range")?;
        if slot == u32::MAX || (slot as i32) < 0 {
            return Err("piece not in the part file".into());
        }
        let mut buf = vec![0u8; len];
        self.file
            .read_exact_at(&mut buf, self.header + u64::from(slot) * self.piece_size + offset_in_piece)
            .map_err(|e| format!("part file read failed ({e})"))?;
        Ok(buf)
    }
}

fn read_at(path: &Path, offset: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf).map_err(|e| format!("{} is shorter than expected ({e})", path.display()))?;
    Ok(buf)
}

/// What [`plan`] works on.
#[derive(Clone, Copy)]
pub struct Input<'a> {
    pub meta: &'a TorrentMeta,
    /// Current relative paths (our order).
    pub file_paths: &'a [String],
    pub priorities: &'a [i32],
    /// Where the data is.
    pub location: &'a Path,
    pub part_file: &'a Path,
    pub v1_hashes: &'a [u8],
    pub incomplete_suffix: bool,
}

/// Plans the recovery of had pieces overlapping skipped files that don't
/// exist in `location`. `have` is updated (unclaimed pieces cleared).
/// `v1_hashes` are the 20-byte piece hashes (empty for a pure v2 torrent:
/// nothing can be verified then, so such pieces are unclaimed).
/// Only reads files.
pub fn plan(input: &Input<'_>, have: &mut [bool]) -> Recovery {
    let Input { meta, file_paths, priorities, location, part_file, v1_hashes, incomplete_suffix } = *input;
    let mut out = Recovery::default();
    let plen = u64::from(meta.piece_length);
    let absent: Vec<bool> = meta
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            priorities.get(i).copied().unwrap_or(1) == 0
                && f.size > 0
                && existing_file(location, file_paths.get(i).map(String::as_str).unwrap_or(&f.path)).is_none()
        })
        .collect();
    let mut affected: Vec<u32> = Vec::new();
    for (i, f) in meta.files.iter().enumerate() {
        if !absent[i] {
            continue;
        }
        for p in f.first_piece..=f.last_piece {
            if have.get(p as usize).copied().unwrap_or(false) && !affected.contains(&p) {
                affected.push(p);
            }
        }
    }
    if affected.is_empty() {
        return out;
    }
    affected.sort_unstable();
    let part = PartFile::open(part_file, meta.pieces, meta.piece_length);

    // file index -> regions
    let mut planned: Vec<Vec<(u64, Vec<u8>)>> = vec![Vec::new(); meta.files.len()];
    for p in affected {
        let ps = u64::from(p) * plen;
        let pe = (ps + plen).min(meta.total_size);
        let result = (|| -> Result<Vec<(usize, u64, Vec<u8>)>, String> {
            let expected = v1_hashes.get(p as usize * 20..p as usize * 20 + 20).ok_or("no v1 piece hash to verify against")?;
            let part = part.as_ref().map_err(Clone::clone)?;
            // Bytes not covered by a file are BEP 47 padding: zeros.
            let mut piece = vec![0u8; (pe - ps) as usize];
            let mut to_write = Vec::new();
            for (i, f) in meta.files.iter().enumerate() {
                let s = ps.max(f.offset);
                let e = pe.min(f.offset + f.size);
                if e <= s {
                    continue;
                }
                let len = (e - s) as usize;
                let dst = &mut piece[(s - ps) as usize..(e - ps) as usize];
                let rel = file_paths.get(i).map(String::as_str).unwrap_or(&f.path);
                if absent[i] {
                    let bytes = part.read(p, s - ps, len)?;
                    dst.copy_from_slice(&bytes);
                    to_write.push((i, s - f.offset, bytes));
                } else {
                    let path = existing_file(location, rel).ok_or_else(|| format!("\"{rel}\" is missing"))?;
                    dst.copy_from_slice(&read_at(&path, s - f.offset, len)?);
                }
            }
            let mut h = sha1w::Sha1::new();
            h.update(&piece);
            if h.finish() != expected {
                return Err("rebuilt piece fails the hash check".into());
            }
            Ok(to_write)
        })();
        match result {
            Ok(writes) => {
                for (i, off, bytes) in writes {
                    planned[i].push((off, bytes));
                }
                out.recovered.push(p);
            }
            Err(reason) => {
                if let Some(h) = have.get_mut(p as usize) {
                    *h = false;
                }
                out.unclaimed.push((p, reason));
            }
        }
    }
    for (i, regions) in planned.into_iter().enumerate() {
        if regions.is_empty() {
            continue;
        }
        let f = &meta.files[i];
        let covered: u64 = regions.iter().map(|(_, b)| b.len() as u64).sum();
        let complete = covered == f.size;
        let rel = file_paths.get(i).map(String::as_str).unwrap_or(&f.path);
        let path = if complete || !incomplete_suffix { location.join(rel) } else { location.join(format!("{rel}{QB_EXT}")) };
        out.writes.push(SkippedFileWrite { file: i, path, regions, complete });
    }
    out
}

/// Creates the planned files. Never overwrites: a file that appeared in
/// the meantime is an error.
pub fn apply(writes: &[SkippedFileWrite]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    for w in writes {
        if let Some(dir) = w.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&w.path)
            .map_err(|e| format!("{}: {e}", w.path.display()))?;
        for (off, bytes) in &w.regions {
            f.write_all_at(bytes, *off).map_err(|e| format!("{}: {e}", w.path.display()))?;
        }
        f.sync_all().map_err(|e| format!("{}: {e}", w.path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bittorrent::bencode::{Limits, Value};
    use crate::bittorrent::metainfo;
    use crate::migrate::libtorrent::tests::sample_torrent;

    fn part_file(pieces: u32, piece_size: u32, slots: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&pieces.to_be_bytes());
        out.extend_from_slice(&piece_size.to_be_bytes());
        let mut map = vec![u32::MAX; pieces as usize];
        for (slot, (p, _)) in slots.iter().enumerate() {
            map[*p as usize] = slot as u32;
        }
        for s in map {
            out.extend_from_slice(&s.to_be_bytes());
        }
        out.resize((pieces as usize * 4 + 8).div_ceil(1024) * 1024, 0);
        for (_, data) in slots {
            let mut d = data.clone();
            d.resize(piece_size as usize, 0);
            out.extend_from_slice(&d);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn plan8(
        meta: &TorrentMeta,
        file_paths: &[String],
        priorities: &[i32],
        have: &mut [bool],
        location: &Path,
        part_file: &Path,
        v1_hashes: &[u8],
        incomplete_suffix: bool,
    ) -> Recovery {
        plan(&Input { meta, file_paths, priorities, location, part_file, v1_hashes, incomplete_suffix }, have)
    }

    fn hashes(tor: &[u8]) -> Vec<u8> {
        let v = crate::bittorrent::bencode::decode(tor, Limits::default()).unwrap();
        v.get("info").and_then(|i| i.get("pieces")).and_then(Value::as_bytes).unwrap().to_vec()
    }

    #[test]
    fn rebuilds_shared_piece_from_part_file() {
        // a.mkv = 40 bytes (pieces 0..2, piece 2 holds a's last 8 bytes),
        // b.nfo = 4 bytes at the end of piece 2, skipped, in the part file.
        let a: Vec<u8> = (0..40u8).collect();
        let tor = sample_torrent(16, &a, b"nfo!");
        let meta = metainfo::parse_torrent(&tor, Limits::default()).unwrap().meta;
        let dir = tempfile::tempdir().unwrap();
        let loc = dir.path().join("Movies");
        std::fs::create_dir_all(&loc).unwrap();
        std::fs::write(loc.join("a.mkv"), &a).unwrap();
        let mut slot = vec![0u8; 16];
        slot[8..12].copy_from_slice(b"nfo!");
        let pf = dir.path().join("parts");
        std::fs::write(&pf, part_file(3, 16, &[(2, slot)])).unwrap();

        let paths = vec!["a.mkv".to_string(), "b.nfo".to_string()];
        let mut have = vec![true, true, true];
        let rec = plan8(&meta, &paths, &[1, 0], &mut have, &loc, &pf, &hashes(&tor), true);
        assert_eq!(rec.recovered, vec![2]);
        assert!(rec.unclaimed.is_empty());
        assert_eq!(have, vec![true, true, true]);
        assert_eq!(rec.writes.len(), 1);
        assert!(rec.writes[0].complete);
        assert_eq!(rec.writes[0].path, loc.join("b.nfo"));
        assert_eq!(rec.writes[0].regions, vec![(0, b"nfo!".to_vec())]);
        assert_eq!(rec.bytes(), 4);
        // Planning only reads.
        assert!(!loc.join("b.nfo").exists());
        apply(&rec.writes).unwrap();
        assert_eq!(std::fs::read(loc.join("b.nfo")).unwrap(), b"nfo!");
        // Never overwrites.
        assert!(apply(&rec.writes).is_err());
        // Now present: nothing left to do.
        let mut have = vec![true, true, true];
        assert_eq!(plan8(&meta, &paths, &[1, 0], &mut have, &loc, &pf, &hashes(&tor), true), Recovery::default());
    }

    #[test]
    fn unclaims_what_cannot_be_verified() {
        let a: Vec<u8> = (0..40u8).collect();
        let tor = sample_torrent(16, &a, b"nfo!");
        let meta = metainfo::parse_torrent(&tor, Limits::default()).unwrap().meta;
        let dir = tempfile::tempdir().unwrap();
        let loc = dir.path().to_path_buf();
        std::fs::write(loc.join("a.mkv"), &a).unwrap();
        let paths = vec!["a.mkv".to_string(), "b.nfo".to_string()];

        // No part file.
        let mut have = vec![true, true, true];
        let rec = plan8(&meta, &paths, &[1, 0], &mut have, &loc, &loc.join("none"), &hashes(&tor), true);
        assert_eq!(have, vec![true, true, false]);
        assert_eq!(rec.unclaimed.len(), 1);
        assert!(rec.writes.is_empty());

        // Wrong bytes in the part file.
        let mut slot = vec![0u8; 16];
        slot[8..12].copy_from_slice(b"NFO?");
        let pf = loc.join("parts");
        std::fs::write(&pf, part_file(3, 16, &[(2, slot)])).unwrap();
        let mut have = vec![true, true, true];
        let rec = plan8(&meta, &paths, &[1, 0], &mut have, &loc, &pf, &hashes(&tor), true);
        assert_eq!(have, vec![true, true, false]);
        assert!(rec.unclaimed[0].1.contains("hash"));

        // Part file for another torrent shape.
        std::fs::write(&pf, part_file(4, 16, &[])).unwrap();
        let mut have = vec![true, true, true];
        let rec = plan8(&meta, &paths, &[1, 0], &mut have, &loc, &pf, &hashes(&tor), true);
        assert!(rec.unclaimed[0].1.contains("part file is for"));

        // Pure v2 (no v1 hashes): unclaimed.
        let mut have = vec![true, true, true];
        let rec = plan8(&meta, &paths, &[1, 0], &mut have, &loc, &pf, &[], true);
        assert_eq!(have, vec![true, true, false]);
        assert!(rec.unclaimed[0].1.contains("v1"));

        // Piece not had: nothing to do.
        let mut have = vec![true, true, false];
        assert_eq!(plan8(&meta, &paths, &[1, 0], &mut have, &loc, &pf, &hashes(&tor), true), Recovery::default());
    }

    #[test]
    fn partial_skipped_file_gets_the_suffix() {
        // b is skipped and spans pieces 1..2; only piece 2 (shared with c)
        // is had.
        let mut all: Vec<u8> = (0..48u8).collect();
        all.truncate(48);
        let info_files = [("a", 10usize), ("b", 30), ("c", 8)];
        let mut hashes_v = Vec::new();
        for chunk in all.chunks(16) {
            let mut h = sha1w::Sha1::new();
            h.update(chunk);
            hashes_v.extend_from_slice(&h.finish());
        }
        use crate::migrate::libtorrent::tests::{b, dict, enc};
        let info = dict(vec![
            ("name", b("d")),
            ("piece length", Value::Int(16)),
            ("pieces", Value::Bytes(hashes_v.clone())),
            (
                "files",
                Value::List(
                    info_files
                        .iter()
                        .map(|(n, l)| dict(vec![("length", Value::Int(*l as i64)), ("path", Value::List(vec![b(n)]))]))
                        .collect(),
                ),
            ),
        ]);
        let tor = enc(&dict(vec![("info", info)]));
        let meta = metainfo::parse_torrent(&tor, Limits::default()).unwrap().meta;
        let dir = tempfile::tempdir().unwrap();
        let loc = dir.path().to_path_buf();
        std::fs::write(loc.join("a"), &all[0..10]).unwrap();
        std::fs::write(loc.join("c"), &all[40..48]).unwrap();
        let mut slot = vec![0u8; 16];
        slot[0..8].copy_from_slice(&all[32..40]);
        let pf = loc.join("parts");
        std::fs::write(&pf, part_file(3, 16, &[(2, slot)])).unwrap();
        let paths = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut have = vec![false, false, true];
        let rec = plan8(&meta, &paths, &[1, 0, 1], &mut have, &loc, &pf, &hashes_v, true);
        assert_eq!(rec.recovered, vec![2]);
        assert_eq!(rec.writes[0].path, loc.join("b.!qB"));
        assert!(!rec.writes[0].complete);
        assert_eq!(rec.writes[0].regions, vec![(22, all[32..40].to_vec())]);
        let rec = plan8(&meta, &paths, &[1, 0, 1], &mut [false, false, true], &loc, &pf, &hashes_v, false);
        assert_eq!(rec.writes[0].path, loc.join("b"));
    }
}
