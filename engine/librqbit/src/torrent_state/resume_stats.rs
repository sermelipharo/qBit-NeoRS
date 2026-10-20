//! File sizes and modification times saved next to the fastresume bitfield, so
//! that a restart can trust the bitfield without reading pieces back.
//!
//! When the torrent's files are recorded, every file's length and mtime is
//! stored together with a snapshot of the have-bitfield. On load, a file whose
//! length and mtime are unchanged is trusted: its pieces that are both in the
//! loaded bitfield and in the snapshot are kept without hashing. Pieces touching
//! a changed (or uncertain) file are hashed; pieces touching a missing file are
//! dropped. Fastresume data without this record is validated the old way
//! (sampling a few pieces).
//!
//! # Why recording is safe while pieces are being written
//!
//! The files are stat'ed first and the have-bitfield is snapshotted after
//! that. A piece is only marked as had after all its data was written (and
//! hashed). Take a piece `p` in the snapshot of a file whose mtime is unchanged
//! at the next start:
//! * if `p`'s data was written before the stat, the data on disk at stat time
//!   was complete, and nothing wrote to the file since (unchanged mtime);
//! * if any of `p`'s data was written after the stat, that write bumped the
//!   mtime, so the file doesn't match and `p` is hashed.
//!
//! Taking the snapshot first would be unsafe: a piece could be in the snapshot,
//! then be found bad and partly rewritten before the stat, which would then
//! record the half-written file as current.
//!
//! The loaded bitfield (flushed asynchronously, possibly before or after the
//! record) is intersected with the snapshot: a piece cleared in memory but not
//! yet flushed is not in the snapshot, and pieces only in the loaded bitfield
//! are hashed.
//!
//! Coarse timestamps: a write in the same clock tick as the recorded mtime
//! would not change it. Files modified within [`RACY_WINDOW`] of the stat are
//! therefore recorded as uncertain and get their pieces hashed on load.

use std::{
    path::Path,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use librqbit_core::lengths::{Lengths, ValidPieceIndex};

use crate::{
    bitv::BitV,
    storage::StorageLayout,
    type_aliases::{BF, BS, FileInfos},
};

/// Files modified this recently (or in the future) when recorded are uncertain.
const RACY_WINDOW: Duration = Duration::from_secs(2);

const MAGIC: &[u8; 4] = b"NRFS";
const VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileStat {
    pub len: u64,
    pub mtime_secs: u64,
    pub mtime_nanos: u32,
}

impl FileStat {
    fn from_metadata(m: &std::fs::Metadata) -> Option<Self> {
        let d = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
        Some(Self {
            len: m.len(),
            mtime_secs: d.as_secs(),
            mtime_nanos: d.subsec_nanos(),
        })
    }

    fn mtime(&self) -> SystemTime {
        UNIX_EPOCH + Duration::new(self.mtime_secs, self.mtime_nanos)
    }
}

/// Current length and mtime of a file, `None` if it doesn't exist (or isn't a
/// regular file).
pub(crate) fn stat_file(path: &Path) -> Option<FileStat> {
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() {
        return None;
    }
    FileStat::from_metadata(&m)
}

/// Like [`stat_file`], but `None` for files modified too recently to be sure a
/// later write would change their mtime.
fn stat_file_for_record(path: &Path) -> Option<FileStat> {
    let st = stat_file(path)?;
    let now = SystemTime::now();
    (st.mtime() + RACY_WINDOW <= now).then_some(st)
}

/// Whether a file is ignored for matching (not on disk or holding no data).
fn is_ignored(fi: &crate::file_info::FileInfo) -> bool {
    fi.attrs.padding || fi.len == 0
}

/// The saved record: per file `(len, mtime)` (`None`: missing or uncertain) and
/// the have-bitfield snapshot taken right after the files were stat'ed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResumeStats {
    pub have: BF,
    pub files: Vec<Option<FileStat>>,
}

impl ResumeStats {
    /// Records the files at `path_of(idx)`, then calls `snapshot` for the
    /// have-bitfield (in this order, see the module docs).
    pub fn record(
        file_infos: &FileInfos,
        path_of: impl Fn(usize) -> Option<std::path::PathBuf>,
        snapshot: impl FnOnce() -> Option<BF>,
    ) -> Option<Self> {
        let files = file_infos
            .iter()
            .enumerate()
            .map(|(idx, fi)| {
                if is_ignored(fi) {
                    return None;
                }
                stat_file_for_record(&path_of(idx)?)
            })
            .collect();
        let have = snapshot()?;
        Some(Self { have, files })
    }

