//! One torrent's app-level state (what qBittorrent keeps in `TorrentImpl`
//! and its resume data) plus the runtime values refreshed from the engine,
//! and the computation of every `TorrentInfo`/properties/files field.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use super::categories::{self, CategoryMap, GlobalPaths, GlobalShareLimits};
use super::engine::EngineHandle;
use super::metainfo::{self, TorrentMeta};
use super::paths;
use super::resume::FastResume;
use super::state::{self, RateAverage, StateInputs};
use crate::model::*;
use crate::prefs::Preferences;

/// Global values needed to derive per-torrent fields.
pub struct Env<'a> {
    pub prefs: &'a Preferences,
    pub cats: &'a CategoryMap,
    pub paths: GlobalPaths,
    pub limits: GlobalShareLimits,
    pub now: i64,
}

impl<'a> Env<'a> {
    pub fn new(prefs: &'a Preferences, cats: &'a CategoryMap) -> Self {
        Self {
            prefs,
            cats,
            paths: global_paths(prefs),
            limits: global_limits(prefs),
            now: chrono::Utc::now().timestamp(),
        }
    }
}

pub fn global_paths(p: &Preferences) -> GlobalPaths {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let save = paths::resolve(&format!("{home}/Downloads"), &p.downloads.save_path);
    let temp = if p.downloads.temp_path.trim().is_empty() {
        paths::join(&save, "temp")
    } else {
        paths::resolve(&paths::join(&save, "temp"), &p.downloads.temp_path)
    };
    GlobalPaths { save_path: save, download_path: temp, download_path_enabled: p.downloads.temp_path_enabled }
}

pub fn global_limits(p: &Preferences) -> GlobalShareLimits {
    let b = &p.bittorrent;
    GlobalShareLimits {
        ratio: if b.max_ratio < 0.0 { -1.0 } else { b.max_ratio },
        seeding_minutes: if b.max_seeding_time < 0 { -1 } else { b.max_seeding_time },
        inactive_seeding_minutes: if b.max_inactive_seeding_time < 0 { -1 } else { b.max_inactive_seeding_time },
        action: b.max_ratio_act,
    }
}

/// Where the torrent is in the engine.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Phase {
    /// Not handed to the engine (no metadata, missing files, moving, ...).
    #[default]
    Detached,
    /// Being added.
    Adding,
    Checking { checked_bytes: u64 },
    Paused,
    Live,
}

/// Values that are not persisted.
#[derive(Default)]
pub struct Runtime {
    pub engine: Option<(EngineHandle, u64)>,
    /// Engine output folder (actual storage location) once attached.
    pub location: Option<String>,
    pub phase: Phase,
    /// Loaded from BT_backup and not yet in the engine.
    pub restoring: bool,
    pub moving: bool,
    pub missing_files: bool,
    pub error: Option<String>,
    pub error_reported: bool,
    pub queued: bool,
    /// Have-bytes per file (API index).
    pub file_progress: Vec<u64>,
    pub finished: bool,
    pub dl_rate: i64,
    pub ul_rate: i64,
    pub avg: RateAverage,
    pub last_fetched: u64,
    pub last_uploaded: u64,
    pub session_download: i64,
    pub session_upload: i64,
    pub peers_live: u32,
    pub seeders_live: u32,
    pub connections: u32,
    pub wasted: i64,
    pub availability: f64,
    pub swarm_seeds: Option<u32>,
    pub swarm_leechers: Option<u32>,
    pub tracker_warning: bool,
    pub tracker_error: bool,
    pub tracker_other_error: bool,
    pub working_tracker: Option<String>,
    pub next_announce: i64,
    /// The current error is "disk full".
    pub disk_full: bool,
    /// Start was requested for a torrent with missing files.
    pub retry_missing: bool,
    /// First engine add should skip the hash check.
    pub skip_hash_check: bool,
    pub active_ms: u64,
    pub seeding_ms: u64,
    pub slow_since: Option<Instant>,
    pub metadata_task: Option<tokio::task::AbortHandle>,
    /// The trackers the running metadata lookup announces to.
    pub metadata_trackers: Option<super::engine::MetadataTrackers>,
    pub dirty: bool,
    pub adding: bool,
    /// Data was downloaded since the last full check.
    pub unchecked: bool,
    /// A recheck was started because of "recheck completed torrents".
    pub recheck_on_complete_pending: bool,
    /// Per-torrent engine settings must be (re)applied.
    pub settings_dirty: bool,
    pub last_checked_phase: bool,
}

