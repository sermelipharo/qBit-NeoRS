//! Peer country lookup with the DB-IP "IP to Country Lite" database
//! (`data/GeoDB/dbip-country-lite.mmdb`), qBittorrent `Net::GeoIPManager`.
//!
//! While "Resolve peer countries" is on, the database is memory-mapped and
//! refreshed when it is missing or from an earlier month (checked on load and
//! daily). Downloads come only from the fixed `https://download.db-ip.com`
//! URL (current month, then the previous one), go through the "general
//! purposes" proxy, are size-capped compressed and decompressed (no zip
//! bombs), fully verified as a MaxMind DB country database and must be newer
//! than the loaded one before they atomically replace the file. Turning the
//! option off unloads the database.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use maxminddb::{Mmap, PathElement, Reader};
use parking_lot::RwLock;

use super::http::{self, HttpPurpose, HttpRequest};
use crate::context::CoreContext;
use crate::model::LogLevel;

mod countries;

pub const DATABASE_FILE: &str = "dbip-country-lite.mmdb";
const MAX_COMPRESSED: u64 = 32 * 1024 * 1024;
const MAX_DECOMPRESSED: u64 = 128 * 1024 * 1024;
const RECHECK_INTERVAL: Duration = Duration::from_secs(24 * 3600);

pub struct GeoIp {
    ctx: CoreContext,
    path: PathBuf,
    db: RwLock<Option<Arc<Reader<Mmap>>>>,
}

impl GeoIp {
    /// Loads the database (if present and enabled) and keeps it updated,
    /// following the `resolve_peer_countries` preference.
    pub fn launch(ctx: &CoreContext) -> Arc<Self> {
        let this = Arc::new(Self {
            ctx: ctx.clone(),
            path: ctx.profile.geoip_dir().join(DATABASE_FILE),
            db: RwLock::new(None),
        });
        let task = this.clone();
        ctx.rt.spawn(async move { task.run().await });
        this
    }

    /// ISO 3166-1 alpha-2 code (uppercase), or None (unknown, private
    /// address, database not loaded).
    pub fn country_code(&self, ip: IpAddr) -> Option<String> {
        let db = self.db.read().clone()?;
        lookup(&db, ip)
    }

    /// The database is loaded.
    pub fn is_loaded(&self) -> bool {
        self.db.read().is_some()
    }

    fn log(&self, level: LogLevel, msg: String) {
        self.ctx.logger.log(level, msg);
    }

    async fn run(self: Arc<Self>) {
        let mut prefs_rx = self.ctx.prefs.subscribe();
        let mut shutdown = self.ctx.events.shutdown_requested();
        let mut enabled = false;
        loop {
            let want = prefs_rx.borrow_and_update().advanced.resolve_peer_countries;
            if want != enabled {
                enabled = want;
                if enabled {
                    self.load().await;
                    self.update_if_needed().await;
                } else {
                    *self.db.write() = None;
                }
            }
            tokio::select! {
                changed = prefs_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                _ = shutdown.changed() => break,
                _ = tokio::time::sleep(RECHECK_INTERVAL), if enabled => self.update_if_needed().await,
            }
        }
    }

    async fn load(&self) {
        let path = self.path.clone();
        // SAFETY (mmap): the file is only ever replaced by rename, never
        // modified or truncated in place, so the mapping stays valid.
        let result = tokio::task::spawn_blocking(move || unsafe { Reader::open_mmap(&path) }).await;
        match result {
            Ok(Ok(reader)) => {
                self.log(LogLevel::Info, loaded_message(&reader));
                *self.db.write() = Some(Arc::new(reader));
            }
            Ok(Err(e)) => self.log(LogLevel::Warning, format!("Couldn't load IP geolocation database. Reason: {e}")),
            Err(e) => self.log(LogLevel::Warning, format!("Couldn't load IP geolocation database. Reason: {e}")),
        }
    }

