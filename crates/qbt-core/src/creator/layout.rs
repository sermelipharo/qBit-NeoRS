//! Input file collection and the file layout of the generated torrent.
//!
//! Collection follows qBittorrent's `TorrentCreator::run`: a single file is
//! taken as is; a folder is walked recursively, directories are ordered by
//! natural case-insensitive comparison of their paths and the files of each
//! directory are ordered the same way. Names starting with `.` are skipped
//! (and, on macOS, entries carrying the "hidden" flag, which Qt also treats
//! as hidden). FIFOs, sockets and devices are skipped like `QDir::Files`.
//!
//! Symbolic links: libtorrent/qBittorrent read through symlinks. To avoid
//! accidentally publishing data from outside the selected folder (e.g. a
//! downloaded folder containing a link to `~/.ssh/id_rsa`), a link is
//! followed only when its resolved target is a regular file inside the
//! source folder; links to directories are never traversed (no loops), and
//! links leaving the folder are skipped and reported.
//!
//! Layout: v1 torrents keep the collection order; with "optimize alignment"
//! libtorrent 1.2's reordering/padding algorithm is applied. v2 and hybrid
//! torrents use libtorrent 2.0's canonical layout: files sorted by path
//! components and, in multi-file torrents, a `.pad/<size>` file after every
//! non-empty file whose end is not piece aligned, so each file starts on a
//! piece boundary (required for hybrid v1/v2 consistency).

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use crate::utils::compare::natural_cmp_ci;

/// Upper bound on the number of files in one torrent; protects the creator
/// from unbounded memory use on enormous trees.
pub const MAX_FILES: usize = 1_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    /// Path components relative to the torrent root. For a single-file
    /// torrent this is just the file name.
    pub path: Vec<String>,
    pub size: u64,
    /// Where to read the data from.
    pub disk_path: PathBuf,
    /// True when `disk_path` is an already resolved symlink target (the open
    /// may follow links); false for plain entries, which are opened with
    /// `O_NOFOLLOW` so a file swapped for a link mid-creation is refused.
    pub resolved_link: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Collected {
    /// Torrent name (the file or folder name).
    pub name: String,
    pub files: Vec<SourceFile>,
    /// Folder source (paths have a parent); a single selected file is not.
    pub multi_file: bool,
    /// Links that were not followed, relative to the source (for the log).
    pub skipped_links: Vec<String>,
}

pub const ABORTED: &str = "Operation aborted";

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(AtomicOrdering::Relaxed) { Err(ABORTED.to_string()) } else { Ok(()) }
}

fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

#[cfg(target_os = "macos")]
fn has_hidden_flag(meta: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    const UF_HIDDEN: u32 = 0x0000_8000;
    meta.st_flags() & UF_HIDDEN != 0
}

