//! Migration of a qBittorrent 5.x installation into a qBit NeoRS profile
//! (`qbit-neors-nox migrate-from-qbittorrent`, docs/MIGRATION.md).
//!
//! Everything of qBittorrent is only read: its configuration
//! (`qBittorrent.ini`, `qBittorrent-data.ini`, `categories.json`,
//! `watched_folders.json`, `rss/`), its data folder (`BT_backup`, `rss`,
//! `nova3`, `SearchUI`) and, optionally, its WebAPI with GET requests.
//! The only writes outside the new profile are the bytes of skipped files
//! that libtorrent kept in its part file (see [`partfile`]), created as
//! new files next to the torrent's data and only when that is enabled.

pub mod gui;
pub mod import;
pub mod ini;
pub mod libtorrent;
pub mod partfile;
pub mod plist;
pub mod prefs_map;
pub mod transmission;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::bittorrent::bencode::{self, Limits};
use crate::bittorrent::categories::{self, CategoryMap, GlobalPaths};
use crate::bittorrent::metainfo;
use crate::bittorrent::resume::{self, BackupDir, SessionStateFile, StatsFile};
use crate::prefs::{Preferences, PreferencesStore};

use ini::{IniFile, QValue, Variant};
use libtorrent::LtResume;

/// What to do with had pieces whose skipped-file part is only in
/// libtorrent's part file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkippedData {
    /// Rebuild and verify the piece, write the skipped file's bytes.
    Recover,
    /// Unclaim the piece (downloaded again when the torrent runs).
    Unclaim,
}

/// Test profile settings: no network side effects, nothing started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sandbox {
    pub listen_port: u16,
    pub webui_port: u16,
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Folder with `qBittorrent.ini` (macOS: `~/.config/qBittorrent`).
    pub source_config: PathBuf,
    /// Folder with `categories.json`, `watched_folders.json`, `rss/`
    /// (macOS: `~/Library/Preferences/qBittorrent`).
    pub source_prefs: PathBuf,
    /// qBittorrent's data folder (`~/Library/Application Support/qBittorrent`).
    pub source_data: PathBuf,
    /// Base URL of qBittorrent's running WebUI (GET requests only).
    pub webui: Option<String>,
    /// A saved `app/preferences` JSON instead of a live WebUI.
    pub webui_prefs_file: Option<PathBuf>,
    pub profile: PathBuf,
    pub dry_run: bool,
    /// Every torrent is added stopped.
    pub stop_all: bool,
    pub sandbox: Option<Sandbox>,
    pub skipped_data: SkippedData,
    /// Write into a profile that already has settings or torrents.
    pub force: bool,
    /// Merge into an existing profile: torrents already there only get
    /// what is missing (statistics, tracker tiers, web seeds, limits,
    /// queue position); its settings are kept.
    pub merge: bool,
    /// Write the GUI settings to `migration/` only (the app applies them
    /// itself through its defaults store).
    pub gui_plist_in_migration: bool,
}

impl Options {
    /// qBittorrent's default locations on macOS for `home`.
    pub fn macos_defaults(home: &Path, profile: PathBuf) -> Self {
        Self {
            source_config: home.join(".config/qBittorrent"),
            source_prefs: home.join("Library/Preferences/qBittorrent"),
            source_data: home.join("Library/Application Support/qBittorrent"),
            webui: None,
            webui_prefs_file: None,
            profile,
            dry_run: false,
            stop_all: false,
            sandbox: None,
            skipped_data: SkippedData::Recover,
            force: false,
            merge: false,
            gui_plist_in_migration: false,
        }
    }
}

/// One migrated torrent (no tracker URLs: they may carry passkeys).
#[derive(Clone, Debug, Default)]
pub struct TorrentSummary {
    pub id: String,
    pub name: String,
    pub state: String,
    /// Wanted progress from the imported bitfield (None = rechecked / no metadata).
    pub progress: Option<f64>,
    /// qBittorrent's own progress (WebAPI), when available.
    pub qbt_progress: Option<f64>,
    pub category: String,
    pub tags: Vec<String>,
    pub save_path: String,
    pub auto_tmm: bool,
    pub location: String,
    pub files: usize,
    pub skipped_files: usize,
    pub trackers: usize,
    pub web_seeds: usize,
    pub pieces: String,
    pub uploaded: i64,
    pub downloaded: i64,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    /// The source application ("" = qBittorrent).
    pub source_app: String,
    /// Transmission's remote access (RPC) was on; its password is never copied.
    pub remote_access_enabled: bool,
    pub dry_run: bool,
    pub profile: String,
    pub sources: Vec<String>,
    pub prefs_source: String,
    pub prefs_notes: Vec<String>,
    /// key, theirs, ours, reason
    pub prefs_diff: Vec<(String, String, String, String)>,
    /// reason -> ini keys
    pub ini_unused: BTreeMap<String, Vec<String>>,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    pub watched_folders: usize,
    pub rss_feeds: usize,
    pub rss_rules: usize,
    pub rss_article_files: usize,
    pub search_plugins: Vec<String>,
    pub disabled_plugins: Vec<String>,
    pub gui_keys: Vec<String>,
    pub stats: Option<(i64, i64)>,
    pub torrents: Vec<TorrentSummary>,
    pub failed_torrents: Vec<(String, String)>,
    pub skipped_files_created: Vec<String>,
    pub warnings: Vec<String>,
    pub sandbox: Option<Sandbox>,
    /// Files written into the profile (relative paths).
    pub files_written: Vec<String>,
}

pub(crate) fn read_opt(path: &Path, limit: u64) -> Option<Vec<u8>> {
    resume::read_limited(path, limit).ok()
}

/// Same file found in either folder (`categories.json` & co. live in the
/// preferences folder on macOS, next to the ini elsewhere).
fn find_file(opts: &Options, rel: &str) -> Option<PathBuf> {
    [&opts.source_prefs, &opts.source_config].iter().map(|d| d.join(rel)).find(|p| p.is_file())
}

