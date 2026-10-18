//! The app's "Import from qBittorrent… / Transmission…" (UniFFI exports).
//!
//! The import never runs next to a live core on the same profile. The app
//! plans it with a dry run ([`import_plan`]) while its session runs, then
//! records the request ([`import_request`]), quits normally (resume data
//! saved, engine stopped, WebUI closed) and relaunches itself. The new
//! instance runs the pending request ([`import_run_pending`]) before it
//! creates its core: the backup and the migration see a profile nobody
//! uses, and nothing of the old core is left in memory to write after them.
//!
//! These functions block on disk I/O (and run `defaults export` for
//! Transmission): call them off the main thread.

use std::path::{Path, PathBuf};

use super::Report;
use crate::app::{AppConfig, Profile};
use crate::error::{QbtError, QbtResult};

/// The applications the app imports from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum ImportSource {
    QBittorrent,
    Transmission,
}

impl ImportSource {
    pub fn bundle_id(self) -> &'static str {
        match self {
            ImportSource::QBittorrent => "org.qbittorrent.qBittorrent",
            ImportSource::Transmission => super::transmission::BUNDLE_ID,
        }
    }
}

/// What was found of a source application.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ImportDetection {
    pub source: ImportSource,
    pub found: bool,
    /// Its data folder.
    pub location: String,
    /// Torrents in it (files found, before any parsing).
    pub torrents: u32,
    pub bundle_id: String,
}

/// The dry run shown before importing.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ImportPlan {
    pub source: ImportSource,
    /// The profile's settings are kept and its torrents only get what is
    /// missing (it has torrents, or the user chose to keep its settings).
    pub merge: bool,
    pub profile_has_torrents: bool,
    pub torrents: u32,
    /// Torrents whose pieces carry over without a check.
    pub torrents_with_pieces: u32,
    /// Torrents checked in full when they start.
    pub torrents_to_check: u32,
    /// Magnet links still waiting for metadata.
    pub magnets: u32,
    /// Torrents already in the profile (merge).
    pub torrents_merged: u32,
    pub torrents_failed: u32,
    pub categories: u32,
    pub tags: u32,
    pub watched_folders: u32,
    pub rss_feeds: u32,
    pub search_plugins: u32,
    pub gui_settings: u32,
    /// Transmission's remote access was on (its password is not copied).
    pub remote_access_enabled: bool,
    /// One line per kind of thing that doesn't carry over.
    pub not_carried: Vec<String>,
    pub warnings: Vec<String>,
    /// The full report (`migration/report.txt`).
    pub report: String,
}

/// The result of an import.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ImportOutcome {
    pub plan: ImportPlan,
    /// Copy of the profile's files taken first ("" = there was nothing).
    pub backup_dir: String,
    pub report_path: String,
}

/// A pending import (`migration/pending-import.json`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ImportRequest {
    pub source: ImportSource,
    pub keep_settings: bool,
    /// Unix seconds.
    pub requested_at: i64,
}

/// Requests older than this are dropped instead of run.
pub const REQUEST_LIFETIME_SECS: i64 = 15 * 60;
const REQUEST_FILE: &str = "migration/pending-import.json";

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn profile_root(profile_dir: &str) -> PathBuf {
    Profile::resolve(&AppConfig { profile_dir: profile_dir.to_owned(), ..Default::default() }).root
}

fn count_files(dir: &Path, exts: &[&str]) -> u32 {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter(|e| e.file_name().to_str().is_some_and(|n| !n.starts_with('.') && exts.iter().any(|x| n.ends_with(x)))).count() as u32)
        .unwrap_or(0)
}