#[cfg(not(target_os = "macos"))]
fn has_hidden_flag(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Collects the files of `source` (absolute path to a file or folder).
pub fn collect_files(source: &Path, cancel: &AtomicBool) -> Result<Collected, String> {
    const NOT_READABLE: &str = "Path to file/folder is not readable.";
    if !source.is_absolute() {
        return Err(NOT_READABLE.to_string());
    }
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| NOT_READABLE.to_string())?
        .to_string();
    let meta = std::fs::metadata(source).map_err(|_| NOT_READABLE.to_string())?;

    if meta.is_file() {
        // `lt::add_files` applies qBittorrent's filter to the selected file too.
        let files = if is_hidden_name(&name) {
            Vec::new()
        } else {
            let disk_path = std::fs::canonicalize(source).map_err(|e| e.to_string())?;
            vec![SourceFile { path: vec![name.clone()], size: meta.len(), disk_path, resolved_link: true }]
        };
        return Ok(Collected { name, files, multi_file: false, skipped_links: Vec::new() });
    }
    if !meta.is_dir() {
        return Err(NOT_READABLE.to_string());
    }

    let root = std::fs::canonicalize(source).map_err(|_| NOT_READABLE.to_string())?;
    std::fs::read_dir(&root).map_err(|_| NOT_READABLE.to_string())?;

    struct Dir {
        rel: Vec<String>,
        abs: PathBuf,
        files: Vec<(String, u64, PathBuf, bool)>,
    }
    let mut dirs = vec![Dir { rel: Vec::new(), abs: root.clone(), files: Vec::new() }];
    let mut skipped_links = Vec::new();
    let mut file_count = 0usize;
    let mut next = 0;
    while next < dirs.len() {
        check_cancel(cancel)?;
        let abs = dirs[next].abs.clone();
        let rel = dirs[next].rel.clone();
        let entries = std::fs::read_dir(&abs).map_err(|e| format!("{}: {e}", abs.display()))?;
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", abs.display()))?;
            let os_name = entry.file_name();
            let Some(name) = os_name.to_str().map(str::to_string) else {
                return Err(format!("File name is not valid UTF-8: {}", entry.path().display()));
            };
            if is_hidden_name(&name) {
                continue;
            }
            // `DirEntry::metadata` does not traverse symlinks.
            let lmeta = entry.metadata().map_err(|e| format!("{}: {e}", entry.path().display()))?;
            if has_hidden_flag(&lmeta) {
                continue;
            }
            let ft = lmeta.file_type();
            if ft.is_dir() {
                let mut sub = rel.clone();
                sub.push(name);
                dirs.push(Dir { rel: sub, abs: entry.path(), files: Vec::new() });
            } else if ft.is_file() {
                files.push((name, lmeta.len(), entry.path(), false));
            } else if ft.is_symlink() {
                let shown = rel.iter().chain(std::iter::once(&name)).cloned().collect::<Vec<_>>().join("/");
                match std::fs::canonicalize(entry.path()) {
                    Ok(target) if target.starts_with(&root) => match std::fs::metadata(&target) {
                        Ok(m) if m.is_file() => files.push((name, m.len(), target, true)),
                        _ => skipped_links.push(shown),
                    },
                    _ => skipped_links.push(shown),
                }
            }
            // FIFOs, sockets and device nodes are not regular files: skipped.
            if files.len() + file_count > MAX_FILES {
                return Err(format!("Too many files (the limit is {MAX_FILES})."));
            }
        }
        file_count += files.len();
        dirs[next].files = files;
        next += 1;
    }

    dirs.sort_by(|a, b| natural_cmp_ci(&a.rel.join("/"), &b.rel.join("/")));
    let mut out = Vec::with_capacity(file_count);
    for dir in dirs {
        let mut files = dir.files;
        files.sort_by(|a, b| natural_cmp_ci(&a.0, &b.0));
        for (name, size, disk_path, resolved_link) in files {
            let mut path = dir.rel.clone();
            path.push(name);
            out.push(SourceFile { path, size, disk_path, resolved_link });
        }
    }
    Ok(Collected { name, files: out, multi_file: true, skipped_links })
}

/// One entry of the torrent's file list: a source file or a padding file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutEntry {
    /// Index into the collected files; `None` for padding files.
    pub file: Option<usize>,
    pub size: u64,
    /// Padding file path (`.pad/N`); empty for real files.
    pub pad_path: Vec<String>,
}

impl LayoutEntry {
    fn real(index: usize, size: u64) -> Self {
        Self { file: Some(index), size, pad_path: Vec::new() }
    }
    fn pad(size: u64, name: String) -> Self {
        Self { file: None, size, pad_path: vec![".pad".to_string(), name] }
    }
    pub fn is_pad(&self) -> bool {
        self.file.is_none()
    }
}

/// Component-wise byte comparison of torrent paths (libtorrent
/// `path_compare`); this is also the key order of the v2 `file tree`.
fn path_cmp(a: &[String], b: &[String]) -> Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        match x.as_bytes().cmp(y.as_bytes()) {
            Ordering::Equal => continue,
            o => return o,
        }
    }
    a.len().cmp(&b.len())
}