/// Canonical form of a path that may not exist yet: its deepest existing
/// ancestor resolved (symbolic links, `/var` -> `/private/var`), the rest kept.
pub(crate) fn canonical(p: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        if let Ok(c) = std::fs::canonicalize(&cur) {
            return rest.iter().rev().fold(c, |acc, part: &std::ffi::OsString| acc.join(part));
        }
        match (cur.file_name().map(|n| n.to_owned()), cur.parent().map(Path::to_path_buf)) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                cur = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

pub(crate) fn is_inside(child: &Path, parent: &Path) -> bool {
    let (c, p) = (canonical(child), canonical(parent));
    c.starts_with(&p)
}

/// GET `<base><path>` from qBittorrent's WebUI (localhost bypass or no
/// authentication needed); never sends credentials.
fn webui_get(base: &str, path: &str) -> Result<Value, String> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client.get(&url).send().await.map_err(|e| format!("GET {path}: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("GET {path}: HTTP {}", status.as_u16()));
        }
        let text = resp.text().await.map_err(|e| e.to_string())?;
        serde_json::from_str(&text).or_else(|_| Ok(Value::String(text)))
    })
}

pub(crate) const MAX_CONFIG: u64 = 16 * 1024 * 1024;

pub(crate) struct Writer {
    pub(crate) root: PathBuf,
    pub(crate) dry_run: bool,
    pub(crate) merge: bool,
    pub(crate) written: Vec<String>,
}

impl Writer {
    /// In merge mode, files the profile already has are kept.
    pub(crate) fn keeps(&self, rel: &str) -> bool {
        self.merge && self.root.join(rel).exists()
    }

    pub(crate) fn write_new(&mut self, rel: &str, data: &[u8]) -> Result<(), String> {
        if self.keeps(rel) { Ok(()) } else { self.write(rel, data) }
    }

    pub(crate) fn write(&mut self, rel: &str, data: &[u8]) -> Result<(), String> {
        self.written.push(rel.to_owned());
        if self.dry_run {
            return Ok(());
        }
        // The engine's file size/mtime record (`<hash>.fstat`) belongs to the
        // bitfield it was saved with: a replaced bitfield drops it, so the new
        // one is validated by sampling pieces.
        if let Some(stem) = rel.strip_suffix(".bitv") {
            let _ = std::fs::remove_file(self.root.join(format!("{stem}.fstat")));
        }
        resume::write_private_atomic(&self.root.join(rel), data).map_err(|e| format!("{rel}: {e}"))
    }

    pub(crate) fn copy(&mut self, from: &Path, rel: &str, limit: u64) -> Result<(), String> {
        let data = resume::read_limited(from, limit).map_err(|e| format!("{}: {e}", from.display()))?;
        self.write(rel, &data)
    }
}

