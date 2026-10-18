//! Persistence of torrents in `data/BT_backup`: `<id>.torrent` (metadata)
//! and `<id>.fastresume` (our own versioned JSON schema), plus the small
//! session files (`data/stats.json`, `data/session_state.json`).
//!
//! Files are written atomically (temp file + fsync + rename) with mode 0600
//! and read with size limits.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::TrackerTierEntry;

pub const RESUME_VERSION: u32 = 1;
const MAX_RESUME_SIZE: u64 = 16 * 1024 * 1024;

/// Per-torrent resume record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FastResume {
    pub version: u32,
    pub info_hash: String,
    /// Explicit name ("" = from metadata / magnet).
    pub name: String,
    /// Magnet `dn` while metadata is missing.
    pub magnet_name: String,
    /// BEP 52 info hash (hex) from the magnet link while metadata is
    /// missing ("" = none).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub info_hash_v2: String,
    /// v1 info hash (hex) of a magnet link with both hashes while metadata
    /// is missing (the id is then the truncated v2 hash).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub info_hash_v1: String,
    /// Comment override (None = from metadata).
    pub comment: Option<String>,
    pub category: String,
    pub tags: Vec<String>,
    pub auto_tmm: bool,
    /// Manual paths (empty with ATM).
    pub save_path: String,
    pub download_path: String,
    pub content_layout: String,
    /// Current relative file paths (user view, after layout/renames).
    pub file_paths: Vec<String>,
    pub file_priorities: Vec<i32>,
    pub added_on: i64,
    pub completed_on: i64,
    pub seen_complete: i64,
    pub all_time_download: i64,
    pub all_time_upload: i64,
    pub active_time: i64,
    pub seeding_time: i64,
    pub last_download: i64,
    pub last_upload: i64,
    pub ratio_limit: f64,
    pub seeding_time_limit: i64,
    pub inactive_seeding_time_limit: i64,
    pub share_limit_action: String,
    pub download_limit: i64,
    pub upload_limit: i64,
    pub stopped: bool,
    /// "AutoManaged" or "Forced".
    pub operating_mode: String,
    pub stop_condition: String,
    /// 0-based, -1 when not queued.
    pub queue_position: i64,
    pub sequential: bool,
    pub first_last_piece_priority: bool,
    pub super_seeding: bool,
    /// The torrent completed at some point.
    pub seed_status: bool,
    pub trackers: Vec<TrackerTierEntry>,
    pub url_seeds: Vec<String>,
    /// Tracker and web seed URLs trusted by SSRF mitigation: those of a
    /// .torrent file from a local source and those the user added. Missing in
    /// old records: nothing is trusted.
    pub trusted_urls: Vec<String>,
    /// Peers to connect to (magnet `x.pe`, "Add peers"), `ip:port`.
    pub peers: Vec<String>,
    /// Wanted bytes completed when last saved (missing files detection).
    pub completed_bytes: i64,
    /// Current storage location of the data ("" = derived from the paths).
    pub location: String,
    pub disable_dht: bool,
    pub disable_pex: bool,
    pub disable_lsd: bool,
}

impl Default for FastResume {
    fn default() -> Self {
        Self {
            version: RESUME_VERSION,
            info_hash: String::new(),
            name: String::new(),
            magnet_name: String::new(),
            info_hash_v2: String::new(),
            info_hash_v1: String::new(),
            comment: None,
            category: String::new(),
            tags: Vec::new(),
            auto_tmm: false,
            save_path: String::new(),
            download_path: String::new(),
            content_layout: "Original".into(),
            file_paths: Vec::new(),
            file_priorities: Vec::new(),
            added_on: 0,
            completed_on: -1,
            seen_complete: -1,
            all_time_download: 0,
            all_time_upload: 0,
            active_time: 0,
            seeding_time: 0,
            last_download: -1,
            last_upload: -1,
            ratio_limit: -2.0,
            seeding_time_limit: -2,
            inactive_seeding_time_limit: -2,
            share_limit_action: "Default".into(),
            download_limit: 0,
            upload_limit: 0,
            stopped: false,
            operating_mode: "AutoManaged".into(),
            stop_condition: "None".into(),
            queue_position: -1,
            sequential: false,
            first_last_piece_priority: false,
            super_seeding: false,
            seed_status: false,
            trackers: Vec::new(),
            url_seeds: Vec::new(),
            trusted_urls: Vec::new(),
            peers: Vec::new(),
            completed_bytes: 0,
            location: String::new(),
            disable_dht: false,
            disable_pex: false,
            disable_lsd: false,
        }
    }
}