    async fn update_if_needed(&self) {
        let current_epoch = self.db.read().as_ref().map(|d| d.metadata().build_epoch as i64);
        let now = chrono::Utc::now().timestamp();
        if current_epoch.is_some_and(|e| !is_expired(e, now)) {
            return;
        }
        self.download(current_epoch, now).await;
    }

    /// Downloads, unpacks and verifies a newer database. The data streams
    /// through files next to the database (the compressed download, then the
    /// unpacked database, verified through a mapping), so the database never
    /// sits in the heap: buffering it there cost a ~15 MiB transient that
    /// the allocator kept dirty afterwards.
    async fn download(&self, current_epoch: Option<i64>, now: i64) {
        let gz_path = self.path.with_file_name(format!(".{DATABASE_FILE}.{:016x}.gz", rand::random::<u64>()));
        let mut downloaded = false;
        let mut last_error = String::new();
        for url in database_urls(now) {
            let req = HttpRequest { url, cookie: None, max_size: MAX_COMPRESSED, timeout_secs: 300 };
            match http::fetch_to_file(&self.ctx, req, HttpPurpose::General, &gz_path).await {
                Ok(r) if (200..300).contains(&r.status) => {
                    downloaded = true;
                    break;
                }
                Ok(r) => last_error = format!("HTTP {}", r.status),
                Err(e) => last_error = e.to_string(),
            }
        }
        if !downloaded {
            self.log(LogLevel::Warning, format!("Couldn't download IP geolocation database file. Reason: {last_error}"));
            return;
        }
        let path = self.path.clone();
        let outcome = tokio::task::spawn_blocking(move || -> Result<Option<Reader<Mmap>>, (LogLevel, String)> {
            let unpacked = path.with_file_name(format!(".{DATABASE_FILE}.{:016x}.tmp", rand::random::<u64>()));
            let result = std::fs::File::open(&gz_path)
                .map_err(|_| "Could not decompress IP geolocation database file.".to_string())
                .and_then(|gz| unpack_download(std::io::BufReader::new(gz), &unpacked, MAX_DECOMPRESSED));
            let _ = std::fs::remove_file(&gz_path);
            let reader = result.map_err(|e| (LogLevel::Warning, e))?;
            if current_epoch.is_some_and(|c| (reader.metadata().build_epoch as i64) <= c) {
                drop(reader);
                let _ = std::fs::remove_file(&unpacked);
                return Ok(None);
            }
            // The mapping follows the file through the atomic rename.
            std::fs::rename(&unpacked, &path).map_err(|e| {
                let _ = std::fs::remove_file(&unpacked);
                (LogLevel::Warning, format!("Couldn't save downloaded IP geolocation database file. Reason: {e}"))
            })?;
            Ok(Some(reader))
        })
        .await;
        match outcome {
            Ok(Ok(Some(reader))) => {
                self.log(LogLevel::Info, loaded_message(&reader));
                self.log(LogLevel::Info, "Successfully updated IP geolocation database.".to_string());
                *self.db.write() = Some(Arc::new(reader));
            }
            Ok(Ok(None)) => {}
            Ok(Err((level, msg))) => self.log(level, msg),
            Err(e) => self.log(LogLevel::Warning, format!("Couldn't load IP geolocation database. Reason: {e}")),
        }
    }
}

fn loaded_message<S: AsRef<[u8]>>(reader: &Reader<S>) -> String {
    let build = chrono::DateTime::from_timestamp(reader.metadata().build_epoch as i64, 0)
        .map(|d| d.format("%a %b %-d %H:%M:%S %Y").to_string())
        .unwrap_or_default();
    format!("IP geolocation database loaded. Type: {}. Build time: {build}.", reader.metadata().database_type)
}

fn lookup<S: AsRef<[u8]>>(db: &Reader<S>, ip: IpAddr) -> Option<String> {
    let ip = ip.to_canonical();
    let result = db.lookup(ip).ok()?;
    let code: String = result.decode_path(&[PathElement::Key("country"), PathElement::Key("iso_code")]).ok()??;
    (code.len() == 2 && code.chars().all(|c| c.is_ascii_alphanumeric())).then(|| code.to_ascii_uppercase())
}