/// Runs the migration (or only plans it with `dry_run`).
pub fn run(opts: &Options) -> Result<Report, String> {
    let mut rep = Report { dry_run: opts.dry_run, profile: opts.profile.display().to_string(), sandbox: opts.sandbox.clone(), ..Default::default() };

    // ---- safety checks
    for src in [&opts.source_config, &opts.source_prefs, &opts.source_data] {
        if src.as_os_str().is_empty() {
            continue;
        }
        if is_inside(&opts.profile, src) || (opts.profile.exists() && is_inside(src, &opts.profile)) {
            return Err(format!("the profile folder must be separate from qBittorrent's folder {}", src.display()));
        }
    }
    if let Some(who) = profile_in_use(&opts.profile) {
        let msg = format!("the profile is in use by {who}: quit it first");
        if opts.dry_run {
            rep.warnings.push(msg);
        } else {
            return Err(msg);
        }
    }
    if opts.merge {
        if !opts.profile.join("config/preferences.json").is_file() {
            return Err(format!("{} has no qBit NeoRS profile to merge into; migrate without --merge", opts.profile.display()));
        }
    } else if !opts.dry_run && !opts.force {
        let prefs = opts.profile.join("config/preferences.json");
        let bt = opts.profile.join("data/BT_backup");
        let has_torrents = std::fs::read_dir(&bt).map(|mut d| d.next().is_some()).unwrap_or(false);
        if prefs.exists() || has_torrents {
            return Err(format!(
                "{} already holds a qBit NeoRS profile; move it away first (or pass --force to write over it)",
                opts.profile.display()
            ));
        }
    }

    // ---- configuration sources
    let ini_path = opts.source_config.join("qBittorrent.ini");
    let ini = match read_opt(&ini_path, MAX_CONFIG) {
        Some(d) => {
            rep.sources.push(format!("settings: {}", ini_path.display()));
            IniFile::parse(&d)
        }
        None => {
            rep.warnings.push(format!("{} not found: settings that only the ini holds (WebUI password, GUI) are not migrated", ini_path.display()));
            IniFile::default()
        }
    };
    let data_ini = read_opt(&opts.source_config.join("qBittorrent-data.ini"), MAX_CONFIG).map(|d| IniFile::parse(&d)).unwrap_or_default();

    let (ini_api, mut ini_used) = prefs_map::api_json_from_ini(&ini);
    let mut live: Option<Map<String, Value>> = None;
    let mut qbt_torrents: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(file) = &opts.webui_prefs_file {
        let data = resume::read_limited(file, MAX_CONFIG).map_err(|e| format!("{}: {e}", file.display()))?;
        match serde_json::from_slice::<Value>(&data) {
            Ok(Value::Object(m)) => {
                rep.prefs_source = format!("saved WebAPI preferences {}", file.display());
                live = Some(m);
            }
            _ => return Err(format!("{} is not an app/preferences JSON object", file.display())),
        }
    } else if let Some(base) = &opts.webui {
        match webui_get(base, "/api/v2/app/preferences") {
            Ok(Value::Object(m)) => {
                let version = webui_get(base, "/api/v2/app/version").ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
                rep.prefs_source = format!("live WebAPI {base} (qBittorrent {version})");
                live = Some(m);
                if let Ok(Value::Array(list)) = webui_get(base, "/api/v2/torrents/info") {
                    for t in list {
                        if let Some(h) = t.get("hash").and_then(Value::as_str) {
                            qbt_torrents.insert(h.to_ascii_lowercase(), t.clone());
                        }
                    }
                }
            }
            Ok(_) => rep.warnings.push("the WebAPI answered without a preferences object (login required?); using qBittorrent.ini".into()),
            Err(e) => rep.warnings.push(format!("WebAPI not reachable ({e}); using qBittorrent.ini")),
        }
    }
    if live.is_none() {
        rep.prefs_source = format!("{} ({} keys)", ini_path.display(), ini_api.len());
    }
    let source_api = live.clone().unwrap_or_else(|| ini_api.clone());

    let watched = match find_file(opts, "watched_folders.json") {
        Some(p) => match read_opt(&p, MAX_CONFIG).map(|d| prefs_map::parse_watched_folders(&d)) {
            Some(Ok(w)) => Some(w),
            Some(Err(e)) => {
                rep.warnings.push(format!("{}: {e}; watched folders taken from the scan_dirs preference", p.display()));
                None
            }
            None => None,
        },
        None => None,
    };
    let built = prefs_map::build(&source_api, &ini, watched, &opts.source_data.to_string_lossy())?;
    let mut prefs = built.prefs;
    rep.prefs_notes = built.notes;
    rep.watched_folders = prefs.downloads.watched_folders.len();

    let target_prefs: Option<Preferences> = if opts.merge {
        let data = resume::read_limited(&opts.profile.join("config/preferences.json"), MAX_CONFIG).map_err(|e| e.to_string())?;
        Some(serde_json::from_slice(&data).map_err(|e| format!("config/preferences.json: {e}"))?)
    } else {
        None
    };
    // Preferences as our WebAPI reports them, against qBittorrent's view
    // (merge: the profile's own settings, which are kept).
    let ours = PreferencesStore::in_memory(target_prefs.clone().unwrap_or_else(|| prefs.clone())).to_api_value();
    let ours = ours.as_object().cloned().unwrap_or_default();
    let theirs: Map<String, Value> = if live.is_some() { source_api.clone() } else { ini_api.clone() };
    for (k, a, b) in prefs_map::diff(&theirs, &ours) {
        if live.is_none() && !theirs.contains_key(&k) {
            continue;
        }
        let reason = prefs_map::known_difference(&k).map(str::to_owned).unwrap_or_else(|| {
            if !ours.contains_key(&k) {
                "not part of qBit NeoRS' WebAPI".into()
            } else if !theirs.contains_key(&k) {
                "not reported by this qBittorrent version".into()
            } else {
                "value normalised or not supported by qBit NeoRS (check)".into()
            }
        });
        rep.prefs_diff.push((k, a, b, reason));
    }

    // ---- categories and tags
    let mut cats: CategoryMap = match find_file(opts, "categories.json") {
        Some(p) => match read_opt(&p, MAX_CONFIG).map(|d| categories::from_json(&d)) {
            Some(Ok(c)) => c,
            Some(Err(e)) => {
                rep.warnings.push(format!("{}: {e}; categories not migrated", p.display()));
                CategoryMap::new()
            }
            None => CategoryMap::new(),
        },
        None => CategoryMap::new(),
    };
    let marker_path = opts.profile.join("migration/qbittorrent-merge.json");
    let mut marker: MergeMarker = if opts.merge { resume::load_json(&marker_path) } else { MergeMarker::default() };
    if opts.merge
        && let Ok(d) = resume::read_limited(&opts.profile.join("config/categories.json"), MAX_CONFIG)
        && let Ok(existing) = categories::from_json(&d)
    {
        // The profile's categories win; qBittorrent's missing ones are added.
        for (k, v) in existing {
            cats.insert(k, v);
        }
    }
    let target_tags: Vec<String> = if opts.merge { resume::load_json(&opts.profile.join("config/tags.json")) } else { Vec::new() };
    let mut tags: BTreeSet<String> = target_tags.into_iter().collect();
    tags.extend(ini.string_list("BitTorrent/Session/Tags").unwrap_or_default().into_iter().filter(|t| categories::is_valid_tag(t)));
    ini_used.push("BitTorrent/Session/Tags".into());

    // ---- torrents
    let bt_dir = opts.source_data.join("BT_backup");
    let mut writer = Writer { root: opts.profile.clone(), dry_run: opts.dry_run, merge: opts.merge, written: Vec::new() };
    let queue = read_opt(&bt_dir.join("queue"), MAX_CONFIG).map(|d| libtorrent::parse_queue(&d)).unwrap_or_default();
    let mut ids: Vec<String> = std::fs::read_dir(&bt_dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".fastresume")).map(str::to_ascii_lowercase))
                .filter(|id| id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit()))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    if ids.is_empty() && opts.source_data.join("torrents.db").is_file() {
        rep.warnings.push(
            "qBittorrent keeps its torrents in torrents.db (SQLite resume data storage), which is not supported: switch \
             Advanced > Resume data storage type to \"Fastresume files\", restart and quit qBittorrent, then migrate again"
                .into(),
        );
    }
    let limits = Limits {
        max_depth: (prefs.advanced.bdecode_depth_limit.max(100)) as usize,
        max_tokens: (prefs.advanced.bdecode_token_limit.max(10_000_000)) as usize,
    };
    // An unset default save path is ~/Downloads (what the core resolves at start).
    let default_save = if prefs.downloads.save_path.is_empty() {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Downloads").to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        prefs.downloads.save_path.clone()
    };
    let global_paths = GlobalPaths {
        save_path: default_save,
        download_path: prefs.downloads.temp_path.clone(),
        download_path_enabled: prefs.downloads.temp_path_enabled,
    };
    let backup = BackupDir::new(opts.profile.join("data/BT_backup"));
    for id in &ids {
        if opts.merge && backup.resume_path(id).is_file() {
            let already = marker.torrents.contains(id);
            match merge_torrent(opts, &bt_dir, id, &queue, limits, &backup, already) {
                Ok(sum) => {
                    if !already {
                        marker.torrents.push(id.clone());
                    }
                    rep.torrents.push(sum);
                }
                Err(e) => rep.failed_torrents.push((id.clone(), e)),
            }
            continue;
        }
        match migrate_torrent(opts, &bt_dir, id, &queue, limits, &prefs, &cats, &global_paths, &mut writer, &backup) {
            Ok((mut sum, created)) => {
                if let Some(t) = qbt_torrents.get(id) {
                    sum.qbt_progress = t.get("progress").and_then(Value::as_f64);
                }
                for t in &sum.tags {
                    tags.insert(t.clone());
                }
                if !sum.category.is_empty() && !cats.contains_key(&sum.category) {
                    rep.warnings.push(format!("category \"{}\" of a torrent was missing from categories.json; added with default settings", sum.category));
                    for c in categories::expand_category(&sum.category) {
                        cats.entry(c.clone()).or_insert_with(|| categories::new_category(&c));
                    }
                }
                rep.skipped_files_created.extend(created);
                rep.torrents.push(sum);
            }
            Err(e) => rep.failed_torrents.push((id.clone(), e)),
        }
    }
    rep.categories = cats.keys().cloned().collect();
    rep.tags = tags.iter().cloned().collect();
    writer.write("config/categories.json", &categories::to_json(&cats))?;
    let tag_list: Vec<&String> = tags.iter().collect();
    writer.write("config/tags.json", &serde_json::to_vec_pretty(&tag_list).map_err(|e| e.to_string())?)?;

    // ---- RSS
    if let Some(p) = find_file(opts, "rss/feeds.json") {
        let data = read_opt(&p, MAX_CONFIG).unwrap_or_default();
        rep.rss_feeds = count_feeds(&serde_json::from_slice(&data).unwrap_or(Value::Null));
        writer.write_new("data/rss/feeds.json", &data)?;
    }
    if let Some(p) = find_file(opts, "rss/download_rules.json") {
        let data = read_opt(&p, MAX_CONFIG).unwrap_or_default();
        rep.rss_rules = serde_json::from_slice::<Value>(&data).ok().and_then(|v| v.as_object().map(Map::len)).unwrap_or(0);
        writer.write_new("data/rss/download_rules.json", &data)?;
    }
    let articles = opts.source_data.join("rss/articles");
    if let Ok(rd) = std::fs::read_dir(&articles) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let ok_name = name.ends_with(".json") || name.ends_with(".ico") || name.ends_with(".png");
            if ok_name && !name.starts_with('.') && !name.contains('/') && e.path().is_file() {
                let rel = format!("data/rss/articles/{name}");
                if !writer.keeps(&rel) {
                    writer.copy(&e.path(), &rel, 64 * 1024 * 1024)?;
                }
                rep.rss_article_files += 1;
            }
        }
    }

    // ---- search plugins
    let engines = opts.source_data.join("nova3/engines");
    if let Ok(rd) = std::fs::read_dir(&engines) {
        let mut names: Vec<String> = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some((stem, ext)) = name.rsplit_once('.') else { continue };
            let valid = !stem.is_empty() && stem != "__init__" && stem.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            if valid && matches!(ext, "py" | "png" | "ico") && e.path().is_file() {
                let rel = format!("data/nova3/engines/{name}");
                if !writer.keeps(&rel) {
                    writer.copy(&e.path(), &rel, 8 * 1024 * 1024)?;
                }
                if ext == "py" {
                    names.push(stem.to_owned());
                }
            }
        }
        names.sort();
        rep.search_plugins = names;
    }
    rep.disabled_plugins = ini.string_list("SearchEngines/disabledEngines").unwrap_or_default();
    ini_used.push("SearchEngines/disabledEngines".into());
    let search_cfg = serde_json::json!({ "disabled_engines": rep.disabled_plugins });
    writer.write_new("config/search.json", &serde_json::to_vec_pretty(&search_cfg).map_err(|e| e.to_string())?)?;

    // ---- statistics, session state
    if let Some(QValue::Variant(Variant::Map(m))) = data_ini.get("Stats/AllStats") {
        let get = |k: &str| match m.get(k) {
            Some(Variant::Int(i)) => *i,
            _ => 0,
        };
        let mut stats = StatsFile { alltime_dl: get("AlltimeDL").max(0), alltime_ul: get("AlltimeUL").max(0) };
        rep.stats = Some((stats.alltime_dl, stats.alltime_ul));
        if opts.merge {
            // qBittorrent's totals are added once to what the profile counted since.
            let mine: StatsFile = resume::load_json(&opts.profile.join("data/stats.json"));
            if marker.stats_added {
                stats = mine;
            } else {
                stats.alltime_dl += mine.alltime_dl.max(0);
                stats.alltime_ul += mine.alltime_ul.max(0);
                marker.stats_added = true;
            }
        }
        writer.write("data/stats.json", &serde_json::to_vec_pretty(&stats).map_err(|e| e.to_string())?)?;
    }
    let state = SessionStateFile { use_alt_speed_limits: ini.bool("BitTorrent/Session/UseAlternativeGlobalSpeedLimit").unwrap_or(false) };
    ini_used.push("BitTorrent/Session/UseAlternativeGlobalSpeedLimit".into());
    writer.write_new("data/session_state.json", &serde_json::to_vec_pretty(&state).map_err(|e| e.to_string())?)?;

    // ---- GUI settings
    let history = read_opt(&opts.source_data.join("SearchUI/History.txt"), MAX_CONFIG).map(|d| String::from_utf8_lossy(&d).into_owned());
    let (gui_values, gui_used) = gui::collect(&ini, history.as_deref());
    rep.gui_keys = gui_values.iter().map(|(k, _)| k.clone()).collect();
    ini_used.extend(gui_used);
    // Merge: the profile's GUI settings stay; the plist is only offered.
    let plist_rel = if opts.merge || opts.gui_plist_in_migration { "migration/gui-preferences.plist" } else { "config/gui-preferences.plist" };
    writer.write(plist_rel, gui::to_plist(&gui_values).as_bytes())?;
    let mut keys = rep.gui_keys.join("\n");
    keys.push('\n');
    writer.write("migration/gui-preferences.keys", keys.as_bytes())?;

    // ---- unmapped ini keys
    ini_used.extend(prefs_map::EXTRA_KEYS.iter().map(|k| k.to_string()));
    let used: BTreeSet<&str> = ini_used.iter().map(String::as_str).collect();
    for k in ini.values.keys() {
        if !used.contains(k.as_str()) {
            rep.ini_unused.entry(prefs_map::classify_unused(k).to_owned()).or_default().push(k.clone());
        }
    }

    // ---- sandbox, preferences
    if let Some(t) = target_prefs {
        // Merge keeps the profile's settings (a sandbox still applies).
        prefs = t;
        if let Some(sb) = &opts.sandbox {
            apply_sandbox(&mut prefs, sb);
            writer.write("config/preferences.json", &serde_json::to_vec_pretty(&prefs).map_err(|e| e.to_string())?)?;
        }
        writer.write("migration/qbittorrent-merge.json", &serde_json::to_vec_pretty(&marker).map_err(|e| e.to_string())?)?;
    } else {
        if let Some(sb) = &opts.sandbox {
            apply_sandbox(&mut prefs, sb);
        }
        writer.write("config/preferences.json", &serde_json::to_vec_pretty(&prefs).map_err(|e| e.to_string())?)?;
    }

    writer.written.push("migration/report.txt".into());
    rep.files_written = writer.written.clone();
    let text = rep.to_text();
    if !opts.dry_run {
        resume::write_private_atomic(&opts.profile.join("migration/report.txt"), text.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(rep)
}

/// Copies the small files of an existing profile (`config/`,
/// `data/BT_backup`, `data/engine/fastresume`, `data/stats.json`,
/// `data/session_state.json`) to a new sibling folder
/// `<profile>.before-migration-<time>` before a migration writes into it.
/// Returns that folder, or None when there is no profile yet. Content files
/// are never part of a profile, so none are copied.
pub fn backup_profile(profile: &Path) -> Result<Option<PathBuf>, String> {
    let parts = ["config", "data/BT_backup", "data/engine/fastresume", "data/stats.json", "data/session_state.json", "migration"];
    if !parts.iter().any(|p| profile.join(p).exists()) {
        return Ok(None);
    }
    let name = profile.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "profile".into());
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let parent = profile.parent().unwrap_or(Path::new("."));
    let mut dest = parent.join(format!("{name}.before-migration-{stamp}"));
    let mut n = 1;
    while dest.exists() {
        n += 1;
        dest = parent.join(format!("{name}.before-migration-{stamp}-{n}"));
    }
    for p in parts {
        let from = profile.join(p);
        if from.exists() {
            copy_tree(&from, &dest.join(p), 0).map_err(|e| format!("backup of {p}: {e}"))?;
        }
    }
    Ok(Some(dest))
}