/// Writes `data` atomically with owner-only permissions.
pub fn write_private_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Reads a file refusing anything larger than `limit` (and non-regular
/// files such as FIFOs or devices).
pub fn read_limited(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    read_limited_impl(path, limit, false)
}

/// [`read_limited`] that also refuses a symbolic link as the last path
/// component (watched folders: the entry may be swapped after listing).
pub fn read_limited_no_follow(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    read_limited_impl(path, limit, true)
}

fn read_limited_impl(path: &Path, limit: u64, no_follow: bool) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    // O_NONBLOCK: opening a FIFO must not block (the type check below then
    // refuses it); it has no effect on reading regular files.
    let mut flags = libc::O_NONBLOCK;
    if no_follow {
        flags |= libc::O_NOFOLLOW;
    }
    let f = std::fs::OpenOptions::new().read(true).custom_flags(flags).open(path)?;
    let meta = f.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    if meta.len() > limit {
        return Err(std::io::Error::other(format!("file size ({}) exceeds the limit ({limit})", meta.len())));
    }
    let mut buf = Vec::with_capacity(meta.len() as usize);
    f.take(limit + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > limit {
        return Err(std::io::Error::other("file grew beyond the limit"));
    }
    Ok(buf)
}

#[derive(Clone)]
pub struct BackupDir {
    dir: PathBuf,
    /// Hash of the last record written per torrent in this run, so that a
    /// periodic save-all does not rewrite (and fsync) unchanged files.
    written: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, u64>>>,
}

impl BackupDir {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, written: Default::default() }
    }

    pub fn torrent_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.torrent"))
    }

    pub fn resume_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.fastresume"))
    }

    pub fn save_resume(&self, r: &FastResume) -> std::io::Result<()> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let data = serde_json::to_vec_pretty(r).map_err(std::io::Error::other)?;
        let path = self.resume_path(&r.info_hash);
        let mut h = DefaultHasher::new();
        data.hash(&mut h);
        let digest = h.finish();
        let unchanged = self.written.lock().is_ok_and(|w| w.get(&r.info_hash) == Some(&digest));
        if unchanged && path.is_file() {
            return Ok(());
        }
        write_private_atomic(&path, &data)?;
        if let Ok(mut w) = self.written.lock() {
            w.insert(r.info_hash.clone(), digest);
        }
        Ok(())
    }

    pub fn save_torrent_file(&self, id: &str, bytes: &[u8]) -> std::io::Result<()> {
        write_private_atomic(&self.torrent_path(id), bytes)
    }

    pub fn load_torrent_file(&self, id: &str, limit: u64) -> std::io::Result<Vec<u8>> {
        read_limited(&self.torrent_path(id), limit)
    }

    pub fn has_torrent_file(&self, id: &str) -> bool {
        self.torrent_path(id).is_file()
    }

    pub fn delete(&self, id: &str) {
        if let Ok(mut w) = self.written.lock() {
            w.remove(id);
        }
        let _ = std::fs::remove_file(self.resume_path(id));
        let _ = std::fs::remove_file(self.torrent_path(id));
    }

    /// All valid resume records. Invalid ones are reported in the second
    /// vector as (file name, reason).
    pub fn load_all(&self) -> (Vec<FastResume>, Vec<(String, String)>) {
        let mut ok = Vec::new();
        let mut failed = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return (ok, failed) };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".fastresume") else { continue };
            if id.len() != 40 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            match read_limited(&e.path(), MAX_RESUME_SIZE)
                .map_err(|e| e.to_string())
                .and_then(|d| serde_json::from_slice::<FastResume>(&d).map_err(|e| e.to_string()))
            {
                Ok(r) if r.info_hash.eq_ignore_ascii_case(id) => ok.push(r),
                Ok(_) => failed.push((name, "Mismatching info-hash detected in resume data".into())),
                Err(err) => failed.push((name, err)),
            }
        }
        ok.sort_by(|a, b| {
            let qa = if a.queue_position < 0 { i64::MAX } else { a.queue_position };
            let qb = if b.queue_position < 0 { i64::MAX } else { b.queue_position };
            qa.cmp(&qb).then(a.added_on.cmp(&b.added_on))
        });
        (ok, failed)
    }
}