/// Looks for a source application's data in `home`.
pub fn detect_in(source: ImportSource, home: &Path) -> ImportDetection {
    let (found, location, torrents) = match source {
        ImportSource::QBittorrent => {
            let o = super::Options::macos_defaults(home, PathBuf::new());
            let bt = o.source_data.join("BT_backup");
            let n = count_files(&bt, &[".fastresume"]);
            (o.source_config.join("qBittorrent.ini").is_file() || n > 0, o.source_data, n)
        }
        ImportSource::Transmission => {
            let o = super::transmission::Options::macos_defaults(home, PathBuf::new());
            let n = count_files(&o.source_dir.join("Torrents"), &[".torrent", ".magnet"]);
            let prefs = home.join(format!("Library/Preferences/{}.plist", super::transmission::DEFAULTS_DOMAIN));
            (n > 0 || o.source_dir.join("Resume").is_dir() || prefs.is_file(), o.source_dir, n)
        }
    };
    ImportDetection { source, found, location: location.display().to_string(), torrents, bundle_id: source.bundle_id().to_owned() }
}

/// Looks for a source application's data.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn import_detect(source: ImportSource) -> ImportDetection {
    detect_in(source, &home())
}

/// Whether a profile is still empty (no torrents): the app then offers an
/// import on its first launch.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn import_profile_is_empty(profile_dir: String) -> bool {
    !super::transmission::profile_has_torrents(&profile_root(&profile_dir))
}

/// Options of a run for `home` (sources) and `profile`.
pub(crate) enum Run {
    QBittorrent(super::Options),
    Transmission(super::transmission::Options),
}

pub(crate) fn options(source: ImportSource, home: &Path, profile: &Path, keep_settings: bool, dry_run: bool) -> Run {
    let has_torrents = super::transmission::profile_has_torrents(profile);
    let has_settings = profile.join("config/preferences.json").is_file();
    let merge = has_settings && (keep_settings || has_torrents);
    match source {
        ImportSource::QBittorrent => {
            let mut o = super::Options::macos_defaults(home, profile.to_path_buf());
            o.merge = merge;
            o.force = !merge;
            o.dry_run = dry_run;
            o.gui_plist_in_migration = true;
            Run::QBittorrent(o)
        }
        ImportSource::Transmission => {
            let mut o = super::transmission::Options::macos_defaults(home, profile.to_path_buf());
            o.merge = merge;
            o.force = !merge;
            o.dry_run = dry_run;
            o.gui_plist_in_migration = true;
            Run::Transmission(o)
        }
    }
}

impl Run {
    fn merge(&self) -> bool {
        match self {
            Run::QBittorrent(o) => o.merge,
            Run::Transmission(o) => o.merge,
        }
    }

    fn profile(&self) -> &Path {
        match self {
            Run::QBittorrent(o) => &o.profile,
            Run::Transmission(o) => &o.profile,
        }
    }

    pub(crate) fn execute(&self) -> Result<Report, String> {
        match self {
            Run::QBittorrent(o) => super::run(o),
            Run::Transmission(o) => super::transmission::run(o),
        }
    }
}

/// The summary the app shows.
pub fn plan_from_report(source: ImportSource, merge: bool, profile_has_torrents: bool, rep: &Report) -> ImportPlan {
    let merged = rep.torrents.iter().filter(|t| t.state.starts_with("merged")).count();
    let magnets = rep.torrents.iter().filter(|t| t.pieces.starts_with("no metadata")).count();
    let with_pieces = rep.torrents.iter().filter(|t| t.pieces.starts_with("bitfield")).count();
    let to_check = rep.torrents.iter().filter(|t| t.pieces.starts_with("full recheck")).count();
    let mut not_carried = Vec::new();
    match source {
        ImportSource::QBittorrent => {
            for (why, keys) in &rep.ini_unused {
                not_carried.push(format!("{why}: {} setting(s)", keys.len()));
            }
            if !rep.prefs_diff.is_empty() {
                not_carried.push(format!("{} preference(s) read differently in qBit NeoRS (see the report)", rep.prefs_diff.len()));
            }
            not_carried.push("cookies, partially downloaded pieces, known peers, logs".into());
        }
        ImportSource::Transmission => {
            for items in rep.ini_unused.values() {
                not_carried.extend(items.iter().cloned());
            }
            not_carried.push("known peers, per-torrent bandwidth priority and peer limit".into());
        }
    }
    let mut warnings = rep.warnings.clone();
    for (id, e) in &rep.failed_torrents {
        warnings.push(format!("{id}: {e}"));
    }
    ImportPlan {
        source,
        merge,
        profile_has_torrents,
        torrents: rep.torrents.len() as u32,
        torrents_with_pieces: with_pieces as u32,
        torrents_to_check: to_check as u32,
        magnets: magnets as u32,
        torrents_merged: merged as u32,
        torrents_failed: rep.failed_torrents.len() as u32,
        categories: rep.categories.len() as u32,
        tags: rep.tags.len() as u32,
        watched_folders: rep.watched_folders as u32,
        rss_feeds: rep.rss_feeds as u32,
        search_plugins: rep.search_plugins.len() as u32,
        gui_settings: rep.gui_keys.len() as u32,
        remote_access_enabled: rep.remote_access_enabled,
        not_carried,
        warnings,
        report: rep.to_text(),
    }
}