    pub fn encode(&self) -> Vec<u8> {
        let have = self.have.as_raw_slice();
        let mut out = Vec::with_capacity(4 + 1 + 8 + have.len() + 8 + self.files.len() * 21);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&(have.len() as u64).to_le_bytes());
        out.extend_from_slice(have);
        out.extend_from_slice(&(self.files.len() as u64).to_le_bytes());
        for f in &self.files {
            match f {
                Some(f) => {
                    out.push(1);
                    out.extend_from_slice(&f.len.to_le_bytes());
                    out.extend_from_slice(&f.mtime_secs.to_le_bytes());
                    out.extend_from_slice(&f.mtime_nanos.to_le_bytes());
                }
                None => out.push(0),
            }
        }
        out
    }

    /// `None` if the data is malformed or of an unknown version.
    pub fn decode(mut b: &[u8]) -> Option<Self> {
        fn take<'a>(b: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
            if b.len() < n {
                return None;
            }
            let (head, tail) = b.split_at(n);
            *b = tail;
            Some(head)
        }
        fn u32_le(b: &mut &[u8]) -> Option<u32> {
            Some(u32::from_le_bytes(take(b, 4)?.try_into().ok()?))
        }
        fn u64_le(b: &mut &[u8]) -> Option<u64> {
            Some(u64::from_le_bytes(take(b, 8)?.try_into().ok()?))
        }

        if take(&mut b, 4)? != MAGIC || take(&mut b, 1)? != [VERSION] {
            return None;
        }
        let have_len = usize::try_from(u64_le(&mut b)?).ok()?;
        let have = BF::from_boxed_slice(take(&mut b, have_len)?.into());
        let count = usize::try_from(u64_le(&mut b)?).ok()?;
        // Each entry takes at least one byte: don't preallocate from a bogus count.
        if count > b.len() {
            return None;
        }
        let mut files = Vec::with_capacity(count);
        for _ in 0..count {
            let f = match take(&mut b, 1)?[0] {
                0 => None,
                1 => Some(FileStat {
                    len: u64_le(&mut b)?,
                    mtime_secs: u64_le(&mut b)?,
                    mtime_nanos: u32_le(&mut b)?,
                }),
                _ => return None,
            };
            files.push(f);
        }
        b.is_empty().then_some(Self { have, files })
    }

    /// Whether the record belongs to a torrent of this shape.
    pub fn fits(&self, lengths: &Lengths, file_infos: &FileInfos) -> bool {
        self.have.as_raw_slice().len() == lengths.piece_bitfield_bytes()
            && self.files.len() == file_infos.len()
    }
}

/// What to do with the loaded bitfield.
#[derive(Debug)]
pub(crate) struct ResumePlan {
    /// Pieces kept without reading.
    pub trusted: BF,
    /// Pieces to hash; kept if they match.
    pub verify: BF,
}

/// Compares the saved record with the files' current state (`current[idx]`,
/// `None` if missing). Pieces of the loaded bitfield touching a missing file are
/// dropped, those touching a changed file are hashed, the rest (if also in the
/// saved snapshot) are trusted.
pub(crate) fn plan(
    file_infos: &FileInfos,
    loaded: &BS,
    saved: &ResumeStats,
    current: &[Option<FileStat>],
) -> ResumePlan {
    let len = loaded.len();
    let mut changed = BF::from_bitslice(loaded);
    changed.fill(false);
    let mut missing = changed.clone();
    for (idx, fi) in file_infos.iter().enumerate() {
        if is_ignored(fi) {
            continue;
        }
        let range = fi.piece_range_usize();
        let range = range.start.min(len)..range.end.min(len);
        let cur = current.get(idx).copied().flatten();
        let target = match (saved.files.get(idx).copied().flatten(), cur) {
            (_, None) => &mut missing,
            (Some(s), Some(c)) if s == c => continue,
            _ => &mut changed,
        };
        target[range].fill(true);
    }

    let mut trusted = BF::from_bitslice(loaded);
    trusted &= saved.have.as_bitslice();
    trusted &= !changed;
    let missing = !missing;
    trusted &= missing.as_bitslice();

    let mut verify = BF::from_bitslice(loaded);
    verify &= missing.as_bitslice();
    let not_trusted = !trusted.clone();
    verify &= not_trusted.as_bitslice();
    ResumePlan { trusted, verify }
}