/// Copies files and folders; symbolic links are skipped, never followed.
fn copy_tree(from: &Path, to: &Path, depth: usize) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() || depth > 16 {
        return Ok(());
    }
    if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)?.flatten() {
            copy_tree(&e.path(), &to.join(e.file_name()), depth + 1)?;
        }
        Ok(())
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to).map(|_| ())
    }
}

/// What a merge already imported (`migration/qbittorrent-merge.json`), so
/// running it again never counts qBittorrent's statistics twice.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct MergeMarker {
    stats_added: bool,
    torrents: Vec<String>,
}

/// A running qBit NeoRS (app or daemon) using `profile`, if any: a process
/// started with that `--profile` (or without one, for the default
/// profile), or one holding files of the profile's engine folder open.
pub fn profile_in_use(profile: &Path) -> Option<String> {
    let target = canonical(profile);
    let default = std::env::var_os("HOME").map(|h| canonical(&PathBuf::from(h).join("Library/Application Support/qBit NeoRS")));
    let me = std::process::id().to_string();
    if let Ok(out) = std::process::Command::new("/bin/ps").args(["-axww", "-o", "pid=,args="]).output() {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let Some((pid, args)) = line.trim().split_once(' ') else { continue };
            if pid == me {
                continue;
            }
            let is_neo = (args.contains("qbit-neors-nox") && !args.contains("migrate-from-qbittorrent") && !args.contains("migrate-from-transmission"))
                || args.contains("/Contents/MacOS/qBit NeoRS")
                || args.contains("/Contents/MacOS/QBTNeo");
            if !is_neo {
                continue;
            }
            let used = match args.split_once("--profile") {
                // The path may contain spaces: compare as a prefix.
                Some((_, rest)) => {
                    let rest = rest.trim_start_matches('=').trim_start();
                    rest.starts_with(&*target.to_string_lossy()) || rest.starts_with(&*profile.to_string_lossy())
                }
                None => default.as_ref() == Some(&target),
            };
            if used {
                return Some(format!("process {pid} ({})", args.chars().take(80).collect::<String>()));
            }
        }
    }
    let engine = profile.join("data/engine");
    if engine.is_dir()
        && let Ok(out) = std::process::Command::new("/usr/sbin/lsof").arg("-t").arg("+D").arg(&engine).output()
    {
        let pids: Vec<&str> = std::str::from_utf8(&out.stdout).unwrap_or("").lines().map(str::trim).filter(|p| !p.is_empty() && *p != me).collect();
        if !pids.is_empty() {
            return Some(format!("process {} (has files of the profile open)", pids.join(", ")));
        }
    }
    None
}

