//! .torrent file backups (qBittorrent 5.3, `SessionImpl::backupTorrentFile`
//! and `handleTorrentFinished`): a copy of each added torrent's .torrent file
//! in the backup folder, optionally moved to another folder when the torrent
//! finishes and deleted when it is removed.
//!
//! Where each torrent's backup is lives in its resume record
//! ([`TorrentEntry::torrent_file_backup`](super::torrent::TorrentEntry)):
//! the path as configured (relative paths stay relative to the profile's
//! data folder, as upstream stores them), or [`MAGNET_PENDING`] for a magnet
//! whose backup is written once its metadata is known.
//!
//! Files are named after the torrent (`<name>.torrent`, then
//! `<name> (N).torrent`); the name is sanitised so the file is always created
//! directly inside the configured folder, and never replaces an existing file.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::paths;
use super::resume;
use super::session::Session;
use crate::prefs::DEFAULT_TORRENT_BACKUP_DIR;

/// Registry value of a magnet torrent whose backup waits for its metadata.
pub const MAGNET_PENDING: &str = "magnet:";

/// Longest file name stem (bytes); file systems allow 255 bytes per name.
const MAX_STEM_BYTES: usize = 200;

/// Most `<name> (N).torrent` candidates tried.
const MAX_UNIQUE_ATTEMPTS: u32 = 10_000;

/// Whether a registry value is a pending magnet backup.
pub fn is_magnet(registry: &str) -> bool {
    registry.len() >= MAGNET_PENDING.len() && registry[..MAGNET_PENDING.len()].eq_ignore_ascii_case(MAGNET_PENDING)
}

/// A configured folder as an absolute path: `~` expanded, an empty value is
/// the default `backup`, relative paths are inside `data_dir`
/// (`resolvePath(path, SpecialFolder::Data)`).
pub fn resolve(conf: &str, data_dir: &Path) -> PathBuf {
    let conf = conf.trim();
    let conf = if conf.is_empty() { DEFAULT_TORRENT_BACKUP_DIR } else { conf };
    let expanded = paths::expand_home(conf);
    let joined = if expanded.starts_with('/') {
        expanded
    } else {
        format!("{}/{expanded}", data_dir.to_string_lossy().trim_end_matches('/'))
    };
    PathBuf::from(paths::clean(&joined))
}

/// The registry value for a backup written as `file` into the folder
/// configured as `dir_conf`: the absolute path for an absolute folder,
/// `<dir_conf>/<file name>` for a relative one.
pub fn registry_value(dir_conf: &str, file: &Path) -> String {
    let conf = dir_conf.trim();
    let conf = if conf.is_empty() { DEFAULT_TORRENT_BACKUP_DIR } else { conf };
    let expanded = paths::expand_home(conf);
    if expanded.starts_with('/') {
        return file.to_string_lossy().into_owned();
    }
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    format!("{}/{name}", conf.trim_end_matches('/'))
}

/// The file name stem for a torrent: [`paths::to_valid_file_name`] (no
/// separators or control characters, never `.`/`..`), cut to
/// [`MAX_STEM_BYTES`] on a character boundary.
pub fn file_stem(torrent_name: &str) -> String {
    let mut s = paths::to_valid_file_name(torrent_name);
    if s.len() > MAX_STEM_BYTES {
        let mut cut = MAX_STEM_BYTES;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s = s.trim_end().to_owned();
        if s.is_empty() {
            s = "torrent".into();
        }
    }
    s
}