pub fn plan_in(source: ImportSource, home: &Path, profile: &Path, keep_settings: bool) -> Result<ImportPlan, String> {
    let run = options(source, home, profile, keep_settings, true);
    let rep = run.execute()?;
    Ok(plan_from_report(source, run.merge(), super::transmission::profile_has_torrents(profile), &rep))
}

/// Dry run of an import into the profile (`profile_dir` "" = the default
/// one). Safe while the app's core runs: nothing is written.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn import_plan(source: ImportSource, profile_dir: String, keep_settings: bool) -> QbtResult<ImportPlan> {
    plan_in(source, &home(), &profile_root(&profile_dir), keep_settings).map_err(QbtError::other)
}

/// Records an import for the next launch.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn import_request(source: ImportSource, profile_dir: String, keep_settings: bool) -> QbtResult<()> {
    let request = ImportRequest { source, keep_settings, requested_at: chrono::Utc::now().timestamp() };
    let path = profile_root(&profile_dir).join(REQUEST_FILE);
    crate::bittorrent::resume::write_private_atomic(&path, &serde_json::to_vec_pretty(&request)?)?;
    Ok(())
}

/// The pending import, if one was requested recently (an older request is
/// removed and ignored).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn import_pending(profile_dir: String) -> Option<ImportRequest> {
    pending_in(&profile_root(&profile_dir), chrono::Utc::now().timestamp())
}

pub fn pending_in(profile: &Path, now: i64) -> Option<ImportRequest> {
    let path = profile.join(REQUEST_FILE);
    let data = super::read_opt(&path, 64 * 1024)?;
    match serde_json::from_slice::<ImportRequest>(&data) {
        Ok(r) if (0..=REQUEST_LIFETIME_SECS).contains(&(now - r.requested_at)) => Some(r),
        _ => {
            let _ = std::fs::remove_file(&path);
            None
        }
    }
}

/// Drops a pending import.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn import_cancel_pending(profile_dir: String) {
    let _ = std::fs::remove_file(profile_root(&profile_dir).join(REQUEST_FILE));
}

/// The GUI settings an import of `source` brings, as an XML property list
/// (the app applies them to its defaults store before its settings load).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn import_gui_settings(source: ImportSource) -> String {
    gui_settings_in(source, &home())
}

pub fn gui_settings_in(source: ImportSource, home: &Path) -> String {
    let values = match source {
        ImportSource::QBittorrent => {
            let o = super::Options::macos_defaults(home, PathBuf::new());
            let ini = super::read_opt(&o.source_config.join("qBittorrent.ini"), super::MAX_CONFIG).map(|d| super::ini::IniFile::parse(&d)).unwrap_or_default();
            let history = super::read_opt(&o.source_data.join("SearchUI/History.txt"), super::MAX_CONFIG).map(|d| String::from_utf8_lossy(&d).into_owned());
            super::gui::collect(&ini, history.as_deref()).0
        }
        ImportSource::Transmission => {
            let o = super::transmission::Options::macos_defaults(home, PathBuf::new());
            super::transmission::map_settings(&super::transmission::read_defaults(&o, &mut Report::default())).gui
        }
    };
    super::gui::to_plist(&values)
}