/// Merges qBittorrent's data of a torrent the profile already has: the
/// statistics (added once), dates, tracker tiers, web seeds, limits left
/// at their defaults and the queue position. Paths, category, tags, file
/// priorities, state and pieces stay as they are (no recheck).
fn merge_torrent(
    opts: &Options,
    bt_dir: &Path,
    id: &str,
    queue: &[String],
    limits: Limits,
    backup: &BackupDir,
    already_merged: bool,
) -> Result<TorrentSummary, String> {
    let data = resume::read_limited(&bt_dir.join(format!("{id}.fastresume")), 64 * 1024 * 1024).map_err(|e| e.to_string())?;
    let lt = LtResume::parse(&data, limits)?;
    let parsed = read_opt(&bt_dir.join(format!("{id}.torrent")), 1 << 30).and_then(|b| metainfo::parse_torrent(&b, limits).ok());
    let meta = parsed.as_ref().map(|p| &p.meta);
    let queue_position = queue.iter().position(|q| q == id).map(|p| p as i64).or(lt.queue_position).unwrap_or(-1);
    let (q, _) = libtorrent::to_fast_resume(&lt, id, meta, None, queue_position);
    let mine = resume::read_limited(&backup.resume_path(id), 64 * 1024 * 1024).map_err(|e| e.to_string())?;
    let mut r: resume::FastResume = serde_json::from_slice(&mine).map_err(|e| format!("profile record: {e}"))?;
    let mut notes = Vec::new();

    if already_merged {
        notes.push("statistics already merged by an earlier run".into());
    } else {
        r.all_time_upload = r.all_time_upload.max(0) + q.all_time_upload;
        r.all_time_download = r.all_time_download.max(0) + q.all_time_download;
        r.active_time = r.active_time.max(0) + q.active_time;
        r.seeding_time = r.seeding_time.max(0) + q.seeding_time;
        notes.push("qBittorrent's uploaded/downloaded totals, active and seeding time added".into());
    }
    if q.added_on > 0 {
        r.added_on = q.added_on;
    }
    if q.completed_on > 0 {
        r.completed_on = q.completed_on;
    }
    r.seen_complete = r.seen_complete.max(q.seen_complete);
    r.last_upload = r.last_upload.max(q.last_upload);
    r.last_download = r.last_download.max(q.last_download);

    // qBittorrent's tiers, then the trackers only the profile has.
    let next_tier = q.trackers.iter().map(|t| t.tier + 1).max().unwrap_or(0);
    let extra: Vec<_> = r.trackers.iter().filter(|t| !q.trackers.iter().any(|x| x.url == t.url)).cloned().collect();
    let mut trackers = q.trackers.clone();
    for (i, mut t) in extra.into_iter().enumerate() {
        t.tier = next_tier + i as i64;
        trackers.push(t);
    }
    r.trackers = trackers;
    for u in &q.url_seeds {
        if !r.url_seeds.contains(u) {
            r.url_seeds.push(u.clone());
        }
    }
    for u in &q.trusted_urls {
        if !r.trusted_urls.contains(u) {
            r.trusted_urls.push(u.clone());
        }
    }
    if r.download_limit <= 0 {
        r.download_limit = q.download_limit;
    }
    if r.upload_limit <= 0 {
        r.upload_limit = q.upload_limit;
    }
    if r.ratio_limit == -2.0 {
        r.ratio_limit = q.ratio_limit;
    }
    if r.seeding_time_limit == -2 {
        r.seeding_time_limit = q.seeding_time_limit;
    }
    if r.inactive_seeding_time_limit == -2 {
        r.inactive_seeding_time_limit = q.inactive_seeding_time_limit;
    }
    if r.share_limit_action == "Default" {
        r.share_limit_action = q.share_limit_action.clone();
    }
    r.queue_position = queue_position;
    if opts.stop_all {
        r.stopped = true;
    }
    // Differences left alone, for the owner to check.
    if r.category != q.category {
        notes.push(format!("category differs (profile \"{}\", qBittorrent \"{}\"): kept", r.category, q.category));
    }
    let (mut a, mut b) = (r.tags.clone(), q.tags.clone());
    a.sort();
    b.sort();
    if a != b {
        notes.push(format!("tags differ (profile [{}], qBittorrent [{}]): kept", r.tags.join(", "), q.tags.join(", ")));
    }
    if r.auto_tmm != q.auto_tmm || (!r.auto_tmm && r.save_path.trim_end_matches('/') != q.save_path.trim_end_matches('/')) {
        let show = |auto: bool, p: &str| if auto { "automatic".to_owned() } else { p.to_owned() };
        notes.push(format!("save path differs (profile {}, qBittorrent {}): kept", show(r.auto_tmm, &r.save_path), show(q.auto_tmm, &q.save_path)));
    }
    if !q.file_priorities.is_empty() && r.file_priorities != q.file_priorities {
        notes.push("file priorities differ: kept".into());
    }
    if !q.file_paths.is_empty() && r.file_paths != q.file_paths {
        notes.push("file paths differ: kept".into());
    }
    if !opts.dry_run {
        backup.save_resume(&r).map_err(|e| e.to_string())?;
    }
    Ok(TorrentSummary {
        id: id.to_owned(),
        name: meta.map(|m| m.name.clone()).unwrap_or_default(),
        state: format!("merged ({})", if r.stopped { "stopped" } else { "state kept" }),
        category: r.category.clone(),
        tags: r.tags.clone(),
        save_path: if r.auto_tmm { "(automatic)".into() } else { r.save_path.clone() },
        auto_tmm: r.auto_tmm,
        location: r.location.clone(),
        files: r.file_paths.len(),
        skipped_files: r.file_priorities.iter().filter(|p| **p == 0).count(),
        trackers: r.trackers.len(),
        web_seeds: r.url_seeds.len(),
        pieces: "pieces kept (no recheck)".into(),
        uploaded: r.all_time_upload,
        downloaded: r.all_time_download,
        notes,
        ..Default::default()
    })
}