/// qBittorrent's expiry rule: built in an earlier year or month, and today
/// is past the first day of the month (new files appear on the 1st).
pub fn is_expired(build_epoch: i64, now: i64) -> bool {
    use chrono::Datelike;
    let (Some(build), Some(cur)) = (chrono::DateTime::from_timestamp(build_epoch, 0), chrono::DateTime::from_timestamp(now, 0))
    else {
        return true;
    };
    cur.day() > 1 && (build.year() < cur.year() || build.month() < cur.month())
}

/// Download URLs: the current UTC month, then the previous one.
pub fn database_urls(now: i64) -> Vec<String> {
    use chrono::Datelike;
    let cur = chrono::DateTime::from_timestamp(now, 0).unwrap_or_default();
    let (y, m) = (cur.year(), cur.month());
    let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
    [(y, m), (py, pm)]
        .iter()
        .map(|(y, m)| format!("https://download.db-ip.com/free/dbip-country-lite-{y:04}-{m:02}.mmdb.gz"))
        .collect()
}

/// Gunzips a downloaded database into `out` (created exclusively, mode 0644,
/// streamed, at most `max_size` bytes) and verifies it as a MaxMind DB
/// country database. On success the reader maps `out`; on failure the file
/// created here is removed.
pub fn unpack_download(gz: impl std::io::Read, out: &Path, max_size: u64) -> Result<Reader<Mmap>, String> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(out).map_err(save_error)?;
    let result = unpack_and_verify(gz, file, out, max_size);
    if result.is_err() {
        let _ = std::fs::remove_file(out);
    }
    result
}

fn save_error(e: std::io::Error) -> String {
    format!("Couldn't save downloaded IP geolocation database file. Reason: {e}")
}

fn unpack_and_verify(
    gz: impl std::io::Read,
    mut file: std::fs::File,
    out: &Path,
    max_size: u64,
) -> Result<Reader<Mmap>, String> {
    use std::io::{Read, Write};

    let decompress_error = || "Could not decompress IP geolocation database file.".to_string();
    let mut decoder = flate2::read::GzDecoder::new(gz).take(max_size + 1);
    // Read errors come from the decoder (corrupt or truncated data), write
    // errors from the file.
    let mut buf = vec![0u8; 64 * 1024];
    let mut written = 0u64;
    loop {
        let n = match decoder.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(decompress_error()),
        };
        written += n as u64;
        if written > max_size {
            return Err(decompress_error());
        }
        file.write_all(&buf[..n]).map_err(save_error)?;
    }
    drop(buf);
    file.sync_all().map_err(save_error)?;
    drop(file);
    // SAFETY (mmap): `out` is private to this download until it is renamed
    // into place, and never modified afterwards.
    let reader =
        unsafe { Reader::open_mmap(out) }.map_err(|e| format!("Couldn't load IP geolocation database. Reason: {e}"))?;
    reader.verify().map_err(|e| format!("Couldn't load IP geolocation database. Reason: {e}"))?;
    if !reader.metadata().database_type.contains("Country") {
        return Err(format!(
            "Couldn't load IP geolocation database. Reason: unexpected database type {}",
            reader.metadata().database_type
        ));
    }
    Ok(reader)
}

/// English country name for an ISO code (Peers tab tooltip / WebAPI
/// `country`); "N/A" for unknown codes.
pub fn country_name(code: &str) -> String {
    let code = code.trim().to_ascii_uppercase();
    countries::COUNTRIES
        .binary_search_by(|(c, _)| (*c).cmp(code.as_str()))
        .map(|i| countries::COUNTRIES[i].1.to_string())
        .unwrap_or_else(|_| "N/A".to_string())
}

#[cfg(test)]
mod tests;