pub struct TorrentEntry {
    pub id: String,
    pub meta: Option<Arc<TorrentMeta>>,
    pub name: String,
    pub magnet_name: String,
    /// v2 hash (hex) from a magnet link, until the metadata is known.
    pub magnet_v2: String,
    /// v1 hash (hex) of a magnet link with both hashes (its id is the
    /// truncated v2 hash), until the metadata is known.
    pub magnet_v1: String,
    pub comment: Option<String>,
    pub category: String,
    pub tags: Vec<String>,
    pub auto_tmm: bool,
    pub save_path: String,
    pub download_path: String,
    pub content_layout: ContentLayout,
    /// User-visible relative paths per file (API index).
    pub file_paths: Vec<String>,
    pub file_priorities: Vec<FilePriority>,
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
    pub share_limit_action: ShareLimitAction,
    pub dl_limit: i64,
    pub ul_limit: i64,
    pub stopped: bool,
    pub forced: bool,
    pub stop_condition: StopCondition,
    pub queue_position: i64,
    pub sequential: bool,
    pub first_last: bool,
    pub super_seeding: bool,
    pub seed_status: bool,
    pub trackers: Vec<TrackerTierEntry>,
    pub url_seeds: Vec<String>,
    /// Trackers and web seeds trusted by SSRF mitigation (from a local
    /// .torrent file or added by the user); see `trust_urls`.
    pub trusted_urls: Vec<String>,
    pub peers: Vec<SocketAddr>,
    pub completed_bytes: i64,
    pub disable_dht: bool,
    pub disable_pex: bool,
    pub disable_lsd: bool,
    pub rt: Runtime,
}

pub fn layout_to_str(l: ContentLayout) -> &'static str {
    match l {
        ContentLayout::Original => "Original",
        ContentLayout::Subfolder => "Subfolder",
        ContentLayout::NoSubfolder => "NoSubfolder",
    }
}

pub fn layout_from_str(s: &str) -> ContentLayout {
    match s {
        "Subfolder" => ContentLayout::Subfolder,
        "NoSubfolder" => ContentLayout::NoSubfolder,
        _ => ContentLayout::Original,
    }
}

pub fn stop_condition_to_str(c: StopCondition) -> &'static str {
    match c {
        StopCondition::None => "None",
        StopCondition::MetadataReceived => "MetadataReceived",
        StopCondition::FilesChecked => "FilesChecked",
    }
}

pub fn stop_condition_from_str(s: &str) -> StopCondition {
    match s {
        "MetadataReceived" => StopCondition::MetadataReceived,
        "FilesChecked" => StopCondition::FilesChecked,
        _ => StopCondition::None,
    }
}

impl TorrentEntry {
    pub fn new(id: String) -> Self {
        Self {
            id,
            meta: None,
            name: String::new(),
            magnet_name: String::new(),
            magnet_v2: String::new(),
            magnet_v1: String::new(),
            comment: None,
            category: String::new(),
            tags: Vec::new(),
            auto_tmm: false,
            save_path: String::new(),
            download_path: String::new(),
            content_layout: ContentLayout::Original,
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
            share_limit_action: ShareLimitAction::Default,
            dl_limit: 0,
            ul_limit: 0,
            stopped: false,
            forced: false,
            stop_condition: StopCondition::None,
            queue_position: -1,
            sequential: false,
            first_last: false,
            super_seeding: false,
            seed_status: false,
            trackers: Vec::new(),
            url_seeds: Vec::new(),
            trusted_urls: Vec::new(),
            peers: Vec::new(),
            completed_bytes: 0,
            disable_dht: false,
            disable_pex: false,
            disable_lsd: false,
            rt: Runtime::default(),
        }
    }

    pub fn from_resume(r: FastResume, meta: Option<Arc<TorrentMeta>>) -> Self {
        let mut e = Self::new(r.info_hash.to_ascii_lowercase());
        e.name = r.name;
        e.magnet_name = r.magnet_name;
        if metainfo::parse_hex32(&r.info_hash_v2).is_some() {
            e.magnet_v2 = r.info_hash_v2.to_ascii_lowercase();
            if metainfo::parse_hex20(&r.info_hash_v1).is_some() {
                e.magnet_v1 = r.info_hash_v1.to_ascii_lowercase();
            }
        }
        e.comment = r.comment;
        e.category = r.category;
        e.tags = r.tags;
        e.auto_tmm = r.auto_tmm || r.save_path.is_empty();
        e.save_path = r.save_path;
        e.download_path = r.download_path;
        e.content_layout = layout_from_str(&r.content_layout);
        e.added_on = r.added_on;
        e.completed_on = r.completed_on;
        e.seen_complete = r.seen_complete;
        e.all_time_download = r.all_time_download.max(0);
        e.all_time_upload = r.all_time_upload.max(0);
        e.active_time = r.active_time.max(0);
        e.seeding_time = r.seeding_time.max(0);
        e.last_download = r.last_download;
        e.last_upload = r.last_upload;
        e.ratio_limit = if r.ratio_limit < -2.0 { -1.0 } else { r.ratio_limit };
        e.seeding_time_limit = r.seeding_time_limit.max(-2);
        e.inactive_seeding_time_limit = r.inactive_seeding_time_limit.max(-2);
        e.share_limit_action = categories::action_from_str(&r.share_limit_action);
        e.dl_limit = r.download_limit.max(0);
        e.ul_limit = r.upload_limit.max(0);
        e.stopped = r.stopped;
        e.forced = r.operating_mode == "Forced";
        e.stop_condition = stop_condition_from_str(&r.stop_condition);
        e.queue_position = r.queue_position;
        e.sequential = r.sequential;
        e.first_last = r.first_last_piece_priority;
        e.super_seeding = r.super_seeding;
        e.seed_status = r.seed_status;
        e.trackers = r.trackers;
        e.url_seeds = r.url_seeds.into_iter().filter(|u| metainfo::is_web_url(u)).take(metainfo::MAX_WEB_SEEDS).collect();
        e.trusted_urls = r.trusted_urls;
        e.prune_trusted_urls();
        e.peers = r.peers.iter().filter_map(|p| metainfo::parse_peer_address(p)).collect();
        e.completed_bytes = r.completed_bytes;
        e.rt.location = (!r.location.is_empty()).then(|| r.location.clone());
        e.disable_dht = r.disable_dht;
        e.disable_pex = r.disable_pex;
        e.disable_lsd = r.disable_lsd;
        if let Some(m) = &meta {
            let n = m.files.len();
            let valid_paths = r.file_paths.len() == n && r.file_paths.iter().all(|p| paths::is_valid_relative_path(p));
            e.file_paths = if valid_paths { r.file_paths } else { paths::apply_content_layout(&m.file_paths(), e.content_layout) };
            e.file_priorities = if r.file_priorities.len() == n {
                r.file_priorities.iter().map(|p| FilePriority::from_i32(*p).unwrap_or_default()).collect()
            } else {
                vec![FilePriority::Normal; n]
            };
        }
        e.meta = meta;
        e
    }