/// Hashes the pieces in `plan.verify` with `check` and returns the trusted
/// pieces plus those that matched. Progress is reported in `progress` as a share
/// of `total_length`; stops with an error when `pause_requested` is set.
pub(crate) fn run_plan(
    plan: ResumePlan,
    lengths: &Lengths,
    progress: &AtomicU64,
    pause_requested: &AtomicBool,
    mut check: impl FnMut(ValidPieceIndex) -> bool,
) -> anyhow::Result<BF> {
    let ResumePlan {
        mut trusted,
        verify,
    } = plan;
    let count = verify.count_ones();
    for (n, id) in verify.iter_ones().enumerate() {
        if pause_requested.load(Ordering::Relaxed) {
            anyhow::bail!("initial check paused");
        }
        let Some(piece) = u32::try_from(id)
            .ok()
            .and_then(|id| lengths.validate_piece_index(id))
        else {
            continue;
        };
        if check(piece) {
            trusted.set(id, true);
        }
        #[allow(clippy::cast_possible_truncation)]
        let p = (lengths.total_length() as f64 / count as f64 * (n + 1) as f64) as u64;
        progress.store(p.min(lengths.total_length()), Ordering::Relaxed);
    }
    Ok(trusted)
}

/// The record to save for a torrent (see the module docs for the order), or
/// `None` when none should be kept: storage not on disk, or pieces assumed
/// present without hashing ("skip hash check"), which must not become trusted.
pub(crate) fn record_for(
    layout: &StorageLayout,
    file_infos: &FileInfos,
    snapshot: impl FnOnce() -> Option<BF>,
) -> Option<ResumeStats> {
    if !layout.is_disk_backed() || layout.unverified_piece_count() > 0 {
        return None;
    }
    ResumeStats::record(file_infos, |idx| layout.full_actual_path(idx), snapshot)
}

/// A copy of a have-bitfield.
pub(crate) fn copy_bits(b: &dyn BitV) -> BF {
    BF::from_boxed_slice(b.as_bytes().into())
}