/// libtorrent 2.0 `file_storage::canonicalize()`: sort by path and pad the
/// end of every non-empty file to a piece boundary (multi-file only).
pub fn canonical_layout(files: &[SourceFile], piece_len: u64) -> Vec<LayoutEntry> {
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by(|&a, &b| path_cmp(&files[a].path, &files[b].path));
    let pad = files.len() > 1;
    let mut out = Vec::with_capacity(if pad { files.len() * 2 } else { files.len() });
    let mut off = 0u64;
    for i in order {
        out.push(LayoutEntry::real(i, files[i].size));
        off += files[i].size;
        if pad && files[i].size > 0 && !off.is_multiple_of(piece_len) {
            let pad_size = piece_len - off % piece_len;
            out.push(LayoutEntry::pad(pad_size, pad_size.to_string()));
            off += pad_size;
        }
    }
    out
}

/// libtorrent 1.2 `file_storage::optimize(pad_file_limit, alignment, false)`
/// as used by "Optimize alignment": at aligned offsets it places a file whose
/// size is a multiple of the alignment (else the largest remaining file);
/// with `pad_file_limit >= 0`, files larger than the limit that would start
/// unaligned get a filler file or a `.pad/<n>` padding file in front.
pub fn optimized_v1_layout(files: &[SourceFile], alignment: u64, pad_file_limit: i64) -> Vec<LayoutEntry> {
    let mut v: Vec<LayoutEntry> = files.iter().enumerate().map(|(i, f)| LayoutEntry::real(i, f.size)).collect();
    let mut off = 0u64;
    let mut pad_counter = 0u64;
    let mut i = 0;
    while i < v.len() {
        if off.is_multiple_of(alignment) {
            let mut best = i;
            for k in i..v.len() {
                if v[k].size.is_multiple_of(alignment) {
                    best = k;
                    break;
                }
                if v[best].size < v[k].size {
                    best = k;
                }
            }
            if best != i {
                v.swap(best, i);
            }
        } else if pad_file_limit >= 0 && v[i].size > pad_file_limit as u64 && !v[i].is_pad() {
            let pad_size = alignment - off % alignment;
            if pad_file_limit > 0 {
                let mut best: Option<usize> = None;
                for j in i + 1..v.len() {
                    if v[j].size > pad_size {
                        continue;
                    }
                    if best.is_none_or(|b| v[j].size > v[b].size) {
                        best = Some(j);
                    }
                }
                if let Some(j) = best {
                    v.swap(j, i);
                    off += v[i].size;
                    i += 1;
                    continue;
                }
            }
            // Append the pad file and swap it into place (the displaced file
            // moves to the end, exactly like libtorrent).
            v.push(LayoutEntry::pad(pad_size, pad_counter.to_string()));
            pad_counter += 1;
            let last = v.len() - 1;
            v.swap(last, i);
            off += pad_size;
            i += 1;
            continue;
        }
        off += v[i].size;
        i += 1;
    }
    v
}