    pub fn to_resume(&self) -> FastResume {
        FastResume {
            info_hash: self.id.clone(),
            name: self.name.clone(),
            magnet_name: self.magnet_name.clone(),
            info_hash_v2: if self.meta.is_some() { String::new() } else { self.magnet_v2.clone() },
            info_hash_v1: if self.meta.is_some() { String::new() } else { self.magnet_v1.clone() },
            comment: self.comment.clone(),
            category: self.category.clone(),
            tags: self.tags.clone(),
            auto_tmm: self.auto_tmm,
            save_path: if self.auto_tmm { String::new() } else { self.save_path.clone() },
            download_path: if self.auto_tmm { String::new() } else { self.download_path.clone() },
            content_layout: layout_to_str(self.content_layout).into(),
            file_paths: self.file_paths.clone(),
            file_priorities: self.file_priorities.iter().map(|p| p.as_i32()).collect(),
            added_on: self.added_on,
            completed_on: self.completed_on,
            seen_complete: self.seen_complete,
            all_time_download: self.all_time_download,
            all_time_upload: self.all_time_upload,
            active_time: self.active_time,
            seeding_time: self.seeding_time,
            last_download: self.last_download,
            last_upload: self.last_upload,
            ratio_limit: self.ratio_limit,
            seeding_time_limit: self.seeding_time_limit,
            inactive_seeding_time_limit: self.inactive_seeding_time_limit,
            share_limit_action: categories::action_to_str(self.share_limit_action).into(),
            download_limit: self.dl_limit,
            upload_limit: self.ul_limit,
            stopped: self.stopped,
            operating_mode: if self.forced { "Forced".into() } else { "AutoManaged".into() },
            stop_condition: stop_condition_to_str(self.stop_condition).into(),
            queue_position: self.queue_position,
            sequential: self.sequential,
            first_last_piece_priority: self.first_last,
            super_seeding: self.super_seeding,
            seed_status: self.seed_status,
            trackers: self.trackers.clone(),
            url_seeds: self.url_seeds.clone(),
            trusted_urls: self.trusted_urls.clone(),
            peers: self.peers.iter().map(|p| p.to_string()).collect(),
            completed_bytes: self.completed_bytes(),
            location: self.location_for_resume(),
            disable_dht: self.disable_dht,
            disable_pex: self.disable_pex,
            disable_lsd: self.disable_lsd,
            ..Default::default()
        }
    }