/// Current state of every file, for [`plan`].
pub(crate) fn current_stats(
    file_infos: &FileInfos,
    path_of: impl Fn(usize) -> Option<std::path::PathBuf>,
) -> Vec<Option<FileStat>> {
    file_infos
        .iter()
        .enumerate()
        .map(|(idx, fi)| {
            if is_ignored(fi) {
                return None;
            }
            stat_file(&path_of(idx)?)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        path::PathBuf,
        sync::atomic::{AtomicBool, AtomicU64},
    };

    use librqbit_core::{lengths::Lengths, torrent_metainfo::FileDetailsAttrs};

    use super::*;
    use crate::file_info::FileInfo;

    // Three files, 4 pieces of 10 bytes: file 0 = piece 0, file 1 = pieces 1..3
    // (piece 2 shared with file 2), file 2 = pieces 2..4.
    fn layout() -> (Lengths, FileInfos) {
        let lengths = Lengths::new(40, 10).unwrap();
        let f = |offset: u64, len: u64, pieces: std::ops::Range<u32>| FileInfo {
            relative_filename: PathBuf::from(format!("f{offset}")),
            offset_in_torrent: offset,
            piece_range: pieces,
            attrs: FileDetailsAttrs::default(),
            len,
        };
        (
            lengths,
            vec![f(0, 10, 0..1), f(10, 15, 1..3), f(25, 15, 2..4)],
        )
    }

    fn bf(bits: &[bool]) -> BF {
        let mut b = BF::from_boxed_slice(vec![0u8; 1].into());
        for (i, v) in bits.iter().enumerate() {
            b.set(i, *v);
        }
        b
    }

    fn st(len: u64, secs: u64) -> Option<FileStat> {
        Some(FileStat {
            len,
            mtime_secs: secs,
            mtime_nanos: 7,
        })
    }

    fn run(plan: ResumePlan, lengths: &Lengths, bad: &[u32], reads: &Cell<Vec<u32>>) -> BF {
        run_plan(
            plan,
            lengths,
            &AtomicU64::new(0),
            &AtomicBool::new(false),
            |p| {
                let mut r = reads.take();
                r.push(p.get());
                reads.set(r);
                !bad.contains(&p.get())
            },
        )
        .unwrap()
    }

    #[test]
    fn test_unchanged_files_need_no_reads() {
        let (lengths, fis) = layout();
        let all = bf(&[true; 4]);
        let saved = ResumeStats {
            have: all.clone(),
            files: vec![st(10, 1), st(15, 2), st(15, 3)],
        };
        let p = plan(&fis, &all, &saved, &saved.files);
        assert!(p.verify.not_any());
        let reads = Cell::new(Vec::new());
        assert_eq!(run(p, &lengths, &[], &reads), all);
        assert!(reads.take().is_empty());
    }

    #[test]
    fn test_touched_file_rechecks_only_its_pieces() {
        let (lengths, fis) = layout();
        let all = bf(&[true; 4]);
        let saved = ResumeStats {
            have: all.clone(),
            files: vec![st(10, 1), st(15, 2), st(15, 3)],
        };
        // File 2 was written to: pieces 2 and 3 are hashed, piece 3 is bad.
        let current = vec![st(10, 1), st(15, 2), st(15, 4)];
        let p = plan(&fis, &all, &saved, &current);
        let reads = Cell::new(Vec::new());
        let have = run(p, &lengths, &[3], &reads);
        assert_eq!(reads.take(), vec![2, 3]);
        assert_eq!(have, bf(&[true, true, true, false]));
    }

    #[test]
    fn test_missing_uncertain_and_unsnapshotted_pieces() {
        let (lengths, fis) = layout();
        let loaded = bf(&[true, true, true, true]);
        let saved = ResumeStats {
            // Piece 1 was cleared in memory but the flushed bitfield still has it.
            have: bf(&[true, false, true, true]),
            // File 0 was uncertain when recorded.
            files: vec![None, st(15, 2), st(15, 3)],
        };
        // File 2 is gone: pieces 2 and 3 are dropped without reading.
        let current = vec![st(10, 1), st(15, 2), None];
        let p = plan(&fis, &loaded, &saved, &current);
        let reads = Cell::new(Vec::new());
        let have = run(p, &lengths, &[1], &reads);
        assert_eq!(reads.take(), vec![0, 1]);
        assert_eq!(have, bf(&[true, false, false, false]));
    }

    #[test]
    fn test_size_change_and_pieces_not_loaded() {
        let (lengths, fis) = layout();
        let loaded = bf(&[true, false, false, false]);
        let saved = ResumeStats {
            have: bf(&[true; 4]),
            files: vec![st(10, 1), st(15, 2), st(15, 3)],
        };
        // Same mtime, other size: hashed. Pieces not in the loaded bitfield stay out.
        let current = vec![st(11, 1), st(15, 2), st(15, 3)];
        let p = plan(&fis, &loaded, &saved, &current);
        let reads = Cell::new(Vec::new());
        assert_eq!(run(p, &lengths, &[], &reads), loaded);
        assert_eq!(reads.take(), vec![0]);
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        let s = ResumeStats {
            have: bf(&[true, false, true]),
            files: vec![st(10, 1), None, st(u64::MAX, 3)],
        };
        let enc = s.encode();
        assert_eq!(ResumeStats::decode(&enc), Some(s.clone()));
        // Truncated, trailing garbage, other versions: rejected.
        assert_eq!(ResumeStats::decode(&enc[..enc.len() - 1]), None);
        let mut longer = enc.clone();
        longer.push(0);
        assert_eq!(ResumeStats::decode(&longer), None);
        let mut v2 = enc.clone();
        v2[4] = 2;
        assert_eq!(ResumeStats::decode(&v2), None);
        assert_eq!(ResumeStats::decode(b""), None);
        let (lengths, fis) = layout();
        assert!(s.fits(&lengths, &fis));
    }

    #[test]
    fn test_recent_files_are_recorded_uncertain() {
        let dir = tempfile::TempDir::new().unwrap();
        let old = dir.path().join("old");
        let fresh = dir.path().join("fresh");
        std::fs::write(&old, b"0123456789").unwrap();
        std::fs::write(&fresh, b"0123456789").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(60))
            .unwrap();
        assert!(stat_file_for_record(&old).is_some());
        assert!(stat_file_for_record(&fresh).is_none());
        assert_eq!(stat_file(&fresh).unwrap().len, 10);
        assert!(stat_file(&dir.path().join("nope")).is_none());
        assert!(stat_file(dir.path()).is_none());
    }
}