/// Settings of a test run: no announces or discovery, nothing changes
/// outside the profile, WebUI on localhost.
pub fn apply_sandbox(p: &mut Preferences, sb: &Sandbox) {
    p.connection.listen_port = i64::from(sb.listen_port);
    p.connection.upnp = false;
    p.connection.i2p_enabled = false;
    p.bittorrent.dht = false;
    p.bittorrent.pex = false;
    p.bittorrent.lsd = false;
    p.bittorrent.add_trackers_from_url_enabled = false;
    p.webui.web_ui_enabled = true;
    p.webui.web_ui_port = i64::from(sb.webui_port);
    p.webui.web_ui_address = "127.0.0.1".into();
    p.webui.web_ui_upnp = false;
    p.webui.dyndns_enabled = false;
    p.rss.rss_processing_enabled = false;
    p.rss.rss_auto_downloading_enabled = false;
    p.downloads.mail_notification_enabled = false;
    p.downloads.autorun_enabled = false;
    p.downloads.autorun_on_torrent_added_enabled = false;
    p.downloads.export_dir.clear();
    p.downloads.export_dir_fin.clear();
    p.downloads.watched_folders.clear();
    p.advanced.resolve_peer_countries = false;
    p.advanced.enable_embedded_tracker = false;
    p.behavior.file_log_path.clear();
}

fn count_feeds(v: &Value) -> usize {
    match v {
        Value::Object(m) => m.values().map(|x| if x.get("url").is_some() || x.is_string() { 1 } else { count_feeds(x) }).sum(),
        _ => 0,
    }
}