    /// Marks URLs as trusted by SSRF mitigation (user input, or the
    /// trackers and web seeds of a local .torrent file).
    pub fn trust_urls<'a>(&mut self, urls: impl IntoIterator<Item = &'a str>) {
        for u in urls {
            let u = u.trim();
            if !u.is_empty() && !self.trusted_urls.iter().any(|t| t == u) {
                self.trusted_urls.push(u.to_string());
            }
        }
        self.prune_trusted_urls();
    }

    /// Keeps only trusted URLs still in use (bounds the list).
    pub fn prune_trusted_urls(&mut self) {
        let (trackers, seeds) = (&self.trackers, &self.url_seeds);
        self.trusted_urls.retain(|t| trackers.iter().any(|x| x.url.trim() == t) || seeds.iter().any(|x| x.trim() == t));
    }

    pub fn has_metadata(&self) -> bool {
        self.meta.is_some()
    }

    /// Its saved .torrent lacks some piece layers (pure v2): the engine
    /// fetches them while the torrent runs (they are saved as they come),
    /// or a complete copy of the .torrent added again provides them.
    pub fn needs_piece_layers(&self) -> bool {
        self.meta.as_ref().is_some_and(|m| m.missing_piece_layers)
    }

    pub fn is_private(&self) -> bool {
        self.meta.as_ref().is_some_and(|m| m.private)
    }

    /// Display name: explicit, metadata, magnet `dn`, else the id.
    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        if let Some(m) = &self.meta {
            return m.name.clone();
        }
        if !self.magnet_name.is_empty() {
            return self.magnet_name.clone();
        }
        self.id.clone()
    }

    pub fn comment(&self) -> String {
        match &self.comment {
            Some(c) => c.clone(),
            None => self.meta.as_ref().map(|m| m.comment.clone()).unwrap_or_default(),
        }
    }

    pub fn save_path(&self, env: &Env) -> String {
        if self.auto_tmm { categories::category_save_path(env.cats, &self.category, &env.paths) } else { self.save_path.clone() }
    }

    pub fn download_path(&self, env: &Env) -> String {
        if self.auto_tmm {
            categories::category_download_path(env.cats, &self.category, &env.paths)
        } else {
            self.download_path.clone()
        }
    }

    pub fn is_finished(&self) -> bool {
        self.has_metadata() && self.rt.finished && !matches!(self.rt.phase, Phase::Checking { .. })
    }

    /// Where the content should be (`adjustStorageLocation` target).
    pub fn target_location(&self, env: &Env) -> String {
        let dl = self.download_path(env);
        if self.is_finished() || self.seed_status || dl.is_empty() { self.save_path(env) } else { dl }
    }

    /// Current on-disk location of the content.
    pub fn actual_location(&self, env: &Env) -> String {
        match &self.rt.location {
            Some(l) => l.clone(),
            None => self.target_location(env),
        }
    }

    pub fn total_size(&self) -> i64 {
        self.meta.as_ref().map(|m| m.total_size as i64).unwrap_or(-1)
    }

    pub fn wanted_size(&self) -> i64 {
        let Some(m) = &self.meta else { return 0 };
        m.files
            .iter()
            .zip(&self.file_priorities)
            .filter(|(_, p)| **p != FilePriority::Ignored)
            .map(|(f, _)| f.size as i64)
            .sum()
    }

    pub fn completed_bytes(&self) -> i64 {
        let Some(m) = &self.meta else { return 0 };
        if self.rt.file_progress.len() != m.files.len() {
            return self.completed_bytes;
        }
        m.files
            .iter()
            .zip(&self.file_priorities)
            .zip(&self.rt.file_progress)
            .filter(|((_, p), _)| **p != FilePriority::Ignored)
            .map(|((f, _), have)| (*have).min(f.size) as i64)
            .sum()
    }

    pub fn progress(&self) -> f64 {
        self.progress_of(self.wanted_size(), self.completed_bytes())
    }

    /// [`Self::progress`] from already computed sizes (each is a pass over all files).
    fn progress_of(&self, wanted: i64, completed: i64) -> f64 {
        let Some(m) = &self.meta else { return 0.0 };
        if let Phase::Checking { checked_bytes } = self.rt.phase {
            return if m.total_size == 0 { 0.0 } else { (checked_bytes as f64 / m.total_size as f64).clamp(0.0, 1.0) };
        }
        if wanted == 0 {
            return if self.rt.finished { 1.0 } else { 0.0 };
        }
        (completed as f64 / wanted as f64).clamp(0.0, 1.0)
    }

    pub fn pieces_have(&self) -> i64 {
        self.pieces_have_of(self.total_done())
    }

    fn pieces_have_of(&self, total_done: i64) -> i64 {
        let Some(m) = &self.meta else { return 0 };
        if m.piece_length == 0 {
            return 0;
        }
        (total_done as u64 / m.piece_length as u64).min(m.pieces as u64) as i64
    }

    pub fn state_inputs(&self) -> StateInputs {
        StateInputs {
            checking_resume_data: self.rt.restoring && self.has_metadata() && !self.stopped,
            moving: self.rt.moving,
            missing_files: self.rt.missing_files,
            error: self.rt.error.is_some(),
            has_metadata: self.has_metadata(),
            stopped: self.stopped,
            queued: self.rt.queued,
            forced: self.forced,
            checking: matches!(self.rt.phase, Phase::Checking { .. }),
            finished: self.is_finished(),
            has_finished_status: self.seed_status,
            dl_rate: self.dl_rate(),
            ul_rate: self.ul_rate(),
        }
    }

    pub fn state(&self) -> TorrentState {
        state::compute_state(&self.state_inputs())
    }

    pub fn dl_rate(&self) -> i64 {
        if self.stopped { 0 } else { self.rt.dl_rate }
    }

    pub fn ul_rate(&self) -> i64 {
        if self.stopped { 0 } else { self.rt.ul_rate }
    }

    pub fn effective_ratio_limit(&self, env: &Env) -> f64 {
        if self.ratio_limit == -2.0 {
            categories::category_ratio_limit(env.cats, &self.category, &env.limits)
        } else {
            self.ratio_limit
        }
    }

    pub fn effective_seeding_time_limit(&self, env: &Env) -> i64 {
        if self.seeding_time_limit == -2 {
            categories::category_seeding_time_limit(env.cats, &self.category, &env.limits)
        } else {
            self.seeding_time_limit
        }
    }

    pub fn effective_inactive_seeding_time_limit(&self, env: &Env) -> i64 {
        if self.inactive_seeding_time_limit == -2 {
            categories::category_inactive_seeding_time_limit(env.cats, &self.category, &env.limits)
        } else {
            self.inactive_seeding_time_limit
        }
    }

    pub fn effective_share_limit_action(&self, env: &Env) -> ShareLimitAction {
        if self.share_limit_action == ShareLimitAction::Default {
            categories::category_share_limit_action(env.cats, &self.category, &env.limits)
        } else {
            self.share_limit_action
        }
    }

    pub fn total_done(&self) -> i64 {
        self.rt.file_progress.iter().sum::<u64>() as i64
    }

    pub fn real_ratio(&self) -> f64 {
        state::real_ratio(self.all_time_upload, self.all_time_download, self.total_done())
    }

    pub fn time_since_activity(&self, now: i64) -> i64 {
        let up = if self.last_upload < 0 { -1 } else { (now - self.last_upload).max(0) };
        let down = if self.last_download < 0 { -1 } else { (now - self.last_download).max(0) };
        state::time_since_activity(up, down)
    }

    /// The working tracker, else the first one.
    pub fn current_tracker(&self) -> String {
        self.rt.working_tracker.clone().or_else(|| self.trackers.first().map(|t| t.url.clone())).unwrap_or_default()
    }

    pub fn root_path(&self, env: &Env) -> String {
        if !self.has_metadata() {
            return String::new();
        }
        match paths::find_root_folder(&self.file_paths) {
            Some(root) => paths::join(&self.actual_location(env), &root),
            None => String::new(),
        }
    }

    pub fn content_path(&self, env: &Env) -> String {
        self.content_path_of(env, || self.root_path(env))
    }

    /// [`Self::content_path`] with the root path supplied by the caller (it
    /// is only needed for multi-file torrents).
    fn content_path_of(&self, env: &Env, root_path: impl FnOnce() -> String) -> String {
        if !self.has_metadata() {
            return String::new();
        }
        if self.file_paths.len() == 1 {
            return paths::join(&self.actual_location(env), &self.file_paths[0]);
        }
        let root = root_path();
        if root.is_empty() { self.actual_location(env) } else { root }
    }

    /// ("Info hash v1", "Info hash v2") as qBittorrent shows them; either
    /// is empty when the torrent has no such hash.
    pub fn info_hashes(&self) -> (String, String) {
        match self.meta.as_deref() {
            Some(m) => (m.info_hash_v1_hex(), m.info_hash_v2_hex()),
            // A magnet with a v2 hash is identified by the truncated v2
            // hash; its v1 hash (if any) is kept apart.
            None if !self.magnet_v2.is_empty() => (self.magnet_v1.clone(), self.magnet_v2.clone()),
            None => (self.id.clone(), String::new()),
        }
    }

    /// The v1 hash of a hybrid torrent (or of a magnet link with both
    /// hashes): qBittorrent's alternative id of the torrent.
    pub fn alt_id(&self) -> Option<String> {
        match self.meta.as_deref() {
            Some(m) => m.alt_id(),
            None => (!self.magnet_v1.is_empty() && !self.magnet_v2.is_empty()).then(|| self.magnet_v1.clone()),
        }
    }

    pub fn magnet_uri(&self) -> String {
        let (v1, v2) = self.info_hashes();
        self.build_magnet(&v1, &v2)
    }

    fn build_magnet(&self, v1: &str, v2: &str) -> String {
        metainfo::build_magnet(
            v1,
            v2,
            &self.display_name(),
            self.meta.as_ref().map(|m| m.total_size),
            &self.trackers,
            &self.url_seeds,
        )
    }

    pub fn eta(&self, env: &Env) -> i64 {
        self.eta_of(env, self.wanted_size(), self.completed_bytes())
    }

    fn eta_of(&self, env: &Env, wanted_size: i64, completed_size: i64) -> i64 {
        state::eta(&state::EtaInputs {
            stopped: self.stopped,
            finished: self.is_finished(),
            avg_dl: self.rt.avg.dl,
            avg_ul: self.rt.avg.ul,
            wanted_size,
            completed_size,
            all_time_download: self.all_time_download,
            all_time_upload: self.all_time_upload,
            ratio_limit: self.effective_ratio_limit(env),
            seeding_time_limit: self.effective_seeding_time_limit(env),
            inactive_seeding_time_limit: self.effective_inactive_seeding_time_limit(env),
            seeding_time: self.seeding_time,
            time_since_activity: self.time_since_activity(env.now),
        })
    }

    /// Distributed copies (-1 unknown or seeding).
    pub fn availability(&self) -> f64 {
        if !self.has_metadata() || !matches!(self.rt.phase, Phase::Live) {
            return -1.0;
        }
        self.rt.availability
    }

    pub fn info(&self, env: &Env) -> TorrentInfo {
        // Each of these is a pass over all files or hashes a name; the
        // GUI asks for every torrent on every refresh, so compute them once.
        let state = self.state();
        let meta = self.meta.as_deref();
        let total_done = self.total_done();
        let ratio = state::real_ratio(self.all_time_upload, self.all_time_download, total_done);
        let queueing = env.prefs.bittorrent.queueing_enabled;
        let wanted = self.wanted_size();
        let completed = self.completed_bytes();
        let (infohash_v1, infohash_v2) = self.info_hashes();
        let magnet_uri = self.build_magnet(&infohash_v1, &infohash_v2);
        let root_path = self.root_path(env);
        let seeds = self.rt.seeders_live as i64;
        let peers = self.rt.peers_live as i64;
        let connections_limit = env.prefs.connection.max_connec_per_torrent;
        TorrentInfo {
            hash: self.id.clone(),
            infohash_v1,
            infohash_v2,
            name: self.display_name(),
            has_metadata: meta.is_some(),
            created_by: meta.map(|m| m.created_by.clone()).unwrap_or_default(),
            creation_date: meta.map(|m| m.creation_date).unwrap_or(-1),
            private: meta.map(|m| m.private),
            total_size: self.total_size(),
            pieces_num: meta.map(|m| m.pieces as i64).unwrap_or(-1),
            piece_size: meta.map(|m| m.piece_length as i64).unwrap_or(-1),
            magnet_uri,
            size: wanted,
            progress: self.progress_of(wanted, completed),
            total_wasted: self.rt.wasted,
            pieces_have: self.pieces_have_of(total_done),
            dl_speed: self.dl_rate(),
            up_speed: self.ul_rate(),
            queue_position: if queueing && self.queue_position >= 0 { self.queue_position + 1 } else { 0 },
            num_seeds: seeds,
            num_complete: self.rt.swarm_seeds.map(|v| v as i64).unwrap_or(seeds),
            num_leechs: (peers - seeds).max(0),
            num_incomplete: self.rt.swarm_leechers.map(|v| v as i64).unwrap_or((peers - seeds).max(0)),
            state,
            eta: self.eta_of(env, wanted, completed),
            seq_dl: self.sequential,
            f_l_piece_prio: self.first_last,
            category: self.category.clone(),
            tags: self.tags.clone(),
            super_seeding: self.super_seeding,
            force_start: self.forced && !self.stopped,
            save_path: self.save_path(env),
            download_path: self.download_path(env),
            content_path: self.content_path_of(env, || root_path.clone()),
            root_path,
            added_on: self.added_on,
            completion_on: self.completed_on,
            tracker: self.current_tracker(),
            trackers_count: self.trackers.len() as i64,
            dl_limit: self.dl_limit.max(0),
            up_limit: self.ul_limit.max(0),
            downloaded: self.all_time_download,
            uploaded: self.all_time_upload,
            downloaded_session: self.rt.session_download,
            uploaded_session: self.rt.session_upload,
            amount_left: (wanted - completed).max(0),
            completed,
            connections_count: self.rt.connections as i64,
            connections_limit: if connections_limit <= 0 { -1 } else { connections_limit },
            max_ratio: self.effective_ratio_limit(env),
            max_seeding_time: self.effective_seeding_time_limit(env),
            max_inactive_seeding_time: self.effective_inactive_seeding_time_limit(env),
            ratio: state::api_ratio(ratio),
            ratio_limit: self.ratio_limit,
            popularity: state::api_ratio(state::popularity(ratio, self.active_time)),
            seeding_time_limit: self.seeding_time_limit,
            inactive_seeding_time_limit: self.inactive_seeding_time_limit,
            share_limit_action: self.share_limit_action,
            last_seen_complete: self.seen_complete,
            last_activity: {
                let t = self.time_since_activity(env.now);
                if t < 0 { self.added_on } else { env.now - t }
            },
            auto_tmm: self.auto_tmm,
            time_active: self.active_time,
            seeding_time: self.seeding_time,
            availability: self.availability(),
            reannounce: self.rt.next_announce,
            comment: self.comment(),
            has_tracker_warning: self.rt.tracker_warning,
            has_tracker_error: self.rt.tracker_error,
            has_other_announce_error: self.rt.tracker_other_error,
            error_message: if self.rt.missing_files {
                "Missing files".to_string()
            } else {
                self.rt.error.clone().unwrap_or_default()
            },
        }
    }

    pub fn properties(&self, env: &Env) -> TorrentProperties {
        let info = self.info(env);
        let downloading_secs = self.active_time - self.seeding_time;
        TorrentProperties {
            hash: self.id.clone(),
            infohash_v1: info.infohash_v1.clone(),
            infohash_v2: info.infohash_v2.clone(),
            name: info.name.clone(),
            time_elapsed: self.active_time,
            seeding_time: self.seeding_time,
            eta: info.eta,
            connections: info.connections_count,
            connections_limit: info.connections_limit,
            total_downloaded: self.all_time_download,
            total_downloaded_session: self.rt.session_download,
            total_uploaded: self.all_time_upload,
            total_uploaded_session: self.rt.session_upload,
            dl_speed: info.dl_speed,
            dl_speed_avg: if downloading_secs > 0 { self.all_time_download / downloading_secs } else { -1 },
            up_speed: info.up_speed,
            up_speed_avg: if self.active_time > 0 { self.all_time_upload / self.active_time } else { -1 },
            dl_limit: if self.dl_limit > 0 { self.dl_limit } else { -1 },
            up_limit: if self.ul_limit > 0 { self.ul_limit } else { -1 },
            total_wasted: info.total_wasted,
            seeds: info.num_seeds,
            seeds_total: info.num_complete,
            peers: info.num_leechs,
            peers_total: info.num_incomplete,
            share_ratio: info.ratio,
            popularity: info.popularity,
            reannounce: info.reannounce,
            total_size: info.total_size,
            pieces_num: info.pieces_num,
            piece_size: info.piece_size,
            pieces_have: info.pieces_have,
            created_by: info.created_by.clone(),
            last_seen: self.seen_complete,
            addition_date: self.added_on,
            completion_date: self.completed_on,
            creation_date: info.creation_date,
            save_path: info.save_path.clone(),
            download_path: info.download_path.clone(),
            comment: info.comment.clone(),
            private: info.private,
            has_metadata: info.has_metadata,
            progress: info.progress,
            availability: info.availability,
        }
    }

    /// `avail`: connected peers per piece (empty = unknown or seeding).
    pub fn files(&self, indexes: &[i64], avail: &[u32]) -> Result<Vec<TorrentFile>, String> {
        let Some(m) = &self.meta else { return Ok(Vec::new()) };
        let n = m.files.len() as i64;
        for &i in indexes {
            if i < 0 || i >= n {
                return Err(format!("Index {i} is out of bounds."));
            }
        }
        let is_seed = self.is_finished();
        let pick: Vec<usize> =
            if indexes.is_empty() { (0..m.files.len()).collect() } else { indexes.iter().map(|&i| i as usize).collect() };
        Ok(pick
            .into_iter()
            .map(|i| {
                let f = &m.files[i];
                let have = self.rt.file_progress.get(i).copied().unwrap_or(0).min(f.size);
                TorrentFile {
                    index: i as i64,
                    name: self.file_paths.get(i).cloned().unwrap_or_else(|| f.path.clone()),
                    size: f.size as i64,
                    progress: if f.size == 0 { 1.0 } else { have as f64 / f.size as f64 },
                    priority: self.file_priorities.get(i).copied().unwrap_or_default(),
                    is_seed,
                    piece_range: vec![f.first_piece as i64, f.last_piece as i64],
                    availability: file_availability(f.first_piece, f.last_piece, f.size, avail),
                }
            })
            .collect())
    }

    pub fn location_for_resume(&self) -> String {
        self.rt.location.clone().unwrap_or_default()
    }

    /// Engine-indexed priorities (padding files are not downloaded).
    pub fn engine_priorities(&self) -> Vec<u8> {
        let Some(m) = &self.meta else { return Vec::new() };
        let mut out = vec![0u8; m.engine_file_count as usize];
        for (f, p) in m.files.iter().zip(&self.file_priorities) {
            if let Some(slot) = out.get_mut(f.engine_index as usize) {
                *slot = p.as_i32() as u8;
            }
        }
        out
    }

    /// Relative path of every (non-padding) file by engine index: content
    /// layout and renames are expressed as engine renames.
    pub fn engine_paths(&self) -> std::collections::BTreeMap<usize, std::path::PathBuf> {
        let Some(m) = &self.meta else { return Default::default() };
        m.files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.engine_index as usize, std::path::PathBuf::from(self.file_paths.get(i).unwrap_or(&f.path))))
            .collect()
    }

    /// Maps engine per-file progress to API indexes.
    pub fn map_file_progress(&self, engine: &[u64]) -> Vec<u64> {
        let Some(m) = &self.meta else { return Vec::new() };
        m.files.iter().map(|f| engine.get(f.engine_index as usize).copied().unwrap_or(0)).collect()
    }

    pub fn wanted_complete(&self) -> bool {
        let Some(m) = &self.meta else { return false };
        if self.rt.file_progress.len() != m.files.len() {
            return false;
        }
        m.files
            .iter()
            .zip(&self.file_priorities)
            .zip(&self.rt.file_progress)
            .all(|((f, p), have)| *p == FilePriority::Ignored || *have >= f.size)
    }

    pub fn belongs_to_category(&self, category: &str) -> bool {
        categories::belongs_to_category(&self.category, category)
    }
}