/// Writes `bytes` to a new file `<stem>.torrent` (or `<stem> (N).torrent`)
/// directly inside `dir`, creating the folder. Never overwrites or follows
/// an existing entry (`O_EXCL`).
pub fn write_unique(dir: &Path, stem: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(dir)?;
    for n in 0..MAX_UNIQUE_ATTEMPTS {
        let name = if n == 0 { format!("{stem}.torrent") } else { format!("{stem} ({n}).torrent") };
        let path = dir.join(&name);
        // The stem has no separators; refuse anything that would still
        // leave the folder.
        if path.parent() != Some(dir) || path.file_name().map(|f| f.to_string_lossy() != name).unwrap_or(true) {
            return Err(std::io::Error::other("invalid file name"));
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(&path) {
            Ok(mut f) => {
                let written = f.write_all(bytes).and_then(|()| f.sync_all());
                if let Err(e) = written {
                    let _ = std::fs::remove_file(&path);
                    return Err(e);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("too many files with the same name"))
}

/// Deletes a backup named by a registry value. Only a regular `.torrent`
/// file is removed (never a folder or a symbolic link's target).
pub fn remove(registry: &str, data_dir: &Path) -> Result<PathBuf, (PathBuf, std::io::Error)> {
    let path = resolve(registry, data_dir);
    let check = || -> std::io::Result<()> {
        let is_torrent = path.extension().is_some_and(|x| x.eq_ignore_ascii_case("torrent"));
        let meta = std::fs::symlink_metadata(&path)?;
        if !is_torrent || !meta.is_file() {
            return Err(std::io::Error::other("not a .torrent file"));
        }
        std::fs::remove_file(&path)
    };
    match check() {
        Ok(()) => Ok(path),
        Err(e) => Err((path, e)),
    }
}

/// Moves a backup into `dir` under a new unique name. The file is copied
/// with [`write_unique`] (works across volumes, never overwrites), then the
/// original is deleted. Returns the new path.
pub fn move_into(old: &Path, dir: &Path, stem: &str, limit: u64) -> std::io::Result<PathBuf> {
    let bytes = resume::read_limited_no_follow(old, limit)?;
    let new = write_unique(dir, stem, &bytes)?;
    if let Err(e) = std::fs::remove_file(old) {
        let _ = std::fs::remove_file(&new);
        return Err(e);
    }
    Ok(new)
}

impl Session {
    fn backup_registry(&self, id: &str) -> Option<String> {
        self.state.read().torrents.get(id).map(|e| e.torrent_file_backup.clone())
    }

    fn set_backup_registry(&self, id: &str, value: String) {
        if let Some(e) = self.state.write().torrents.get_mut(id)
            && e.torrent_file_backup != value
        {
            e.torrent_file_backup = value;
            e.rt.dirty = true;
        }
    }

    /// Writes the torrent's .torrent file into the folder configured as
    /// `dir_conf` and records it.
    async fn write_backup(&self, id: &str, name: &str, dir_conf: &str) {
        let bytes = match self.export_torrent(id) {
            Ok(b) => b,
            Err(e) => {
                self.ctx.logger.warning(format!(
                    "Failed to backup .torrent file. Torrent: \"{name}\". Destination: \"{dir_conf}\". Reason: \"{e}\""
                ));
                return;
            }
        };
        let dir = resolve(dir_conf, &self.ctx.profile.data_dir);
        let stem = file_stem(name);
        let target = dir.clone();
        let result = tokio::task::spawn_blocking(move || write_unique(&target, &stem, &bytes))
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())));
        match result {
            Ok(path) => self.set_backup_registry(id, registry_value(dir_conf, &path)),
            Err(e) => self.ctx.logger.warning(format!(
                "Failed to backup .torrent file. Torrent: \"{name}\". Destination: \"{}\". Reason: \"{e}\"",
                dir.display()
            )),
        }
    }

    /// A torrent was added (`addTorrent_impl` → `backupTorrentFile`): with
    /// metadata its .torrent is written now, a magnet waits for it.
    pub(super) async fn backup_added_torrent(&self, id: &str, name: &str, has_metadata: bool) {
        let prefs = self.ctx.prefs.current();
        if !prefs.downloads.torrent_files_backup_enabled {
            return;
        }
        if has_metadata {
            self.write_backup(id, name, &prefs.downloads.torrent_files_backup_dir).await;
        } else {
            self.set_backup_registry(id, MAGNET_PENDING.into());
        }
    }

    /// `handleTorrentMetadataReceived`: a pending magnet backup is written,
    /// except for torrents with a v2 hash (their piece layers may still be
    /// missing; they are written when the torrent finishes).
    pub(super) async fn backup_on_metadata(&self, id: &str, name: &str) {
        let prefs = self.ctx.prefs.current();
        if !prefs.downloads.torrent_files_backup_enabled {
            return;
        }
        let pending = {
            let st = self.state.read();
            st.torrents.get(id).is_some_and(|e| {
                is_magnet(&e.torrent_file_backup) && e.meta.as_ref().is_some_and(|m| m.info_hash_v2.is_none())
            })
        };
        if pending {
            self.write_backup(id, name, &prefs.downloads.torrent_files_backup_dir).await;
        }
    }

    /// `handleTorrentFinished`: a pending magnet backup is written (to the
    /// finished folder when enabled); an existing backup moves to the
    /// finished folder when enabled and not there yet.
    pub(super) async fn backup_on_finished(&self, id: &str, name: &str) {
        let prefs = self.ctx.prefs.current();
        let d = &prefs.downloads;
        let Some(registry) = self.backup_registry(id).filter(|r| !r.is_empty()) else { return };
        if is_magnet(&registry) {
            if d.torrent_files_backup_enabled {
                let dir_conf = if d.torrent_files_finished_backup_dir_enabled {
                    &d.torrent_files_finished_backup_dir
                } else {
                    &d.torrent_files_backup_dir
                };
                self.write_backup(id, name, dir_conf).await;
            }
            return;
        }
        if !d.torrent_files_finished_backup_dir_enabled {
            return;
        }
        let data_dir = &self.ctx.profile.data_dir;
        let dir = resolve(&d.torrent_files_finished_backup_dir, data_dir);
        let old = resolve(&registry, data_dir);
        if old.parent() == Some(dir.as_path()) {
            return;
        }
        let stem = file_stem(name);
        let limit = self.torrent_file_limit();
        let (from, to) = (old.clone(), dir);
        let moved = tokio::task::spawn_blocking(move || move_into(&from, &to, &stem, limit))
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())));
        match moved {
            Ok(path) => self.set_backup_registry(id, registry_value(&d.torrent_files_finished_backup_dir, &path)),
            Err(_) => self.ctx.logger.info(format!(
                "Could not move .torrent file backup. Torrent: \"{name}\". File: \"{}\".",
                old.display()
            )),
        }
    }

    /// A removed torrent's backup (`removeTorrent` with "Remove backup when
    /// removing torrent").
    pub(super) async fn remove_torrent_backup(&self, name: &str, registry: String) {
        if registry.is_empty() || is_magnet(&registry) || !self.ctx.prefs.current().downloads.remove_torrent_file_backup {
            return;
        }
        let data_dir = self.ctx.profile.data_dir.clone();
        let result = tokio::task::spawn_blocking(move || remove(&registry, &data_dir)).await;
        if let Ok(Err((path, e))) = result {
            self.ctx.logger.info(format!(
                "Could not remove .torrent file backup. Torrent: \"{name}\". File: \"{}\". Reason: {e}",
                path.display()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_folders_against_the_data_folder() {
        let data = Path::new("/p/data");
        assert_eq!(resolve("", data), PathBuf::from("/p/data/backup"));
        assert_eq!(resolve("backup", data), PathBuf::from("/p/data/backup"));
        assert_eq!(resolve("a/./b/", data), PathBuf::from("/p/data/a/b"));
        assert_eq!(resolve("/abs//dir/", data), PathBuf::from("/abs/dir"));
        assert_eq!(resolve("backup/x.torrent", data), PathBuf::from("/p/data/backup/x.torrent"));
    }

    #[test]
    fn registry_values_keep_relative_folders_relative() {
        let file = Path::new("/p/data/backup/A (1).torrent");
        assert_eq!(registry_value("backup", file), "backup/A (1).torrent");
        assert_eq!(registry_value("", file), "backup/A (1).torrent");
        assert_eq!(registry_value("sub/dir/", file), "sub/dir/A (1).torrent");
        assert_eq!(registry_value("/abs", Path::new("/abs/A.torrent")), "/abs/A.torrent");
        assert!(is_magnet("magnet:") && is_magnet("MAGNET:?xt=") && !is_magnet("backup/m.torrent") && !is_magnet(""));
    }

    #[test]
    fn file_stems_are_safe_names() {
        assert_eq!(file_stem("../../etc/passwd"), "....etcpasswd");
        assert_eq!(file_stem(".."), "torrent");
        assert_eq!(file_stem("a/b\\c:d\u{7}e"), "abcde");
        assert_eq!(file_stem("  "), "torrent");
        let long = "й".repeat(300);
        let s = file_stem(&long);
        assert!(s.len() <= MAX_STEM_BYTES && s.chars().all(|c| c == 'й'));
    }

    #[test]
    fn writes_unique_files_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("new/backup");
        let a = write_unique(&folder, "Name", b"one").unwrap();
        let b = write_unique(&folder, "Name", b"two").unwrap();
        assert_eq!(a, folder.join("Name.torrent"));
        assert_eq!(b, folder.join("Name (1).torrent"));
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        // A symbolic link with the target name is skipped, not followed.
        let outside = dir.path().join("outside");
        std::os::unix::fs::symlink(&outside, folder.join("Link.torrent")).unwrap();
        let c = write_unique(&folder, "Link", b"three").unwrap();
        assert_eq!(c, folder.join("Link (1).torrent"));
        assert!(!outside.exists());
        // A stem that is not a plain file name is refused.
        assert!(write_unique(&folder, "../escape", b"x").is_err());
        assert!(!dir.path().join("new/escape.torrent").exists());
    }

    #[test]
    fn moves_and_removes_backups() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let first = write_unique(&data.join("backup"), "T", b"meta").unwrap();
        let done = data.join("done");
        let moved = move_into(&first, &done, "T", 1024).unwrap();
        assert_eq!(moved, done.join("T.torrent"));
        assert!(!first.exists());
        assert_eq!(std::fs::read(&moved).unwrap(), b"meta");
        // A missing file cannot be moved.
        assert!(move_into(&first, &done, "T", 1024).is_err());

        let registry = registry_value("done", &moved);
        assert_eq!(registry, "done/T.torrent");
        assert_eq!(remove(&registry, data).unwrap(), moved);
        assert!(!moved.exists());
        // Only regular .torrent files are removed.
        std::fs::write(data.join("keep.txt"), b"x").unwrap();
        assert!(remove("keep.txt", data).is_err());
        assert!(data.join("keep.txt").exists());
        std::fs::create_dir(data.join("d.torrent")).unwrap();
        assert!(remove("d.torrent", data).is_err());
        std::os::unix::fs::symlink(data.join("keep.txt"), data.join("l.torrent")).unwrap();
        assert!(remove("l.torrent", data).is_err());
        assert!(data.join("keep.txt").exists());
    }
}