/// Runs the pending import of the profile: removes the request, backs the
/// profile up and migrates. Call before creating the core on that profile.
/// None when nothing is pending.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn import_run_pending(profile_dir: String) -> QbtResult<Option<ImportOutcome>> {
    let profile = profile_root(&profile_dir);
    let Some(request) = pending_in(&profile, chrono::Utc::now().timestamp()) else { return Ok(None) };
    let _ = std::fs::remove_file(profile.join(REQUEST_FILE));
    run_in(request.source, &home(), &profile, request.keep_settings).map(Some).map_err(QbtError::other)
}

pub fn run_in(source: ImportSource, home: &Path, profile: &Path, keep_settings: bool) -> Result<ImportOutcome, String> {
    let has_torrents = super::transmission::profile_has_torrents(profile);
    let run = options(source, home, profile, keep_settings, false);
    // Refusals (profile in use, source running) before anything is copied.
    let check = options(source, home, profile, keep_settings, true).execute()?;
    if let Some(w) = check.warnings.iter().find(|w| w.contains("quit it first")) {
        return Err(w.clone());
    }
    let backup = super::backup_profile(run.profile())?.map(|p| p.display().to_string()).unwrap_or_default();
    let rep = run.execute()?;
    Ok(ImportOutcome {
        plan: plan_from_report(source, run.merge(), has_torrents, &rep),
        backup_dir: backup,
        report_path: profile.join("migration/report.txt").display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_expire() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        assert_eq!(pending_in(p, 100), None);
        let r = ImportRequest { source: ImportSource::Transmission, keep_settings: true, requested_at: 1_000 };
        crate::bittorrent::resume::write_private_atomic(&p.join(REQUEST_FILE), &serde_json::to_vec(&r).unwrap()).unwrap();
        assert_eq!(pending_in(p, 1_000 + 60), Some(r.clone()));
        assert_eq!(pending_in(p, 1_000 + REQUEST_LIFETIME_SECS + 1), None);
        assert!(!p.join(REQUEST_FILE).exists(), "an expired request is removed");
        std::fs::write(p.join(REQUEST_FILE), b"{garbage").unwrap();
        assert_eq!(pending_in(p, 1_000), None);
    }

    #[test]
    fn detects_and_chooses_the_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert!(!detect_in(ImportSource::Transmission, home).found);
        assert!(!detect_in(ImportSource::QBittorrent, home).found);
        let tr = home.join("Library/Application Support/Transmission/Torrents");
        std::fs::create_dir_all(&tr).unwrap();
        std::fs::write(tr.join("a.torrent"), b"x").unwrap();
        std::fs::write(tr.join("b.magnet"), b"x").unwrap();
        std::fs::write(tr.join(".hidden.torrent"), b"x").unwrap();
        let d = detect_in(ImportSource::Transmission, home);
        assert!(d.found);
        assert_eq!(d.torrents, 2);
        assert_eq!(d.bundle_id, "org.m0k.transmission");

        // Mode: a new profile is written fully; settings only are replaced
        // unless kept; torrents in the profile always mean merge.
        let profile = home.join("profile");
        assert!(!options(ImportSource::Transmission, home, &profile, true, true).merge(), "nothing to merge into");
        std::fs::create_dir_all(profile.join("config")).unwrap();
        std::fs::write(profile.join("config/preferences.json"), b"{}").unwrap();
        assert!(!options(ImportSource::Transmission, home, &profile, false, true).merge());
        assert!(options(ImportSource::Transmission, home, &profile, true, true).merge());
        std::fs::create_dir_all(profile.join("data/BT_backup")).unwrap();
        std::fs::write(profile.join("data/BT_backup/x.fastresume"), b"{}").unwrap();
        assert!(options(ImportSource::QBittorrent, home, &profile, false, true).merge());
        assert!(!import_profile_is_empty(profile.display().to_string()));
    }
}
