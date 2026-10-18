//! Per-torrent on-disk layout: where each file of a torrent lives.
//!
//! A [`StorageLayout`] is shared by every storage instance a torrent goes through
//! (initializing, paused, live, after errors) and survives them, so it is the one
//! place that knows:
//!
//! * the output folder (changes on [`move_to`](StorageLayout::move_to));
//! * each file's user-visible relative path (changes on renames);
//! * each file's actual on-disk relative path, which adds the decorations: the
//!   incomplete-file suffix and the `.unwanted` folder;
//! * which files are complete and which are wanted, to compute the decorations.
//!
//! Every disk operation on a file (I/O, rename, move) holds that file's lock: I/O
//! takes it shared, path changes exclusively, so a path never changes under a read
//! or a write in flight.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File},
    io::{self, IoSlice},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use anyhow::{Context, bail};
use librqbit_core::lengths::{Lengths, ValidPieceIndex};
use parking_lot::{Mutex, RwLock, RwLockWriteGuard};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::{
    file_info::FileInfo,
    torrent_state::ManagedTorrent,
    type_aliases::{BF, BS},
};

use super::{
    error::write_error,
    filesystem::os,
    paths::{decorated_path, path_variants, validate_relative_path},
    pool::PooledFile,
    settings::{DiskCacheMode, StorageContext, StorageSettings},
};

/// A callback that moves a file to the trash instead of deleting it.
pub type TrashFn = dyn Fn(&Path) -> io::Result<()> + Send + Sync;
pub(crate) type TrashRef<'a> = &'a dyn Fn(&Path) -> io::Result<()>;

/// How [`move_storage`](crate::ManagedTorrent::move_storage) treats files that
/// already exist at the destination (libtorrent's `move_flags_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveMode {
    /// Move every file, replacing whatever is at the destination.
    AlwaysReplace,
    /// Fail without moving anything if any file already exists at the destination.
    FailIfExist,
    /// Keep (and use) files that already exist at the destination instead of moving
    /// ours over them; the torrent is rechecked afterwards. Our copies of those
    /// files stay where they were.
    #[default]
    DontReplace,
}

/// Progress of a running storage move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MoveProgress {
    pub destination: PathBuf,
    pub moved_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug)]
pub(crate) struct MoveState {
    destination: PathBuf,
    pub(crate) moved: AtomicU64,
    total: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MoveOutcome {
    /// False if source and destination were the same (nothing done).
    pub moved: bool,
    /// Some files were kept at the destination instead of moved: data must be rechecked.
    pub need_recheck: bool,
}

#[derive(Debug)]
pub(crate) struct FileSlot {
    user_path: PathBuf,
    // None when equal to user_path (the common case, saves memory).
    actual: Option<PathBuf>,
    len: u64,
    padding: bool,
    complete: bool,
    wanted: bool,
}

impl FileSlot {
    fn actual_path(&self) -> &Path {
        self.actual.as_deref().unwrap_or(&self.user_path)
    }

    fn set_actual(&mut self, p: PathBuf) {
        self.actual = if p == self.user_path { None } else { Some(p) };
    }

