//! Watched folders (`TorrentFilesWatcher`): polled by the session timer.
//! `.torrent` files are added and deleted; files that cannot be parsed are
//! retried and finally renamed to `<file>.qbt_rejected`; `.magnet` files are
//! read line by line and deleted.
//!
//! Symlinks inside watched folders are never followed, file sizes are
//! capped and a scan visits a bounded number of entries.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::adding::Descriptor;
use super::metainfo;
use super::paths;
use super::session::Session;
use crate::model::{AddTorrentParams, TorrentParamsTemplate};
use crate::prefs::{Preferences, WatchedFolder};

const MAX_RETRIES: u32 = 5;
const MAX_DEPTH: usize = 8;
const MAX_FILES_PER_SCAN: usize = 1000;
const MAX_MAGNET_FILE: u64 = 1024 * 1024;

enum Content {
    Torrent(Vec<u8>),
    Magnet(String),
    TooLarge,
}

struct Found {
    folder: usize,
    path: PathBuf,
    /// Sub-folder relative to the watched folder (`/`-separated).
    subdir: String,
    content: Content,
}

fn scan(folders: &[WatchedFolder], torrent_limit: u64) -> Vec<Found> {
    let roots: HashSet<PathBuf> = folders.iter().map(|f| PathBuf::from(f.path.trim())).collect();
    let mut out = Vec::new();
    for (i, folder) in folders.iter().enumerate() {
        let root = PathBuf::from(paths::expand_home(folder.path.trim()));
        if !root.is_absolute() || !root.is_dir() {
            continue;
        }
        walk(&root, &root, i, folder.recursive, 0, &roots, torrent_limit, &mut out);
        if out.len() >= MAX_FILES_PER_SCAN {
            break;
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn walk(root: &Path, dir: &Path, folder: usize, recursive: bool, depth: usize, roots: &HashSet<PathBuf>, limit: u64, out: &mut Vec<Found>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        if out.len() >= MAX_FILES_PER_SCAN {
            return;
        }
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if recursive && depth < MAX_DEPTH && !roots.contains(&path) {
                walk(root, &path, folder, recursive, depth + 1, roots, limit, out);
            }
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let is_torrent = name.ends_with(".torrent");
        let is_magnet = name.ends_with(".magnet");
        if !is_torrent && !is_magnet {
            continue;
        }
        let subdir = path
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .map(|p| p.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"))
            .unwrap_or_default();
        let cap = if is_torrent { limit } else { MAX_MAGNET_FILE };
        let content = match super::resume::read_limited_no_follow(&path, cap) {
            Ok(data) if is_torrent => Content::Torrent(data),
            Ok(data) => Content::Magnet(String::from_utf8_lossy(&data).into_owned()),
            Err(_) if entry.metadata().is_ok_and(|m| m.len() > cap) => Content::TooLarge,
            Err(_) => continue,
        };
        out.push(Found { folder, path, subdir, content });
    }
}

/// Folder parameters (+ sub-folder mapping in recursive mode) as
/// `AddTorrentParams`.
pub(super) fn template_params(t: &TorrentParamsTemplate, subdir: &str, prefs: &Preferences) -> AddTorrentParams {
    let non_empty = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
    let mut p = AddTorrentParams {
        category: non_empty(&t.category),
        tags: t.tags.clone(),
        save_path: non_empty(&t.save_path),
        use_download_path: t.use_download_path,
        download_path: non_empty(&t.download_path),
        auto_tmm: t.auto_tmm,
        stopped: t.stopped,
        stop_condition: t.stop_condition,
        skip_checking: t.skip_checking,
        add_to_queue_top: t.add_to_queue_top,
        content_layout: t.content_layout,
        download_limit: (t.download_limit > 0).then_some(t.download_limit),
        upload_limit: (t.upload_limit > 0).then_some(t.upload_limit),
        ratio_limit: Some(t.ratio_limit),
        seeding_time_limit: Some(t.seeding_time_limit),
        inactive_seeding_time_limit: Some(t.inactive_seeding_time_limit),
        ..Default::default()
    };
    let subdir = subdir.split('/').filter(|c| paths::is_valid_name(c)).collect::<Vec<_>>().join("/");
    if !subdir.is_empty() {
        if t.auto_tmm.unwrap_or(prefs.downloads.auto_tmm_enabled) {
            p.category = Some(match &p.category {
                Some(c) => format!("{c}/{subdir}"),
                None => subdir,
            });
        } else {
            let base = p.save_path.clone().unwrap_or_else(|| super::torrent::global_paths(prefs).save_path);
            p.save_path = Some(paths::join(&base, &subdir));
        }
    }
    p
}

impl Session {
    pub(super) async fn scan_watched_folders(&self) {
        let prefs = self.ctx.prefs.current();
        let folders = prefs.downloads.watched_folders.clone();
        let limit = self.torrent_file_limit();
        let scan_folders = folders.clone();
        let found = tokio::task::spawn_blocking(move || scan(&scan_folders, limit)).await.unwrap_or_default();
        let seen: HashSet<PathBuf> = found.iter().map(|f| f.path.clone()).collect();
        self.misc.lock().watched_retries.retain(|p, _| seen.contains(p));
        for f in found {
            let Some(folder) = folders.get(f.folder) else { continue };
            let params = template_params(&folder.params, &f.subdir, &prefs);
            match f.content {
                Content::Torrent(bytes) => match metainfo::parse_torrent(&bytes, self.limits()) {
                    Ok(parsed) => {
                        let desc = Descriptor::Meta { bytes: std::sync::Arc::new(bytes), parsed: Box::new(parsed) };
                        let _ = self.add_descriptor(desc, params, true, true).await;
                        let _ = std::fs::remove_file(&f.path);
                        self.misc.lock().watched_retries.remove(&f.path);
                    }
                    Err(_) => self.retry_or_reject(&f.path),
                },
                Content::TooLarge => self.reject(&f.path),
                Content::Magnet(text) => {
                    for line in text.lines().take(10_000) {
                        let uri = line.trim();
                        if uri.is_empty() {
                            continue;
                        }
                        match metainfo::parse_magnet(uri) {
                            Ok(link) => {
                                let _ = self.add_descriptor(Descriptor::Magnet(link), params.clone(), true, false).await;
                            }
                            Err(reason) => {
                                let shown: String = uri.chars().take(200).collect();
                                self.ctx.logger.warning(format!("Invalid Magnet URI. URI: {shown}. Reason: {reason}"));
                            }
                        }
                    }
                    let _ = std::fs::remove_file(&f.path);
                }
            }
        }
    }

    fn retry_or_reject(&self, path: &Path) {
        let retries = {
            let mut m = self.misc.lock();
            let r = m.watched_retries.entry(path.to_path_buf()).or_insert(0);
            *r += 1;
            *r
        };
        if retries >= MAX_RETRIES {
            self.misc.lock().watched_retries.remove(path);
            self.reject(path);
        }
    }

    fn reject(&self, path: &Path) {
        self.ctx.logger.warning(format!("Rejecting failed torrent file: {}", path.display()));
        let mut target = path.as_os_str().to_os_string();
        target.push(".qbt_rejected");
        let _ = std::fs::rename(path, PathBuf::from(target));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_skips_symlinks_and_maps_subfolders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("watch");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("x.torrent"), b"d").unwrap();
        std::fs::write(root.join("a/b/y.TORRENT"), b"d").unwrap();
        std::fs::write(root.join("a/m.magnet"), b"magnet:?xt=urn:btih:abc").unwrap();
        std::fs::write(root.join("ignored.txt"), b"x").unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("z.torrent"), b"d").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        std::os::unix::fs::symlink(outside.join("z.torrent"), root.join("l.torrent")).unwrap();
        std::fs::write(root.join("big.torrent"), vec![0u8; 100]).unwrap();

        let folder = WatchedFolder { path: root.to_string_lossy().into_owned(), recursive: true, params: Default::default() };
        let found = scan(std::slice::from_ref(&folder), 50);
        let mut names: Vec<(String, String)> =
            found.iter().map(|f| (f.path.file_name().unwrap().to_string_lossy().into_owned(), f.subdir.clone())).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                ("big.torrent".into(), "".into()),
                ("m.magnet".into(), "a".into()),
                ("x.torrent".into(), "".into()),
                ("y.TORRENT".into(), "a/b".into()),
            ]
        );
        assert!(found.iter().any(|f| matches!(f.content, Content::TooLarge)));
        let flat = WatchedFolder { recursive: false, ..folder };
        assert_eq!(scan(&[flat], 50).len(), 2);
    }

    #[test]
    fn subfolder_mapping() {
        let mut prefs = Preferences::default();
        prefs.downloads.save_path = "/dl".into();
        let manual = TorrentParamsTemplate { save_path: "/in".into(), auto_tmm: Some(false), ..Default::default() };
        assert_eq!(template_params(&manual, "a/b", &prefs).save_path.as_deref(), Some("/in/a/b"));
        let auto = TorrentParamsTemplate { category: "TV".into(), auto_tmm: Some(true), ..Default::default() };
        assert_eq!(template_params(&auto, "Show", &prefs).category.as_deref(), Some("TV/Show"));
        let none = TorrentParamsTemplate::default();
        assert_eq!(template_params(&none, "", &prefs).save_path, None);
        assert_eq!(template_params(&none, "s", &prefs).save_path.as_deref(), Some("/dl/s"));
    }
}