/// Fraction of a file's pieces available in the swarm; -1 when unknown.
fn file_availability(first: u32, last: u32, size: u64, avail: &[u32]) -> f64 {
    if avail.is_empty() {
        return -1.0;
    }
    if size == 0 {
        return 1.0;
    }
    let range = (first as usize)..=(last as usize).min(avail.len().saturating_sub(1));
    let total = range.clone().count();
    if total == 0 {
        return 1.0;
    }
    avail[range].iter().filter(|&&a| a > 0).count() as f64 / total as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bittorrent::metainfo::MetaFile;

    fn meta(files: &[(&str, u64)]) -> Arc<TorrentMeta> {
        let mut offset = 0;
        Arc::new(TorrentMeta {
            info_hash: [1; 20],
            info_hash_v2: None,
            name: "n".into(),
            piece_length: 16,
            pieces: 10,
            total_size: files.iter().map(|f| f.1).sum(),
            private: false,
            files: files
                .iter()
                .enumerate()
                .map(|(i, (p, s))| {
                    let f = MetaFile {
                        path: p.to_string(),
                        size: *s,
                        offset,
                        first_piece: 0,
                        last_piece: 0,
                        engine_index: i as u32,
                    };
                    offset += s;
                    f
                })
                .collect(),
            engine_file_count: files.len() as u32,
            comment: "c".into(),
            created_by: String::new(),
            creation_date: -1,
            trackers: Vec::new(),
            url_seeds: Vec::new(),
            missing_piece_layers: false,
        })
    }

    #[test]
    fn resume_round_trip() {
        let m = meta(&[("r/a", 10), ("r/b", 20)]);
        let mut e = TorrentEntry::new("ab".repeat(20));
        e.meta = Some(m.clone());
        e.file_paths = vec!["r/a".into(), "r/x/b".into()];
        e.file_priorities = vec![FilePriority::Ignored, FilePriority::High];
        e.category = "c".into();
        e.tags = vec!["t".into()];
        e.save_path = "/s".into();
        e.forced = true;
        e.ratio_limit = 1.25;
        e.stop_condition = StopCondition::FilesChecked;
        e.peers = vec!["1.2.3.4:5".parse().unwrap()];
        let back = TorrentEntry::from_resume(e.to_resume(), Some(m));
        assert_eq!(back.to_resume(), e.to_resume());
        assert!(!back.auto_tmm);
        assert!(back.forced);
    }

    #[test]
    fn content_and_root_paths() {
        let prefs = Preferences::default();
        let cats = CategoryMap::new();
        let env = Env::new(&prefs, &cats);
        let mut e = TorrentEntry::new("ab".repeat(20));
        e.meta = Some(meta(&[("r/a", 10), ("r/b", 20)]));
        e.file_paths = vec!["r/a".into(), "r/b".into()];
        e.file_priorities = vec![FilePriority::Normal; 2];
        e.save_path = "/dl".into();
        assert_eq!(e.root_path(&env), "/dl/r");
        assert_eq!(e.content_path(&env), "/dl/r");
        e.file_paths = vec!["a".into(), "b".into()];
        assert_eq!(e.root_path(&env), "");
        assert_eq!(e.content_path(&env), "/dl");
        e.meta = Some(meta(&[("single.bin", 5)]));
        e.file_paths = vec!["single.bin".into()];
        e.file_priorities = vec![FilePriority::Normal];
        assert_eq!(e.content_path(&env), "/dl/single.bin");
        assert_eq!(e.root_path(&env), "");
    }

    #[test]
    fn progress_and_sizes_respect_priorities() {
        let mut e = TorrentEntry::new("ab".repeat(20));
        e.meta = Some(meta(&[("a", 100), ("b", 300)]));
        e.file_paths = vec!["a".into(), "b".into()];
        e.file_priorities = vec![FilePriority::Normal, FilePriority::Ignored];
        e.rt.file_progress = vec![50, 300];
        assert_eq!(e.wanted_size(), 100);
        assert_eq!(e.completed_bytes(), 50);
        assert_eq!(e.progress(), 0.5);
        assert!(!e.wanted_complete());
        e.rt.file_progress = vec![100, 0];
        assert!(e.wanted_complete());
        e.rt.phase = Phase::Checking { checked_bytes: 200 };
        assert_eq!(e.progress(), 0.5);
    }

    #[test]
    fn engine_priorities_and_paths() {
        let mut e = TorrentEntry::new("ab".repeat(20));
        e.meta = Some(meta(&[("d/a", 100), ("d/b", 10)]));
        e.file_paths = vec!["x/a".into(), "d/b".into()];
        e.file_priorities = vec![FilePriority::High, FilePriority::Ignored];
        assert_eq!(e.engine_priorities(), vec![6, 0]);
        let paths = e.engine_paths();
        assert_eq!(paths[&0], std::path::PathBuf::from("x/a"));
        assert_eq!(file_availability(0, 1, 10, &[1, 0]), 0.5);
        assert_eq!(file_availability(0, 1, 10, &[]), -1.0);
    }

    #[test]
    fn info_matches_individual_accessors() {
        let prefs = Preferences::default();
        let cats = CategoryMap::new();
        let env = Env::new(&prefs, &cats);
        let mut e = TorrentEntry::new("ab".repeat(20));
        e.meta = Some(meta(&[("r/a", 100), ("r/b", 300), ("r/c", 50)]));
        e.file_paths = vec!["r/a".into(), "r/b".into(), "r/c".into()];
        e.file_priorities = vec![FilePriority::Normal, FilePriority::Ignored, FilePriority::High];
        e.rt.file_progress = vec![50, 300, 25];
        e.save_path = "/dl".into();
        let i = e.info(&env);
        assert_eq!(i.progress, e.progress());
        assert_eq!(i.eta, e.eta(&env));
        assert_eq!(i.pieces_have, e.pieces_have());
        assert_eq!(i.magnet_uri, e.magnet_uri());
        assert_eq!(i.content_path, e.content_path(&env));
        assert_eq!(i.root_path, e.root_path(&env));
        assert_eq!(i.root_path, "/dl/r");
        assert_eq!(i.ratio, state::api_ratio(e.real_ratio()));
        assert_eq!((i.infohash_v1, i.infohash_v2), e.info_hashes());
    }

    /// `cargo test -p qbt-core --lib info_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn info_cost() {
        let prefs = Preferences::default();
        let cats = CategoryMap::new();
        let env = Env::new(&prefs, &cats);
        let names: Vec<String> = (0..2000).map(|i| format!("root/dir{}/file{i}.bin", i % 50)).collect();
        let files: Vec<(&str, u64)> = names.iter().map(|n| (n.as_str(), 1000)).collect();
        let mut e = TorrentEntry::new("ab".repeat(20));
        e.meta = Some(meta(&files));
        e.file_paths = names.clone();
        e.file_priorities = vec![FilePriority::Normal; names.len()];
        e.rt.file_progress = vec![500; names.len()];
        e.save_path = "/dl".into();
        let t = std::time::Instant::now();
        let mut n = 0;
        for _ in 0..1000 {
            n += e.info(&env).name.len();
        }
        println!("info() x1000, 2000 files: {:?} ({n})", t.elapsed());
    }
}