#[allow(clippy::too_many_arguments)]
fn migrate_torrent(
    opts: &Options,
    bt_dir: &Path,
    id: &str,
    queue: &[String],
    limits: Limits,
    prefs: &Preferences,
    cats: &CategoryMap,
    paths: &GlobalPaths,
    writer: &mut Writer,
    backup: &BackupDir,
) -> Result<(TorrentSummary, Vec<String>), String> {
    let data = resume::read_limited(&bt_dir.join(format!("{id}.fastresume")), 64 * 1024 * 1024).map_err(|e| e.to_string())?;
    let lt = LtResume::parse(&data, limits)?;
    let torrent_bytes = read_opt(&bt_dir.join(format!("{id}.torrent")), prefs.advanced.torrent_file_size_limit.clamp(1024, 1 << 30) as u64);
    let parsed = match &torrent_bytes {
        Some(b) => Some(metainfo::parse_torrent(b, limits).map_err(|e| format!("Cannot parse torrent info: {e}"))?),
        None => None,
    };
    let meta = parsed.as_ref().map(|p| &p.meta);
    if let Some(m) = meta
        && m.id() != id
    {
        return Err("Mismatching info-hash detected in resume data".into());
    }
    if meta.is_none() && lt.info_hash.is_none() {
        return Err("Resume data is invalid: neither metadata nor info-hash was found".into());
    }
    let queue_position = queue.iter().position(|q| q == id).map(|p| p as i64).or(lt.queue_position).unwrap_or(-1);
    let (mut fr, mut notes) = libtorrent::to_fast_resume(&lt, id, meta, None, queue_position);
    if opts.stop_all {
        fr.stopped = true;
        fr.operating_mode = "AutoManaged".into();
    }

    let mut sum = TorrentSummary {
        id: id.to_owned(),
        name: if !fr.name.is_empty() { fr.name.clone() } else { meta.map(|m| m.name.clone()).unwrap_or_else(|| fr.magnet_name.clone()) },
        state: if fr.stopped {
            "stopped".into()
        } else if fr.operating_mode == "Forced" {
            "forced".into()
        } else {
            "started".into()
        },
        category: fr.category.clone(),
        tags: fr.tags.clone(),
        auto_tmm: fr.auto_tmm,
        save_path: if fr.auto_tmm { categories::category_save_path(cats, &fr.category, paths) } else { fr.save_path.clone() },
        location: lt.save_path.clone(),
        trackers: fr.trackers.len(),
        web_seeds: fr.url_seeds.len(),
        uploaded: fr.all_time_upload,
        downloaded: fr.all_time_download,
        ..Default::default()
    };
    if !lt.http_seeds.is_empty() {
        notes.push(format!("{} BEP 17 HTTP seed(s) not supported, dropped", lt.http_seeds.len()));
    }
    if lt.unfinished_pieces > 0 {
        notes.push(format!("{} partially downloaded piece(s) start over", lt.unfinished_pieces));
    }
    if lt.has_piece_priority {
        notes.push("custom piece priorities not carried over (file priorities are)".into());
    }
    if lt.has_ssl_parameters {
        notes.push("per-torrent SSL parameters not carried over".into());
    }
    if lt.upload_mode {
        notes.push("was in upload-only mode after an error; starts normally".into());
    }
    if lt.super_seeding {
        notes.push("super seeding on".into());
    }

    let mut created = Vec::new();
    if let (Some(m), Some(p)) = (meta, &parsed) {
        sum.files = m.files.len();
        sum.skipped_files = fr.file_priorities.iter().filter(|p| **p == 0).count();
        let location = PathBuf::from(&lt.save_path);
        match lt.have_pieces(m.pieces) {
            Ok(mut have) => {
                let v1_hashes = if m.is_pure_v2() {
                    Vec::new()
                } else {
                    bencode::decode(&p.info_bytes, limits).ok().and_then(|i| i.get("pieces").and_then(|x| x.as_bytes().map(<[u8]>::to_vec))).unwrap_or_default()
                };
                let part_file = match opts.skipped_data {
                    SkippedData::Recover => location.join(format!(".{id}.parts")),
                    SkippedData::Unclaim => PathBuf::new(),
                };
                let input = partfile::Input {
                    meta: m,
                    file_paths: &fr.file_paths,
                    priorities: &fr.file_priorities,
                    location: &location,
                    part_file: &part_file,
                    v1_hashes: &v1_hashes,
                    incomplete_suffix: prefs.downloads.incomplete_files_ext,
                };
                let rec = partfile::plan(&input, &mut have);
                if !rec.recovered.is_empty() {
                    notes.push(format!(
                        "{} piece(s) shared with skipped files rebuilt from libtorrent's part file and verified: {} new file(s), {} bytes{}",
                        rec.recovered.len(),
                        rec.writes.len(),
                        rec.bytes(),
                        if opts.dry_run { " (would be written)" } else { "" }
                    ));
                }
                if !rec.unclaimed.is_empty() {
                    let reason = match opts.skipped_data {
                        SkippedData::Unclaim => "--skipped-file-data unclaim".to_owned(),
                        SkippedData::Recover => rec.unclaimed[0].1.clone(),
                    };
                    notes.push(format!("{} piece(s) shared with skipped files unclaimed ({reason}); they are downloaded again", rec.unclaimed.len()));
                }
                for w in &rec.writes {
                    created.push(w.path.display().to_string());
                }
                if !opts.dry_run {
                    partfile::apply(&rec.writes)?;
                }
                fr.completed_bytes = libtorrent::completed_wanted_bytes(m, &fr.file_priorities, &have);
                let progress = libtorrent::wanted_progress(m, &fr.file_priorities, &have);
                sum.progress = Some(progress);
                sum.pieces = format!("bitfield imported: {}/{} pieces", have.iter().filter(|h| **h).count(), have.len());
                let missing: Vec<&str> = m
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(i, f)| {
                        fr.file_priorities.get(*i).copied().unwrap_or(1) != 0
                            && f.size > 0
                            && (f.first_piece..=f.last_piece).any(|pc| have.get(pc as usize).copied().unwrap_or(false))
                            && partfile::existing_file(&location, &fr.file_paths[*i]).is_none()
                    })
                    .map(|(i, _)| fr.file_paths[i].as_str())
                    .collect();
                if !missing.is_empty() && !location.is_dir() {
                    notes.push(format!("data folder {} not found (drive not mounted?): shown as \"Missing Files\"", lt.save_path));
                } else if !missing.is_empty() {
                    notes.push(format!("{} wanted file(s) with data are missing on disk: shown as \"Missing Files\"", missing.len()));
                }
                writer.write(&format!("data/engine/fastresume/{}.bitv", m.engine_hash_hex()), &libtorrent::engine_bitfield(&have))?;
            }
            Err(reason) => {
                sum.pieces = format!("full recheck: {reason}");
            }
        }
        if !opts.dry_run {
            backup.save_torrent_file(id, torrent_bytes.as_deref().unwrap_or_default()).map_err(|e| e.to_string())?;
        }
        writer.written.push(format!("data/BT_backup/{id}.torrent"));
    } else {
        sum.pieces = "no metadata yet (magnet): fetched from peers".into();
    }
    if !fr.auto_tmm || fr.category.is_empty() || !fr.seed_status {
        // nothing to compare
    } else if sum.save_path.trim_end_matches('/') != lt.save_path.trim_end_matches('/') {
        notes.push(format!(
            "data is in \"{}\", not in its category folder \"{}\"; it stays where it is",
            lt.save_path, sum.save_path
        ));
    }
    if !opts.dry_run {
        backup.save_resume(&fr).map_err(|e| e.to_string())?;
    }
    writer.written.push(format!("data/BT_backup/{id}.fastresume"));
    sum.notes = notes;
    Ok((sum, created))
}