/// Plain layout: one entry per file in collection order.
pub fn plain_layout(files: &[SourceFile]) -> Vec<LayoutEntry> {
    files.iter().enumerate().map(|(i, f)| LayoutEntry::real(i, f.size)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf(path: &str, size: u64) -> SourceFile {
        SourceFile {
            path: path.split('/').map(str::to_string).collect(),
            size,
            disk_path: PathBuf::new(),
            resolved_link: false,
        }
    }

    #[test]
    fn collects_with_qbittorrent_rules() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Show");
        std::fs::create_dir_all(root.join("Season 10")).unwrap();
        std::fs::create_dir_all(root.join("season 2")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("b.txt"), b"bb").unwrap();
        std::fs::write(root.join("A.txt"), b"a").unwrap();
        std::fs::write(root.join(".hidden"), b"x").unwrap();
        std::fs::write(root.join(".git/config"), b"x").unwrap();
        std::fs::write(root.join("Season 10/e1.mkv"), b"1").unwrap();
        std::fs::write(root.join("season 2/e10.mkv"), b"10").unwrap();
        std::fs::write(root.join("season 2/e9.mkv"), b"9").unwrap();
        // Links: inside the root (followed), outside (skipped), to a dir (skipped).
        let outside = dir.path().join("secret.txt");
        std::fs::write(&outside, b"secret").unwrap();
        std::os::unix::fs::symlink(root.join("b.txt"), root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("leak.txt")).unwrap();
        std::os::unix::fs::symlink(root.join("season 2"), root.join("loop")).unwrap();

        let c = collect_files(&root, &AtomicBool::new(false)).unwrap();
        assert_eq!(c.name, "Show");
        assert!(c.multi_file);
        let paths: Vec<String> = c.files.iter().map(|f| f.path.join("/")).collect();
        assert_eq!(
            paths,
            ["A.txt", "b.txt", "link.txt", "season 2/e9.mkv", "season 2/e10.mkv", "Season 10/e1.mkv"]
        );
        assert_eq!(c.files[2].size, 2);
        assert!(c.files[2].resolved_link);
        let mut skipped = c.skipped_links.clone();
        skipped.sort();
        assert_eq!(skipped, ["leak.txt", "loop"]);

        let single = collect_files(&root.join("b.txt"), &AtomicBool::new(false)).unwrap();
        assert!(!single.multi_file);
        assert_eq!(single.files.len(), 1);
        assert_eq!(single.files[0].path, ["b.txt"]);

        assert!(collect_files(Path::new("relative"), &AtomicBool::new(false)).is_err());
        assert!(collect_files(&dir.path().join("missing"), &AtomicBool::new(false)).is_err());
        assert_eq!(collect_files(&root, &AtomicBool::new(true)).unwrap_err(), ABORTED);
    }

    #[test]
    fn canonical_layout_pads_every_unaligned_file() {
        let files = vec![sf("b", 10), sf("a/x", 16384), sf("a/y", 0), sf("c", 5)];
        let l = canonical_layout(&files, 16384);
        let desc: Vec<(Option<usize>, u64)> = l.iter().map(|e| (e.file, e.size)).collect();
        // a/x (aligned) a/y (empty, no pad) b + pad c + tail pad
        assert_eq!(desc, [(Some(1), 16384), (Some(2), 0), (Some(0), 10), (None, 16374), (Some(3), 5), (None, 16379)]);
        assert_eq!(l[3].pad_path, [".pad", "16374"]);
        // Single file: never padded.
        let l = canonical_layout(&[sf("only", 10)], 16384);
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn libtorrent12_optimize() {
        let a = 16384u64;
        // Without padding only reordering happens: aligned slot takes a
        // multiple of the alignment first, else the largest file.
        let files = vec![sf("s1", 100), sf("big", 3 * a), sf("s2", 5000)];
        let l = optimized_v1_layout(&files, a, -1);
        // big (multiple of the alignment) first; the offset stays aligned so
        // the largest remaining file (s2) comes next.
        assert_eq!(l.iter().map(|e| e.file).collect::<Vec<_>>(), [Some(1), Some(2), Some(0)]);

        // Padding before files larger than the limit; a small filler is
        // preferred over a pad file.
        let files = vec![sf("x", 100), sf("large", 40000), sf("tiny", 50)];
        let l = optimized_v1_layout(&files, a, 1000);
        // offset 0: largest = large(40000); offset 40000: x (100 <= limit) placed;
        // offset 40100: tiny <= limit placed.
        assert_eq!(l.iter().map(|e| e.file).collect::<Vec<_>>(), [Some(1), Some(0), Some(2)]);

        let files = vec![sf("first", 100), sf("second", 30000), sf("third", 20000)];
        let l = optimized_v1_layout(&files, a, 0);
        // 0: largest (second 30000); 30000: third > 0 -> pad 2768, third moves to end... then first.
        let mut off = 0;
        for e in &l {
            if let Some(i) = e.file
                && files[i].size > 0
            {
                assert_eq!(off % a, 0, "{l:?}");
            }
            off += e.size;
        }
        assert!(l.iter().any(LayoutEntry::is_pad));
        assert_eq!(l.iter().filter(|e| !e.is_pad()).count(), 3);
    }
}