    fn desired(&self, settings: &StorageSettings) -> PathBuf {
        let suffix = settings.suffix().filter(|_| self.len > 0 && !self.complete);
        decorated_path(
            &self.user_path,
            suffix,
            settings.use_unwanted_folder && !self.wanted,
        )
    }
}

struct Completion {
    initialized: bool,
    have: BF,
    // Per file: how many of its pieces we don't have.
    remaining: Vec<u32>,
}

/// See the module documentation.
pub struct StorageLayout {
    uid: u64,
    ctx: Arc<StorageContext>,
    disk_backed: bool,
    owns_output_folder: bool,
    output_folder: RwLock<PathBuf>,
    files: Box<[RwLock<FileSlot>]>,
    // (start piece, end piece, file index) of non-empty, non-padding files, in
    // torrent order. Both starts and ends are non-decreasing.
    piece_map: Box<[(u32, u32, usize)]>,
    completion: Mutex<Completion>,
    unverified: Mutex<Option<BF>>,
    unverified_active: AtomicBool,
    op_lock: tokio::sync::Mutex<()>,
    busy: AtomicBool,
    move_state: Mutex<Option<Arc<MoveState>>>,
    owner: OnceLock<Weak<ManagedTorrent>>,
}

fn exists_no_follow(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

fn is_existing_file(p: &Path) -> bool {
    fs::metadata(p).is_ok_and(|m| m.is_file())
}

/// Rename `from` to `to` if `from` exists, creating parents of `to`.
/// Returns whether anything was renamed.
fn rename_if_exists(from: &Path, to: &Path) -> io::Result<bool> {
    if !exists_no_follow(from) {
        return Ok(false);
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(from, to)?;
    Ok(true)
}

/// Remove `root/rel_dir` and its parents up to (not including) `root` while they
/// are empty.
fn remove_empty_dirs_upwards(root: &Path, rel_dir: Option<&Path>) {
    let mut cur = rel_dir;
    while let Some(d) = cur {
        if d.as_os_str().is_empty() {
            break;
        }
        if fs::remove_dir(root.join(d)).is_err() {
            break;
        }
        cur = d.parent();
    }
}

fn is_junk_file_name(name: &str) -> bool {
    const JUNK: [&str; 4] = [".ds_store", "thumbs.db", "desktop.ini", ".directory"];
    let lower = name.to_lowercase();
    JUNK.contains(&lower.as_str()) || name.ends_with('~')
}

/// Remove a directory if it's empty or contains only junk files (`.DS_Store`,
/// `Thumbs.db`, `desktop.ini`, `.directory`, `*~`), like qBittorrent does.
fn remove_dir_if_empty_or_junk(dir: &Path) -> bool {
    let Ok(rd) = fs::read_dir(dir) else {
        return false;
    };
    let mut junk = Vec::new();
    for entry in rd {
        let Ok(entry) = entry else {
            return false;
        };
        let is_file = entry.file_type().is_ok_and(|t| t.is_file());
        match entry.file_name().to_str() {
            Some(name) if is_file && is_junk_file_name(name) => junk.push(entry.path()),
            _ => return false,
        }
    }
    for j in junk {
        if fs::remove_file(&j).is_err() {
            return false;
        }
    }
    fs::remove_dir(dir).is_ok()
}

#[cfg(test)]
thread_local! {
    // Makes renames in this thread fail as if crossing volumes, to test the copy path.
    static SIMULATE_CROSS_DEVICE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn rename(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(test)]
    if SIMULATE_CROSS_DEVICE.with(|c| c.get()) {
        return Err(io::Error::from(io::ErrorKind::CrossesDevices));
    }
    fs::rename(src, dst)
}

fn move_file(src: &Path, dst: &Path, len: u64, progress: &AtomicU64) -> io::Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    match rename(src, dst) {
        Ok(()) => {
            progress.fetch_add(len, Ordering::Relaxed);
            Ok(())
        }
        Err(e) if os::is_cross_device(&e) => {
            if let Err(e) = os::copy_file_with_progress(src, dst, progress) {
                let _ = fs::remove_file(dst);
                return Err(e);
            }
            fs::remove_file(src)
        }
        Err(e) => Err(e),
    }
}

impl StorageLayout {
    /// Create the layout of a torrent.
    ///
    /// `renamed` holds user path overrides by file index (validated here);
    /// `only_files` the wanted files (None = all).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        ctx: Arc<StorageContext>,
        disk_backed: bool,
        owns_output_folder: bool,
        output_folder: PathBuf,
        lengths: &Lengths,
        file_infos: &[FileInfo],
        renamed: Option<&BTreeMap<usize, PathBuf>>,
        only_files: Option<&[usize]>,
    ) -> anyhow::Result<Self> {
        let files: Vec<RwLock<FileSlot>> = file_infos
            .iter()
            .enumerate()
            .map(|(idx, fi)| {
                RwLock::new(FileSlot {
                    user_path: fi.relative_filename.clone(),
                    actual: None,
                    len: fi.len,
                    padding: fi.attrs.padding,
                    complete: fi.len == 0,
                    wanted: only_files.is_none_or(|o| o.contains(&idx)),
                })
            })
            .collect();
        let piece_map = file_infos
            .iter()
            .enumerate()
            .filter(|(_, fi)| !fi.attrs.padding && fi.piece_range.start < fi.piece_range.end)
            .map(|(idx, fi)| (fi.piece_range.start, fi.piece_range.end, idx))
            .collect();
        let layout = Self {
            uid: ctx.next_uid(),
            ctx,
            disk_backed,
            owns_output_folder,
            output_folder: RwLock::new(output_folder),
            files: files.into_boxed_slice(),
            piece_map,
            completion: Mutex::new(Completion {
                initialized: false,
                have: BF::from_boxed_slice(vec![0u8; lengths.piece_bitfield_bytes()].into()),
                remaining: Vec::new(),
            }),
            unverified: Mutex::new(None),
            unverified_active: AtomicBool::new(false),
            op_lock: tokio::sync::Mutex::new(()),
            busy: AtomicBool::new(false),
            move_state: Mutex::new(None),
            owner: OnceLock::new(),
        };
        if let Some(renamed) = renamed.filter(|r| !r.is_empty()) {
            let renames: Vec<(usize, PathBuf)> =
                renamed.iter().map(|(k, v)| (*k, v.clone())).collect();
            let validated = layout.validate_renames(&renames)?;
            for (idx, path) in validated {
                layout.files[idx].write().user_path = path;
            }
        }
        Ok(layout)
    }

    pub fn uid(&self) -> u64 {
        self.uid
    }

    /// The folder the torrent's files are stored under.
    pub fn output_folder(&self) -> PathBuf {
        self.output_folder.read().clone()
    }

    /// Whether the output folder belongs to the torrent (it's removed when it
    /// becomes empty after deleting or moving the files).
    pub fn owns_output_folder(&self) -> bool {
        self.owns_output_folder
    }

    pub fn is_disk_backed(&self) -> bool {
        self.disk_backed
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// User-visible relative paths of all files (renames applied, no decorations).
    pub fn user_paths(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .map(|f| f.read().user_path.clone())
            .collect()
    }

    /// Relative on-disk paths of all files (with suffix / `.unwanted` decorations).
    pub fn actual_paths(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .map(|f| f.read().actual_path().to_owned())
            .collect()
    }

    /// Absolute on-disk path of a file.
    pub fn full_actual_path(&self, idx: usize) -> Option<PathBuf> {
        let slot = self.files.get(idx)?.read();
        Some(self.output_folder.read().join(slot.actual_path()))
    }

    /// User path overrides compared to the torrent's own file names.
    pub(crate) fn renamed_files(&self, file_infos: &[FileInfo]) -> BTreeMap<usize, PathBuf> {
        self.files
            .iter()
            .zip(file_infos.iter())
            .enumerate()
            .filter_map(|(idx, (slot, fi))| {
                let slot = slot.read();
                (slot.user_path != fi.relative_filename).then(|| (idx, slot.user_path.clone()))
            })
            .collect()
    }

    pub(crate) fn settings(&self) -> Arc<StorageSettings> {
        self.ctx.settings()
    }

    pub(crate) fn set_owner(&self, owner: Weak<ManagedTorrent>) {
        let _ = self.owner.set(owner);
    }

    pub(crate) fn owner(&self) -> Option<Arc<ManagedTorrent>> {
        self.owner.get().and_then(|w| w.upgrade())
    }

    pub(crate) fn op_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.op_lock
    }

    /// Set while a storage operation keeps the torrent internally paused.
    pub(crate) fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    pub(crate) fn set_busy(&self, busy: bool) {
        self.busy.store(busy, Ordering::Release)
    }

    fn slot(&self, idx: usize) -> anyhow::Result<&RwLock<FileSlot>> {
        self.files.get(idx).context("no such file")
    }

    // ---------------------------------------------------------------------------
    // I/O
    // ---------------------------------------------------------------------------

    fn handle(&self, idx: usize, slot: &FileSlot, write: bool) -> io::Result<Arc<PooledFile>> {
        let key = (self.uid, idx);
        if let Some(h) = self.ctx.pool.get(key)
            && (h.writable || !write)
        {
            return Ok(h);
        }
        let path = self.output_folder.read().join(slot.actual_path());
        let settings = self.ctx.settings();
        let file = if write {
            os::open_read_write(&path, settings.write_cache)?
        } else {
            os::open_read(&path, settings.read_cache)?
        };
        let h = Arc::new(PooledFile {
            file,
            writable: write,
        });
        self.ctx.pool.insert(key, h.clone());
        Ok(h)
    }

    fn path_for_error(&self, slot: &FileSlot) -> PathBuf {
        self.output_folder.read().join(slot.actual_path())
    }

    pub(crate) fn read_at(&self, idx: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let slot = self.slot(idx)?.read();
        if slot.padding {
            buf.fill(0);
            return Ok(());
        }
        let r = self
            .handle(idx, &slot, false)
            .and_then(|h| os::read_exact_at(&h.file, offset, buf));
        r.with_context(|| {
            format!(
                "error reading {} bytes at {offset} from {:?}",
                buf.len(),
                self.path_for_error(&slot)
            )
        })
    }

    pub(crate) fn write_at(&self, idx: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        let slot = self.slot(idx)?.read();
        if slot.padding {
            return Ok(());
        }
        self.handle(idx, &slot, true)
            .and_then(|h| os::write_all_at(&h.file, offset, buf))
            .map_err(|e| write_error(e, &self.path_for_error(&slot)))
    }

    pub(crate) fn write_vectored_at(
        &self,
        idx: usize,
        offset: u64,
        bufs: [IoSlice<'_>; 2],
    ) -> anyhow::Result<usize> {
        let slot = self.slot(idx)?.read();
        if slot.padding {
            return Ok(bufs[0].len() + bufs[1].len());
        }
        self.handle(idx, &slot, true)
            .and_then(|h| os::write_vectored_at(&h.file, offset, bufs))
            .map_err(|e| write_error(e, &self.path_for_error(&slot)))
    }

    /// Make the file `len` bytes long (sparse, or preallocated when the option is on).
    pub(crate) fn ensure_len(&self, idx: usize, len: u64) -> anyhow::Result<()> {
        let slot = self.slot(idx)?.read();
        if slot.padding {
            return Ok(());
        }
        let preallocate = self.ctx.settings().preallocate;
        self.handle(idx, &slot, true)
            .and_then(|h| {
                if preallocate {
                    os::preallocate(&h.file, len)
                } else if h.file.metadata()?.len() != len {
                    h.file.set_len(len)
                } else {
                    Ok(())
                }
            })
            .map_err(|e| write_error(e, &self.path_for_error(&slot)))
    }

    /// Open a file for writing outside of the pool (for storages built on top).
    pub(crate) fn open_rw_unpooled(&self, idx: usize) -> anyhow::Result<File> {
        let slot = self.slot(idx)?.read();
        let path = self.path_for_error(&slot);
        os::open_read_write(&path, DiskCacheMode::Enabled)
            .with_context(|| format!("error opening {path:?}"))
    }

    /// Close all open handles of this torrent.
    pub(crate) fn close_handles(&self) {
        self.ctx.pool.remove_owner(self.uid);
    }

    // ---------------------------------------------------------------------------
    // Paths, completion, decorations
    // ---------------------------------------------------------------------------

    /// Point every file at existing data, looking at all decoration variants (for
    /// restarts with partially suffixed files, or options that changed meanwhile).
    /// Files with no data anywhere get their desired path.
    pub(crate) fn resolve_existing_paths(&self) {
        if !self.disk_backed {
            return;
        }
        let settings = self.ctx.settings();
        let root = self.output_folder();
        for (idx, slot) in self.files.iter().enumerate() {
            let mut slot = slot.write();
            if slot.padding || is_existing_file(&root.join(slot.actual_path())) {
                continue;
            }
            self.ctx.pool.remove((self.uid, idx));
            let found = path_variants(&slot.user_path, Some(settings.search_suffix()))
                .into_iter()
                .find(|v| is_existing_file(&root.join(v)));
            let actual = match found {
                Some(v) => v,
                None => slot.desired(&settings),
            };
            slot.set_actual(actual);
        }
    }

    /// Files that exist at their current path (non-padding).
    pub(crate) fn existing_files(&self) -> Vec<PathBuf> {
        if !self.disk_backed {
            return Vec::new();
        }
        let root = self.output_folder();
        self.files
            .iter()
            .filter_map(|s| {
                let s = s.read();
                let p = root.join(s.actual_path());
                (!s.padding && exists_no_follow(&p)).then_some(p)
            })
            .collect()
    }

    /// User paths of non-empty files for which `has_data` is true but that don't
    /// exist on disk.
    pub(crate) fn missing_files(&self, has_data: impl Fn(usize) -> bool) -> Vec<PathBuf> {
        if !self.disk_backed {
            return Vec::new();
        }
        let root = self.output_folder();
        self.files
            .iter()
            .enumerate()
            .filter_map(|(idx, s)| {
                let s = s.read();
                if s.padding || s.len == 0 || !has_data(idx) {
                    return None;
                }
                (!is_existing_file(&root.join(s.actual_path()))).then(|| s.user_path.clone())
            })
            .collect()
    }

    /// Whether a file has any of the given pieces.
    pub(crate) fn file_has_any_piece(&self, idx: usize, have: &BS) -> bool {
        self.piece_map
            .iter()
            .find(|(_, _, f)| *f == idx)
            .is_some_and(|(s, e, _)| have.get(*s as usize..*e as usize).is_some_and(|r| r.any()))
    }

    fn files_of_piece(&self, piece: u32) -> impl Iterator<Item = usize> + '_ {
        let start = self.piece_map.partition_point(|&(_, end, _)| end <= piece);
        self.piece_map[start..]
            .iter()
            .take_while(move |&&(s, _, _)| s <= piece)
            .filter(move |&&(s, e, _)| s <= piece && piece < e)
            .map(|&(_, _, f)| f)
    }

    /// Set which pieces we have (after a check). Updates per-file completion.
    pub(crate) fn set_completion(&self, have: &BS) {
        let mut remaining = vec![0u32; self.files.len()];
        for &(s, e, f) in self.piece_map.iter() {
            remaining[f] = have
                .get(s as usize..e as usize)
                .map(|r| r.count_zeros())
                .unwrap_or_default()
                .try_into()
                .unwrap_or(u32::MAX);
        }
        for (f, slot) in self.files.iter().enumerate() {
            let mut slot = slot.write();
            slot.complete = slot.padding || slot.len == 0 || remaining[f] == 0;
        }
        let mut c = self.completion.lock();
        c.have = have.to_bitvec().into_boxed_bitslice();
        c.remaining = remaining;
        c.initialized = true;
    }

    /// A piece was downloaded and verified. Files that became complete get their
    /// final name.
    pub(crate) fn on_piece_completed(&self, piece: ValidPieceIndex) {
        let newly_complete: Vec<usize> = {
            let mut c = self.completion.lock();
            let p = piece.get_usize();
            if !c.initialized || c.have.get(p).is_none_or(|b| *b) {
                return;
            }
            c.have.set(p, true);
            let mut v = Vec::new();
            for f in self.files_of_piece(piece.get()) {
                c.remaining[f] = c.remaining[f].saturating_sub(1);
                if c.remaining[f] == 0 {
                    v.push(f);
                }
            }
            v
        };

        let settings = self.ctx.settings();
        if settings.write_cache == DiskCacheMode::WriteThrough {
            for f in self.files_of_piece(piece.get()) {
                if let Some(h) = self.ctx.pool.get((self.uid, f))
                    && h.writable
                    && let Err(e) = h.file.sync_data()
                {
                    warn!(file = f, "error flushing file: {e:#}");
                }
            }
        }

        for f in newly_complete {
            let mut slot = self.files[f].write();
            slot.complete = true;
            if let Err(e) = self.reconcile_locked(f, &mut slot, &settings) {
                warn!(file = ?slot.user_path, "error renaming completed file: {e:#}");
            }
        }
    }

    /// A piece we had turned out to be bad. Files that become incomplete get their
    /// incomplete name back.
    pub(crate) fn on_piece_missing(&self, piece: ValidPieceIndex) {
        let newly_incomplete: Vec<usize> = {
            let mut c = self.completion.lock();
            let p = piece.get_usize();
            if !c.initialized || c.have.get(p).is_none_or(|b| !*b) {
                return;
            }
            c.have.set(p, false);
            let mut v = Vec::new();
            for f in self.files_of_piece(piece.get()) {
                c.remaining[f] += 1;
                if c.remaining[f] == 1 {
                    v.push(f);
                }
            }
            v
        };
        let settings = self.ctx.settings();
        for f in newly_incomplete {
            let mut slot = self.files[f].write();
            slot.complete = false;
            if let Err(e) = self.reconcile_locked(f, &mut slot, &settings) {
                warn!(file = ?slot.user_path, "error renaming incomplete file: {e:#}");
            }
        }
    }

    pub(crate) fn is_completion_initialized(&self) -> bool {
        self.completion.lock().initialized
    }

    /// Set the wanted files (priority > 0). Call [`reconcile_all`](Self::reconcile_all)
    /// afterwards to move files in and out of the unwanted folder.
    pub(crate) fn set_wanted(&self, wanted: impl Fn(usize) -> bool) {
        for (idx, slot) in self.files.iter().enumerate() {
            slot.write().wanted = wanted(idx);
        }
    }

    fn reconcile_locked(
        &self,
        idx: usize,
        slot: &mut FileSlot,
        settings: &StorageSettings,
    ) -> anyhow::Result<()> {
        if slot.padding {
            return Ok(());
        }
        let desired = slot.desired(settings);
        if desired == slot.actual_path() {
            return Ok(());
        }
        if self.disk_backed {
            let root = self.output_folder.read().clone();
            let from = root.join(slot.actual_path());
            let to = root.join(&desired);
            self.ctx.pool.remove((self.uid, idx));
            let renamed = rename_if_exists(&from, &to)
                .with_context(|| format!("error renaming {from:?} to {to:?}"))?;
            if renamed {
                debug!(?from, ?to, "renamed");
                remove_empty_dirs_upwards(&root, slot.actual_path().parent());
            }
        }
        slot.set_actual(desired);
        Ok(())
    }

    /// Bring every file's on-disk name in line with the options, completion and
    /// wanted state. Does nothing before the first check.
    pub(crate) fn reconcile_all(&self) {
        if !self.is_completion_initialized() {
            return;
        }
        let settings = self.ctx.settings();
        for (idx, slot) in self.files.iter().enumerate() {
            let mut slot = slot.write();
            if let Err(e) = self.reconcile_locked(idx, &mut slot, &settings) {
                warn!(file = ?slot.user_path, "{e:#}");
            }
        }
    }

    // ---------------------------------------------------------------------------
    // Renames
    // ---------------------------------------------------------------------------

    fn validate_renames(
        &self,
        renames: &[(usize, PathBuf)],
    ) -> anyhow::Result<Vec<(usize, PathBuf)>> {
        let mut validated = Vec::with_capacity(renames.len());
        let mut final_paths: Vec<Option<PathBuf>> = self
            .files
            .iter()
            .map(|s| {
                let s = s.read();
                (!s.padding).then(|| s.user_path.clone())
            })
            .collect();
        for (idx, path) in renames {
            let slot = self
                .files
                .get(*idx)
                .with_context(|| format!("no such file: {idx}"))?;
            if slot.read().padding {
                bail!("can't rename padding file {idx}");
            }
            let path = validate_relative_path(path)
                .with_context(|| format!("invalid path for file {idx}"))?;
            final_paths[*idx] = Some(path.clone());
            validated.push((*idx, path));
        }

        // Conflicts: two files with one path, or a file path that is also a folder
        // of another file. Compared case-sensitively, so that case-only renames work.
        let mut file_counts: HashMap<&Path, usize> = HashMap::new();
        let mut dirs: HashSet<&Path> = HashSet::new();
        for p in final_paths.iter().flatten() {
            *file_counts.entry(p.as_path()).or_default() += 1;
            for a in p.ancestors().skip(1) {
                if a.as_os_str().is_empty() {
                    break;
                }
                dirs.insert(a);
            }
        }
        for (_, p) in &validated {
            if file_counts.get(p.as_path()).copied().unwrap_or_default() > 1 {
                bail!("the file already exists: {p:?}");
            }
            if dirs.contains(p.as_path()) {
                bail!("a folder with this name already exists: {p:?}");
            }
            if let Some(a) = p
                .ancestors()
                .skip(1)
                .take_while(|a| !a.as_os_str().is_empty())
                .find(|a| file_counts.contains_key(a))
            {
                bail!("{a:?} is a file, can't be a folder of {p:?}");
            }
        }
        Ok(validated)
    }

    /// Rename files (user-visible paths) and their data on disk.
    ///
    /// All new paths are validated first (relative, no `..`, no empty components,
    /// no conflicts with other files). If a file's data exists on disk it's renamed
    /// right away, and the target must not exist (unless it's the same file, e.g.
    /// a case-only change). Missing files just get their new path recorded.
    pub(crate) fn rename_files(&self, renames: &[(usize, PathBuf)]) -> anyhow::Result<()> {
        let validated = self.validate_renames(renames)?;
        let settings = self.ctx.settings();
        for (idx, new_user) in validated {
            let mut slot = self.files[idx].write();
            if slot.user_path == new_user {
                continue;
            }
            let old_actual = slot.actual_path().to_owned();
            let old_user = std::mem::replace(&mut slot.user_path, new_user);
            let desired = slot.desired(&settings);
            if self.disk_backed {
                let root = self.output_folder.read().clone();
                let from = root.join(&old_actual);
                let to = root.join(&desired);
                if exists_no_follow(&to) && !os::same_file(&from, &to) {
                    slot.user_path = old_user;
                    bail!("the file already exists on disk: {to:?}");
                }
                self.ctx.pool.remove((self.uid, idx));
                match rename_if_exists(&from, &to) {
                    Ok(true) => remove_empty_dirs_upwards(&root, old_actual.parent()),
                    Ok(false) => {}
                    Err(e) => {
                        slot.user_path = old_user;
                        return Err(e)
                            .with_context(|| format!("error renaming {from:?} to {to:?}"));
                    }
                }
            }
            slot.set_actual(desired);
        }
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // Move
    // ---------------------------------------------------------------------------

    pub(crate) fn begin_move(&self, destination: PathBuf) -> Arc<MoveState> {
        let state = Arc::new(MoveState {
            destination,
            moved: AtomicU64::new(0),
            total: AtomicU64::new(0),
        });
        *self.move_state.lock() = Some(state.clone());
        state
    }

    pub(crate) fn end_move(&self) {
        *self.move_state.lock() = None;
    }

    /// Progress of the running move, if any.
    pub fn move_progress(&self) -> Option<MoveProgress> {
        self.move_state.lock().as_ref().map(|s| MoveProgress {
            destination: s.destination.clone(),
            moved_bytes: s.moved.load(Ordering::Relaxed),
            total_bytes: s.total.load(Ordering::Relaxed),
        })
    }

    /// Move all files to `new_root`. Must be called with the torrent's I/O stopped.
    ///
    /// Same-volume moves rename files, cross-volume moves copy, fsync and delete
    /// them. On failure, files moved so far are moved back and the output folder
    /// stays unchanged. Empty source folders are removed afterwards.
    pub(crate) fn move_to(
        &self,
        new_root: &Path,
        mode: MoveMode,
        state: &MoveState,
    ) -> anyhow::Result<MoveOutcome> {
        let mut slots: Vec<RwLockWriteGuard<'_, FileSlot>> =
            self.files.iter().map(|s| s.write()).collect();
        let old_root = self.output_folder();
        if old_root == new_root || os::same_file(&old_root, new_root) {
            return Ok(MoveOutcome::default());
        }
        self.ctx.pool.remove_owner(self.uid);
        if !self.disk_backed {
            *self.output_folder.write() = new_root.to_owned();
            return Ok(MoveOutcome {
                moved: true,
                need_recheck: false,
            });
        }

        struct Plan {
            src: PathBuf,
            dst: PathBuf,
            rel: PathBuf,
            len: u64,
        }

        let settings = self.ctx.settings();
        let mut plan = Vec::new();
        let mut keep_at_destination: Vec<(usize, PathBuf)> = Vec::new();
        let mut total = 0u64;
        for (idx, slot) in slots.iter().enumerate() {
            if slot.padding {
                continue;
            }
            let rel = slot.actual_path();
            let src = old_root.join(rel);
            let src_len = fs::symlink_metadata(&src)
                .ok()
                .filter(|m| m.is_file())
                .map(|m| m.len());
            let existing = path_variants(&slot.user_path, Some(settings.search_suffix()))
                .into_iter()
                .chain(std::iter::once(rel.to_owned()))
                .find(|v| exists_no_follow(&new_root.join(v)));
            match (mode, existing) {
                (MoveMode::FailIfExist, Some(v)) => {
                    bail!(
                        "file already exists at the destination: {:?}",
                        new_root.join(v)
                    );
                }
                (MoveMode::DontReplace, Some(v)) => {
                    keep_at_destination.push((idx, v));
                    continue;
                }
                _ => {}
            }
            if let Some(len) = src_len {
                total += len;
                plan.push(Plan {
                    src,
                    dst: new_root.join(rel),
                    rel: rel.to_owned(),
                    len,
                });
            }
        }
        state.total.store(total, Ordering::Relaxed);

        fs::create_dir_all(new_root).with_context(|| format!("error creating {new_root:?}"))?;

        for (i, p) in plan.iter().enumerate() {
            if let Err(e) = move_file(&p.src, &p.dst, p.len, &state.moved) {
                let err = anyhow::Error::new(e)
                    .context(format!("error moving {:?} to {:?}", p.src, p.dst));
                warn!("{err:#}; moving files back");
                let ignored = AtomicU64::new(0);
                for done in plan[..i].iter().rev() {
                    if let Err(e) = move_file(&done.dst, &done.src, done.len, &ignored) {
                        warn!(file = ?done.dst, "error moving file back: {e:#}");
                    } else {
                        remove_empty_dirs_upwards(new_root, done.rel.parent());
                    }
                }
                return Err(err);
            }
        }

        *self.output_folder.write() = new_root.to_owned();
        let need_recheck = !keep_at_destination.is_empty();
        for (idx, v) in keep_at_destination {
            slots[idx].set_actual(v);
        }
        drop(slots);

        let mut dirs: Vec<&Path> = plan
            .iter()
            .filter_map(|p| p.rel.parent())
            .filter(|d| !d.as_os_str().is_empty())
            .collect();
        dirs.sort_unstable_by_key(|d| std::cmp::Reverse(d.components().count()));
        dirs.dedup();
        for d in dirs {
            remove_empty_dirs_upwards(&old_root, Some(d));
        }
        if self.owns_output_folder {
            remove_dir_if_empty_or_junk(&old_root);
        }
        Ok(MoveOutcome {
            moved: true,
            need_recheck,
        })
    }

    // ---------------------------------------------------------------------------
    // Delete
    // ---------------------------------------------------------------------------

    /// Delete (or move to trash) this torrent's files, then remove the folders of
    /// the torrent that are left empty (only junk files inside), and the output
    /// folder itself if the torrent owns it. Nothing outside the torrent's own
    /// relative paths is touched. Returns the errors encountered.
    pub(crate) fn delete_files(&self, trash: Option<TrashRef<'_>>) -> Vec<anyhow::Error> {
        let mut errors = Vec::new();
        if !self.disk_backed {
            return errors;
        }
        self.ctx.pool.remove_owner(self.uid);
        let root = self.output_folder();
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        for slot in self.files.iter() {
            let slot = slot.read();
            if slot.padding {
                continue;
            }
            for p in [slot.actual_path(), slot.user_path.as_path()] {
                for a in p.ancestors().skip(1) {
                    if a.as_os_str().is_empty() {
                        break;
                    }
                    dirs.insert(a.to_owned());
                }
            }
            let full = root.join(slot.actual_path());
            if !exists_no_follow(&full) {
                continue;
            }
            let r = match trash {
                Some(trash) => trash(&full),
                None => fs::remove_file(&full),
            };
            match r {
                Ok(()) => debug!(file = ?full, "deleted"),
                Err(e) => {
                    errors.push(anyhow::Error::new(e).context(format!("error deleting {full:?}")))
                }
            }
        }
        let mut dirs: Vec<PathBuf> = dirs.into_iter().collect();
        dirs.sort_unstable_by_key(|d| std::cmp::Reverse(d.components().count()));
        for d in dirs {
            remove_dir_if_empty_or_junk(&root.join(d));
        }
        if self.owns_output_folder {
            remove_dir_if_empty_or_junk(&root);
        }
        errors
    }

    // ---------------------------------------------------------------------------
    // Skip hash check ("seed mode"): pieces assumed present until first read.
    // ---------------------------------------------------------------------------

    pub(crate) fn set_unverified(&self, pieces: Option<BF>) {
        let mut g = self.unverified.lock();
        let active = pieces.as_ref().is_some_and(|p| p.any());
        *g = pieces.filter(|_| active);
        self.unverified_active.store(active, Ordering::Release);
    }

    /// Whether the piece is assumed present but was never verified (added with
    /// skip hash check).
    pub fn is_piece_unverified(&self, piece: ValidPieceIndex) -> bool {
        if !self.unverified_active.load(Ordering::Acquire) {
            return false;
        }
        self.unverified
            .lock()
            .as_ref()
            .and_then(|b| b.get(piece.get_usize()).map(|b| *b))
            .unwrap_or(false)
    }

    pub fn unverified_piece_count(&self) -> usize {
        self.unverified
            .lock()
            .as_ref()
            .map(|b| b.count_ones())
            .unwrap_or_default()
    }

    pub(crate) fn mark_piece_verified(&self, piece: ValidPieceIndex) {
        let mut g = self.unverified.lock();
        if let Some(b) = g.as_mut() {
            b.set(piece.get_usize(), false);
            if b.not_any() {
                *g = None;
                self.unverified_active.store(false, Ordering::Release);
            }
        }
    }

    /// An unverified piece failed its hash check when it was first read: forget
    /// it and let the torrent mark it as missing.
    pub(crate) fn on_unverified_piece_failed(&self, piece: ValidPieceIndex) {
        self.mark_piece_verified(piece);
        match self.owner() {
            Some(t) => t.on_piece_verification_failed(piece),
            None => warn!(?piece, "piece failed verification, but the torrent is gone"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
        sync::{Arc, atomic::Ordering},
    };

    use librqbit_core::{lengths::Lengths, torrent_metainfo::FileDetailsAttrs};

    use super::{MoveMode, StorageLayout};
    use crate::{
        file_info::FileInfo,
        storage::settings::{StorageContext, StorageSettings},
        type_aliases::BF,
    };

    const PIECE: u32 = 16;

    // Files laid out back to back; returns infos and lengths.
    fn infos(files: &[(&str, u64)]) -> (Vec<FileInfo>, Lengths) {
        let total: u64 = files.iter().map(|f| f.1).sum();
        let lengths = Lengths::new(total, PIECE).unwrap();
        let mut offset = 0;
        let v = files
            .iter()
            .map(|(name, len)| {
                let fi = FileInfo {
                    relative_filename: PathBuf::from(name),
                    offset_in_torrent: offset,
                    piece_range: lengths.iter_pieces_within_offset(offset, *len),
                    attrs: FileDetailsAttrs::default(),
                    len: *len,
                };
                offset += len;
                fi
            })
            .collect();
        (v, lengths)
    }

    fn ctx(settings: StorageSettings) -> Arc<StorageContext> {
        StorageContext::new(settings)
    }

    fn layout(
        ctx: &Arc<StorageContext>,
        root: &Path,
        files: &[(&str, u64)],
        renamed: Option<&BTreeMap<usize, PathBuf>>,
        only_files: Option<&[usize]>,
    ) -> (StorageLayout, Vec<FileInfo>, Lengths) {
        let (fi, lengths) = infos(files);
        let l = StorageLayout::new(
            ctx.clone(),
            true,
            true,
            root.to_owned(),
            &lengths,
            &fi,
            renamed,
            only_files,
        )
        .unwrap();
        (l, fi, lengths)
    }

    fn bits(n: usize, ones: &[usize]) -> BF {
        let mut b = BF::from_boxed_slice(vec![0u8; n.div_ceil(8)].into());
        for o in ones {
            b.set(*o, true);
        }
        b
    }

    fn write_file(l: &StorageLayout, idx: usize, len: usize) {
        l.write_at(idx, 0, &vec![7u8; len]).unwrap();
    }

    #[test]
    fn test_rename_mapping_and_validation() {
        let dir = tempfile::TempDir::with_prefix("layout_rename").unwrap();
        let c = ctx(Default::default());
        let (l, fi, _) = layout(
            &c,
            dir.path(),
            &[("a/x", 10), ("a/y", 10), ("b", 10)],
            None,
            None,
        );
        write_file(&l, 0, 10);

        l.rename_files(&[(0, "c/d/x2".into())]).unwrap();
        assert_eq!(l.user_paths()[0], PathBuf::from("c/d/x2"));
        assert!(dir.path().join("c/d/x2").exists());
        assert!(!dir.path().join("a/x").exists());
        assert_eq!(
            l.renamed_files(&fi),
            BTreeMap::from([(0, PathBuf::from("c/d/x2"))])
        );

        // A file that has no data yet just gets the new path.
        l.rename_files(&[(1, "y-renamed".into())]).unwrap();
        assert_eq!(l.actual_paths()[1], PathBuf::from("y-renamed"));
        // Empty leftover folder "a" was removed.
        assert!(!dir.path().join("a").exists());

        // Traversal, absolute, empty components.
        for bad in ["../evil", "/etc/passwd", "a//b", "a/./b", "", "x/.."] {
            assert!(l.rename_files(&[(2, bad.into())]).is_err(), "{bad}");
        }
        // Conflicts with other files.
        assert!(l.rename_files(&[(2, "y-renamed".into())]).is_err());
        assert!(l.rename_files(&[(2, "c/d".into())]).is_err());
        assert!(l.rename_files(&[(2, "c/d/x2/z".into())]).is_err());
        assert!(l.rename_files(&[(7, "z".into())]).is_err());
        // Swapping two names in one batch is a conflict-free final state, but the
        // on-disk target exists in the middle: the first rename must not clobber it.
        write_file(&l, 1, 10);
        assert!(
            l.rename_files(&[(0, "y-renamed".into()), (1, "c/d/x2".into())])
                .is_err()
        );
        assert!(dir.path().join("y-renamed").exists());
        assert!(dir.path().join("c/d/x2").exists());

        // Renames given at creation are validated too.
        let bad = BTreeMap::from([(0usize, PathBuf::from("../x"))]);
        let (fi2, lengths) = infos(&[("a", 1)]);
        assert!(
            StorageLayout::new(
                c.clone(),
                true,
                true,
                dir.path().to_owned(),
                &lengths,
                &fi2,
                Some(&bad),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn test_suffix_lifecycle() {
        let dir = tempfile::TempDir::with_prefix("layout_suffix").unwrap();
        let c = ctx(StorageSettings {
            incomplete_suffix: Some(".!qB".into()),
            ..Default::default()
        });
        // 3 pieces: file0 = piece 0 + half of piece 1, file1 = other half + piece 2.
        let (l, _, _) = layout(
            &c,
            dir.path(),
            &[("f0", 24), ("f1", 24), ("empty", 0)],
            None,
            None,
        );
        l.resolve_existing_paths();
        assert_eq!(
            l.actual_paths(),
            vec![
                PathBuf::from("f0.!qB"),
                PathBuf::from("f1.!qB"),
                PathBuf::from("empty")
            ]
        );
        write_file(&l, 0, 24);
        write_file(&l, 1, 24);
        assert!(dir.path().join("f0.!qB").exists());

        l.set_completion(&bits(3, &[]));
        l.reconcile_all();
        assert!(dir.path().join("f0.!qB").exists());

        l.on_piece_completed(c_piece(0));
        assert!(dir.path().join("f0.!qB").exists());
        l.on_piece_completed(c_piece(1));
        assert!(dir.path().join("f0").exists());
        assert!(!dir.path().join("f0.!qB").exists());
        assert!(dir.path().join("f1.!qB").exists());
        // Data still reachable after the rename.
        let mut buf = [0u8; 4];
        l.read_at(0, 0, &mut buf).unwrap();
        assert_eq!(buf, [7; 4]);
        l.on_piece_completed(c_piece(2));
        assert!(dir.path().join("f1").exists());

        // A piece going bad makes the file incomplete again.
        l.on_piece_missing(c_piece(2));
        assert!(dir.path().join("f1.!qB").exists());

        // Turning the option off renames everything to final names.
        c.set_settings(StorageSettings::default());
        l.reconcile_all();
        assert_eq!(l.actual_paths()[1], PathBuf::from("f1"));
        assert!(dir.path().join("f1").exists());
    }

    fn c_piece(i: u32) -> librqbit_core::lengths::ValidPieceIndex {
        Lengths::new(48, PIECE)
            .unwrap()
            .validate_piece_index(i)
            .unwrap()
    }

    #[test]
    fn test_restart_finds_partially_suffixed_files() {
        let dir = tempfile::TempDir::with_prefix("layout_restart").unwrap();
        std::fs::write(dir.path().join("f0"), vec![1u8; 24]).unwrap();
        std::fs::write(dir.path().join("f1.!qB"), vec![1u8; 24]).unwrap();
        std::fs::create_dir_all(dir.path().join(".unwanted")).unwrap();
        std::fs::write(dir.path().join(".unwanted/f2"), vec![1u8; 16]).unwrap();
        // Option now off, files left in the state of a previous run.
        let c = ctx(Default::default());
        let (l, _, _) = layout(
            &c,
            dir.path(),
            &[("f0", 24), ("f1", 24), ("f2", 16)],
            None,
            None,
        );
        l.resolve_existing_paths();
        assert_eq!(
            l.actual_paths(),
            vec![
                PathBuf::from("f0"),
                PathBuf::from("f1.!qB"),
                PathBuf::from(".unwanted/f2")
            ]
        );
        l.set_completion(&bits(4, &[0, 1, 2]));
        l.reconcile_all();
        assert_eq!(
            l.actual_paths(),
            vec![
                PathBuf::from("f0"),
                PathBuf::from("f1"),
                PathBuf::from("f2")
            ]
        );
        assert!(dir.path().join("f1").exists());
        assert!(dir.path().join("f2").exists());
        assert!(!dir.path().join(".unwanted").exists());
    }

    #[test]
    fn test_unwanted_folder() {
        let dir = tempfile::TempDir::with_prefix("layout_unwanted").unwrap();
        let c = ctx(StorageSettings {
            use_unwanted_folder: true,
            ..Default::default()
        });
        let (l, _, _) = layout(
            &c,
            dir.path(),
            &[("d/a", 16), ("d/b", 16)],
            None,
            Some(&[0]),
        );
        l.resolve_existing_paths();
        assert_eq!(
            l.actual_paths(),
            vec![PathBuf::from("d/a"), PathBuf::from("d/.unwanted/b")]
        );
        write_file(&l, 1, 16);
        assert!(dir.path().join("d/.unwanted/b").exists());
        l.set_completion(&bits(2, &[1]));
        // Wanted again: moves back, empty .unwanted removed.
        l.set_wanted(|_| true);
        l.reconcile_all();
        assert!(dir.path().join("d/b").exists());
        assert!(!dir.path().join("d/.unwanted").exists());
        // Unwanted again.
        l.set_wanted(|i| i == 0);
        l.reconcile_all();
        assert!(dir.path().join("d/.unwanted/b").exists());
        assert_eq!(l.user_paths()[1], PathBuf::from("d/b"));
    }

    #[test]
    fn test_move_across_volumes_copies_with_progress() {
        let tmp = tempfile::TempDir::with_prefix("layout_move_copy").unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        let c = ctx(Default::default());
        let (l, _, _) = layout(&c, &src, &[("sub/a", 3000), ("b", 5000)], None, None);
        write_file(&l, 0, 3000);
        write_file(&l, 1, 5000);
        super::SIMULATE_CROSS_DEVICE.with(|f| f.set(true));
        let st = l.begin_move(dst.clone());
        let out = l.move_to(&dst, MoveMode::FailIfExist, &st);
        super::SIMULATE_CROSS_DEVICE.with(|f| f.set(false));
        let out = out.unwrap();
        assert!(out.moved && !out.need_recheck);
        let progress = l.move_progress().unwrap();
        assert_eq!(progress.total_bytes, 8000);
        assert_eq!(progress.moved_bytes, 8000);
        assert_eq!(std::fs::read(dst.join("sub/a")).unwrap(), vec![7u8; 3000]);
        assert_eq!(std::fs::read(dst.join("b")).unwrap(), vec![7u8; 5000]);
        // Sources deleted, owned source folder cleaned up.
        assert!(!src.exists());
    }

    #[test]
    fn test_move_failure_moves_files_back() {
        let tmp = tempfile::TempDir::with_prefix("layout_move_fail").unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        let c = ctx(Default::default());
        let (l, _, _) = layout(&c, &src, &[("a", 16), ("d/b", 16)], None, None);
        write_file(&l, 0, 16);
        write_file(&l, 1, 16);
        // A file where the second file's folder needs to be makes its move fail.
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(dst.join("d"), b"in the way").unwrap();
        let st = l.begin_move(dst.clone());
        assert!(l.move_to(&dst, MoveMode::AlwaysReplace, &st).is_err());
        assert_eq!(l.output_folder(), src);
        assert!(src.join("a").exists());
        assert!(src.join("d/b").exists());
        assert!(!dst.join("a").exists());
        let mut buf = [0u8; 4];
        l.read_at(0, 0, &mut buf).unwrap();
        assert_eq!(buf, [7; 4]);
    }

    #[test]
    fn test_move_modes_and_cleanup() {
        let tmp = tempfile::TempDir::with_prefix("layout_move").unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        let c = ctx(Default::default());
        let (l, _, _) = layout(&c, &src, &[("sub/a", 16), ("b", 16)], None, None);
        write_file(&l, 0, 16);
        write_file(&l, 1, 16);
        std::fs::write(src.join("unrelated"), b"keep").unwrap();

        // Fail if exist.
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(dst.join("b"), b"existing").unwrap();
        let st = l.begin_move(dst.clone());
        assert!(l.move_to(&dst, MoveMode::FailIfExist, &st).is_err());
        assert!(src.join("sub/a").exists());
        assert_eq!(l.output_folder(), src);

        // Don't replace: existing "b" is kept and used, ours stays put.
        let out = l.move_to(&dst, MoveMode::DontReplace, &st).unwrap();
        assert!(out.moved && out.need_recheck);
        assert_eq!(l.output_folder(), dst);
        assert!(dst.join("sub/a").exists());
        assert_eq!(std::fs::read(dst.join("b")).unwrap(), b"existing");
        assert!(src.join("b").exists());
        assert!(!src.join("sub").exists());
        assert_eq!(st.moved.load(Ordering::Relaxed), 16);
        // The unrelated file and the (not empty) source folder stay.
        assert!(src.join("unrelated").exists());
        l.end_move();
        assert!(l.move_progress().is_none());

        // Always replace, back to a new place; data must follow.
        let dst2 = tmp.path().join("dst2");
        std::fs::create_dir_all(&dst2).unwrap();
        std::fs::write(dst2.join("b"), b"old").unwrap();
        let st = l.begin_move(dst2.clone());
        assert!(l.move_progress().is_some());
        let out = l.move_to(&dst2, MoveMode::AlwaysReplace, &st).unwrap();
        assert!(out.moved && !out.need_recheck);
        assert_eq!(std::fs::read(dst2.join("b")).unwrap(), b"existing");
        let mut buf = [0u8; 4];
        l.read_at(0, 0, &mut buf).unwrap();
        assert_eq!(buf, [7; 4]);
        // Owned output folder left empty is removed.
        assert!(!dst.exists());

        // Moving to the same place is a no-op.
        let out = l.move_to(&dst2, MoveMode::AlwaysReplace, &st).unwrap();
        assert!(!out.moved);
    }

    #[test]
    fn test_delete_only_own_files() {
        let tmp = tempfile::TempDir::with_prefix("layout_delete").unwrap();
        let root = tmp.path().join("t");
        let c = ctx(StorageSettings {
            incomplete_suffix: Some(".!qB".into()),
            ..Default::default()
        });
        let (l, _, _) = layout(
            &c,
            &root,
            &[("d/a", 16), ("d/e/b", 16), ("c", 16)],
            None,
            None,
        );
        l.resolve_existing_paths();
        write_file(&l, 0, 16);
        write_file(&l, 1, 16);
        std::fs::write(root.join("d/.DS_Store"), b"junk").unwrap();
        std::fs::write(root.join("unrelated"), b"keep").unwrap();
        std::fs::write(root.join("d/a"), b"same name, not ours (.!qB is)").unwrap();

        let trashed = std::sync::Mutex::new(Vec::new());
        let trash = |p: &Path| {
            trashed.lock().unwrap().push(p.to_owned());
            std::fs::remove_file(p)
        };
        let errors = l.delete_files(Some(&trash));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(trashed.lock().unwrap().len(), 2);
        assert!(!root.join("d/a.!qB").exists());
        assert!(!root.join("d/e").exists());
        // Not ours: kept, and so is the folder holding it.
        assert!(root.join("d/a").exists());
        assert!(root.join("unrelated").exists());
        assert!(root.exists());

        std::fs::remove_file(root.join("d/a")).unwrap();
        std::fs::remove_file(root.join("unrelated")).unwrap();
        l.delete_files(None);
        // Only junk left: removed with the owned root.
        assert!(!root.exists());
    }

    #[test]
    fn test_missing_files_and_seed_mode_bits() {
        let dir = tempfile::TempDir::with_prefix("layout_missing").unwrap();
        let c = ctx(Default::default());
        let (l, _, _) = layout(
            &c,
            dir.path(),
            &[("a", 16), ("b", 16), ("z", 0)],
            None,
            None,
        );
        write_file(&l, 0, 16);
        let have = bits(2, &[0, 1]);
        assert!(l.file_has_any_piece(1, &have));
        assert!(!l.file_has_any_piece(2, &have));
        assert_eq!(
            l.missing_files(|i| l.file_has_any_piece(i, &have)),
            vec![PathBuf::from("b")]
        );
        assert_eq!(l.existing_files().len(), 1);

        l.set_unverified(Some(bits(2, &[0, 1])));
        let p0 = Lengths::new(32, PIECE)
            .unwrap()
            .validate_piece_index(0)
            .unwrap();
        let p1 = Lengths::new(32, PIECE)
            .unwrap()
            .validate_piece_index(1)
            .unwrap();
        assert!(l.is_piece_unverified(p0));
        l.mark_piece_verified(p0);
        assert!(!l.is_piece_unverified(p0));
        assert_eq!(l.unverified_piece_count(), 1);
        l.mark_piece_verified(p1);
        assert_eq!(l.unverified_piece_count(), 0);
        assert!(!l.is_piece_unverified(p1));
    }
}