/// Moves the resume data of a torrent saved under its v1 hash to
/// qBittorrent's id (the truncated v2 hash): hybrid torrents (`meta` with
/// both hashes) and magnet links with both hashes saved by older versions.
/// `torrent_bytes` are the bytes of `<old>.torrent`. The record's
/// `info_hash` is set to the new id even if the files can't be moved (they
/// are then saved under it later). Returns the new id.
///
/// Order: `<new>.torrent`, `<new>.fastresume`, then the old files go; an
/// interruption leaves the old record, which migrates again next time.
pub fn migrate_id(
    backup: &BackupDir,
    r: &mut FastResume,
    meta: Option<&std::sync::Arc<super::metainfo::TorrentMeta>>,
    torrent_bytes: Option<&[u8]>,
) -> Option<String> {
    use super::metainfo;
    let old = r.info_hash.to_ascii_lowercase();
    let new = match meta {
        Some(m) => (m.alt_id().as_deref() == Some(old.as_str())).then(|| m.id())?,
        None => {
            let v2 = metainfo::parse_hex32(&r.info_hash_v2)?;
            let new = metainfo::hex(&metainfo::truncate_v2(&v2));
            if new == old {
                return None;
            }
            // A magnet with both hashes: keep its v1 hash apart.
            r.info_hash_v1 = old.clone();
            new
        }
    };
    r.info_hash = new.clone();
    let moved = (|| -> std::io::Result<()> {
        if let Some(b) = torrent_bytes {
            backup.save_torrent_file(&new, b)?;
        }
        backup.save_resume(r)
    })();
    if moved.is_ok() {
        backup.delete(&old);
    }
    Some(new)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatsFile {
    pub alltime_dl: i64,
    pub alltime_ul: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionStateFile {
    pub use_alt_speed_limits: bool,
}

pub fn load_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    read_limited(path, MAX_RESUME_SIZE).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
}

pub fn save_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let data = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    write_private_atomic(path, &data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_reads_refuse_fifos_and_optionally_links() {
        let tmp = tempfile::tempdir().unwrap();
        let fifo = tmp.path().join("x.torrent");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        // Used to block forever in open(2) (no writer).
        let (tx, rx) = std::sync::mpsc::channel();
        let p = fifo.clone();
        std::thread::spawn(move || tx.send(read_limited(&p, 1024).is_err()).unwrap());
        assert!(rx.recv_timeout(std::time::Duration::from_secs(5)).expect("open blocked on a FIFO"));

        let real = tmp.path().join("real.torrent");
        std::fs::write(&real, b"d4:infode").unwrap();
        let link = tmp.path().join("link.torrent");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(read_limited(&link, 1024).is_ok());
        assert!(read_limited_no_follow(&link, 1024).is_err());
        assert_eq!(read_limited_no_follow(&real, 1024).unwrap(), b"d4:infode");
        assert!(read_limited(&real, 3).is_err());
    }

    #[test]
    fn resume_round_trip_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = BackupDir::new(tmp.path().join("BT_backup"));
        let id = "0123456789abcdef0123456789abcdef01234567".to_string();
        let r = FastResume {
            info_hash: id.clone(),
            name: "x".into(),
            category: "Movies/HD".into(),
            tags: vec!["a".into()],
            file_paths: vec!["d/f".into()],
            file_priorities: vec![0],
            ratio_limit: 1.5,
            queue_position: 3,
            trackers: vec![TrackerTierEntry { url: "http://t".into(), tier: 1 }],
            ..Default::default()
        };
        dir.save_resume(&r).unwrap();
        dir.save_torrent_file(&id, b"d4:infode").unwrap();
        let mode = std::fs::metadata(dir.resume_path(&id)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let (all, failed) = dir.load_all();
        assert!(failed.is_empty());
        assert_eq!(all, vec![r.clone()]);
        assert_eq!(dir.load_torrent_file(&id, 100).unwrap(), b"d4:infode");
        assert!(dir.load_torrent_file(&id, 3).is_err());

        // Corrupt and mismatching records are reported, not loaded.
        std::fs::write(dir.resume_path(&"a".repeat(40)), b"{not json").unwrap();
        let other = FastResume { info_hash: "b".repeat(40), ..Default::default() };
        std::fs::write(dir.resume_path(&"c".repeat(40)), serde_json::to_vec(&other).unwrap()).unwrap();
        let (all, failed) = dir.load_all();
        assert_eq!(all.len(), 1);
        assert_eq!(failed.len(), 2);

        // An unchanged record is not rewritten; a changed or deleted one is.
        let before = std::fs::metadata(dir.resume_path(&id)).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        dir.save_resume(&r).unwrap();
        assert_eq!(std::fs::metadata(dir.resume_path(&id)).unwrap().modified().unwrap(), before);
        let changed = FastResume { queue_position: 4, ..r.clone() };
        dir.save_resume(&changed).unwrap();
        assert_eq!(dir.load_all().0.iter().find(|x| x.info_hash == id).unwrap().queue_position, 4);
        std::fs::remove_file(dir.resume_path(&id)).unwrap();
        dir.save_resume(&changed).unwrap();
        assert!(dir.resume_path(&id).is_file());

        dir.delete(&id);
        assert!(!dir.has_torrent_file(&id));
    }

    #[test]
    fn unknown_fields_and_defaults() {
        let r: FastResume = serde_json::from_str(r#"{"info_hash":"x","future_field":1}"#).unwrap();
        assert_eq!(r.ratio_limit, -2.0);
        assert_eq!(r.queue_position, -1);
        assert_eq!(r.operating_mode, "AutoManaged");
    }
}