impl Report {
    /// "qBittorrent" or "Transmission".
    pub fn source_name(&self) -> &str {
        if self.source_app.is_empty() { "qBittorrent" } else { &self.source_app }
    }

    /// Human-readable report. Never contains secrets (no password, API
    /// key, tracker URLs or cookies).
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "{} -> qBit NeoRS migration{}", self.source_name(), if self.dry_run { " (dry run: nothing written)" } else { "" });
        let _ = writeln!(s, "Profile: {}", self.profile);
        for src in &self.sources {
            let _ = writeln!(s, "Source {src}");
        }
        let _ = writeln!(s, "Preferences from: {}", self.prefs_source);
        if let Some(sb) = &self.sandbox {
            let _ = writeln!(
                s,
                "SANDBOX: listen port {}, WebUI 127.0.0.1:{}, DHT/LSD/PeX/UPnP/RSS/watched folders/e-mail/external programs off, all torrents stopped",
                sb.listen_port, sb.webui_port
            );
        }
        for n in &self.prefs_notes {
            let _ = writeln!(s, "  - {n}");
        }
        let _ = writeln!(s, "\n== Summary");
        let ok_bits = self.torrents.iter().filter(|t| t.pieces.starts_with("bitfield")).count();
        let _ = writeln!(
            s,
            "Torrents: {} migrated ({} with imported bitfield, {} needing a recheck or metadata), {} failed",
            self.torrents.len(),
            ok_bits,
            self.torrents.len() - ok_bits,
            self.failed_torrents.len()
        );
        let _ = writeln!(s, "Categories ({}): {}", self.categories.len(), self.categories.join(", "));
        let _ = writeln!(s, "Tags ({}): {}", self.tags.len(), self.tags.join(", "));
        let _ = writeln!(s, "Watched folders: {}", self.watched_folders);
        if self.source_app.is_empty() {
            let _ = writeln!(s, "RSS: {} feed(s), {} rule(s), {} article file(s)", self.rss_feeds, self.rss_rules, self.rss_article_files);
            let _ = writeln!(
                s,
                "Search plugins ({}): {}; disabled: {}",
                self.search_plugins.len(),
                self.search_plugins.join(", "),
                if self.disabled_plugins.is_empty() { "none".to_owned() } else { self.disabled_plugins.join(", ") }
            );
        }
        let _ = writeln!(s, "GUI settings: {} key(s) -> config/gui-preferences.plist", self.gui_keys.len());
        let _ = writeln!(s, "Profile files {}: {}", if self.dry_run { "to write" } else { "written" }, self.files_written.len());
        if let Some((dl, ul)) = self.stats {
            let _ = writeln!(s, "All-time statistics: downloaded {dl} B, uploaded {ul} B");
        }
        if !self.skipped_files_created.is_empty() {
            let _ = writeln!(
                s,
                "Skipped files {} from libtorrent part files ({}):",
                if self.dry_run { "to be created" } else { "created" },
                self.skipped_files_created.len()
            );
            for f in &self.skipped_files_created {
                let _ = writeln!(s, "  + {f}");
            }
        }
        let _ = writeln!(s, "\n== Torrents");
        for t in &self.torrents {
            let progress = match (t.progress, t.qbt_progress) {
                (Some(p), Some(q)) => format!("{:.2}% (qBittorrent {:.2}%)", p * 100.0, q * 100.0),
                (Some(p), None) => format!("{:.2}%", p * 100.0),
                (None, Some(q)) => format!("? (qBittorrent {:.2}%)", q * 100.0),
                (None, None) => "?".into(),
            };
            let _ = writeln!(s, "{} {}", t.id, t.name);
            let _ = writeln!(
                s,
                "    {} | {} | category \"{}\" | tags [{}] | {} files ({} skipped) | {} tracker(s), {} web seed(s) | up {} B, down {} B",
                t.state,
                progress,
                t.category,
                t.tags.join(", "),
                t.files,
                t.skipped_files,
                t.trackers,
                t.web_seeds,
                t.uploaded,
                t.downloaded
            );
            let _ = writeln!(s, "    save path {}{} | data in {}", t.save_path, if t.auto_tmm { " (automatic)" } else { "" }, t.location);
            let _ = writeln!(s, "    {}", t.pieces);
            for n in &t.notes {
                let _ = writeln!(s, "    - {n}");
            }
        }
        for (id, e) in &self.failed_torrents {
            let _ = writeln!(s, "FAILED {id}: {e}");
        }
        if self.source_app.is_empty() || !self.prefs_diff.is_empty() {
            let _ = writeln!(s, "\n== Preferences that read differently ({})", self.prefs_diff.len());
        }
        for (k, a, b, why) in &self.prefs_diff {
            let _ = writeln!(s, "  {k}: qBittorrent {a} -> qBit NeoRS {b}  [{why}]");
        }
        let _ = writeln!(s, "\n== {} not carried over", if self.source_app.is_empty() { "qBittorrent.ini keys" } else { "Settings and data" });
        for (why, keys) in &self.ini_unused {
            let _ = writeln!(s, "  {why}: {}", keys.join(", "));
        }
        if !self.warnings.is_empty() {
            let _ = writeln!(s, "\n== Warnings");
            for w in &self.warnings {
                let _ = writeln!(s, "  ! {w}");
            }
        }
        s
    }
}

#[cfg(test)]
mod tests;
