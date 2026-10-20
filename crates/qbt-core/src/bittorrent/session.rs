//! The BitTorrent session: torrent registry, categories, tags, global
//! transfer state and the facade operations. Long-running work lives in
//! `actions.rs` (engine-facing actions), `adding.rs` (adding torrents and
//! metadata), `policy.rs` (the session timer) and `watched.rs`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Instant;

use parking_lot::{Mutex, RwLock};
use tokio::sync::{Notify, watch};

use super::categories::{self, CategoryMap};
use super::engine::{Engine, EngineHandle};
use super::metainfo::{self, TorrentMeta};
use super::peers::{HostCache, PeerFlags, determine_flags};
use super::resume::{self, BackupDir, SessionStateFile, StatsFile};
use super::speed::SpeedHistory;
use super::sync::MainDataSnapshot;
use super::torrent::{Env, TorrentEntry};
use super::{bencode, state, trackers};
use crate::context::CoreContext;
use crate::error::{QbtError, QbtResult};
use crate::events::CoreEvent;
use crate::model::*;
use crate::net::geoip::{self, GeoIp};
use crate::prefs::Preferences;

/// A peer source that can be switched off per torrent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoverySource {
    Dht,
    Pex,
    Lsd,
}

/// Registry state guarded by one lock.
pub(super) struct State {
    pub torrents: HashMap<String, TorrentEntry>,
    pub categories: CategoryMap,
    pub tags: BTreeSet<String>,
}

/// Small session-wide values that change on the timer.
#[derive(Default)]
pub(super) struct Misc {
    pub speed: SpeedHistory,
    pub stats_base: StatsFile,
    pub session_download: i64,
    pub session_upload: i64,
    pub engine_fetched: u64,
    pub engine_uploaded: u64,
    pub stats_dirty: bool,
    pub dl_rate: i64,
    pub ul_rate: i64,
    pub peers_live: i64,
    pub dht_nodes: i64,
    pub engine_total_down: u64,
    pub engine_total_up: u64,
    pub engine_wasted: u64,
    pub session_total_download: i64,
    pub session_total_upload: i64,
    pub session_wasted: i64,
    pub hosts: HostCache,
    /// Recently fetched metadata (id -> .torrent bytes), newest last.
    pub metadata_cache: VecDeque<(String, Arc<Vec<u8>>)>,
    pub free_space: i64,
    pub has_incoming: bool,
    pub trackers_from_url: Vec<TrackerTierEntry>,
    pub watched_retries: HashMap<PathBuf, u32>,
    pub restart_pending: Option<(Instant, String)>,
    pub scheduler_last: Option<bool>,
    pub ip_filter_rules: usize,
    /// (source key, parsed IP filter file).
    pub ip_filter_cache: Option<(String, Option<Arc<crate::ipfilter::IpFilter>>)>,
    pub external_v4: String,
    pub external_v6: String,
    pub mapping_error: Option<String>,
}

const METADATA_CACHE_CAP: usize = 4;

pub struct Session {
    pub(super) ctx: CoreContext,
    pub(super) state: RwLock<State>,
    pub(super) engine: RwLock<Option<Arc<Engine>>>,
    pub(super) engine_gen: AtomicU64,
    /// Serialises engine restarts.
    pub(super) engine_lock: tokio::sync::Mutex<()>,
    /// Storage moves run one at a time (qBittorrent's move queue).
    pub(super) move_lock: tokio::sync::Mutex<()>,
    pub(super) misc: Mutex<Misc>,
    pub(super) geoip: Arc<GeoIp>,
    pub(super) backup: BackupDir,
    pub(super) alt_speed: AtomicBool,
    /// Session-level pause (`SessionImpl::m_isPaused`): runtime state, never
    /// persisted; torrents keep their own stopped/started state.
    pub(super) paused: AtomicBool,
    pub(super) wake: Notify,
    pub(super) shutdown_tx: watch::Sender<bool>,
    pub(super) me: OnceLock<Weak<Session>>,
}

pub(super) fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Session {
    pub async fn launch(ctx: &CoreContext) -> QbtResult<Arc<Self>> {
        let profile = ctx.profile.clone();
        let prefs = ctx.prefs.current();
        let backup = BackupDir::new(profile.bt_backup_dir());
        std::fs::create_dir_all(profile.bt_backup_dir())?;
        std::fs::create_dir_all(profile.engine_dir())?;

        let (mut cats, tags, stats, session_state, records) = {
            let profile = profile.clone();
            let backup = backup.clone();
            tokio::task::spawn_blocking(move || {
                let cats = match resume::read_limited(&profile.categories_file(), 1024 * 1024) {
                    Ok(data) => categories::from_json(&data).map_err(|e| (profile.categories_file(), e)),
                    Err(_) => Ok(CategoryMap::new()),
                };
                let tags: Vec<String> = resume::load_json(&profile.config_dir.join("tags.json"));
                let stats: StatsFile = resume::load_json(&profile.stats_file());
                let st: SessionStateFile = resume::load_json(&profile.data_dir.join("session_state.json"));
                let records = backup.load_all();
                (cats, tags, stats, st, records)
            })
            .await
            .map_err(|e| QbtError::other(e.to_string()))?
        };
        let logger = &ctx.logger;
        let cats = match cats.as_mut() {
            Ok(c) => std::mem::take(c),
            Err((file, e)) => {
                logger.warning(format!(
                    "Failed to parse Categories configuration. File: \"{}\". Error: \"{e}\"",
                    file.display()
                ));
                CategoryMap::new()
            }
        };
        let tags: BTreeSet<String> = tags.into_iter().filter(|t| categories::is_valid_tag(t)).map(|t| t.trim().to_string()).collect();

        let session = Arc::new(Self {
            ctx: ctx.clone(),
            state: RwLock::new(State { torrents: HashMap::new(), categories: cats, tags }),
            engine: RwLock::new(None),
            engine_gen: AtomicU64::new(0),
            engine_lock: tokio::sync::Mutex::new(()),
            move_lock: tokio::sync::Mutex::new(()),
            misc: Mutex::new(Misc { stats_base: stats, free_space: -1, ..Default::default() }),
            geoip: GeoIp::launch(ctx),
            backup,
            alt_speed: AtomicBool::new(session_state.use_alt_speed_limits),
            // `StartPaused`: set before any torrent is attached to the engine.
            paused: AtomicBool::new(prefs.advanced.start_paused),
            wake: Notify::new(),
            shutdown_tx: watch::channel(false).0,
            me: OnceLock::new(),
        });
        let _ = session.me.set(Arc::downgrade(&session));

        session.restore_records(records.0, records.1, &prefs).await;

        logger.info("Peer ID: \"-NR0220-\"");
        logger.info(format!("HTTP User-Agent: \"{}\"", super::policy::client_name()));
        let onoff = |b: bool| if b { "ON" } else { "OFF" };
        logger.info(format!("Distributed Hash Table (DHT) support: {}", onoff(prefs.bittorrent.dht)));
        logger.info(format!("Local Peer Discovery support: {}", onoff(prefs.bittorrent.lsd)));
        logger.info(format!("Peer Exchange (PeX) support: {}", onoff(prefs.bittorrent.pex)));
        logger.info(format!("Anonymous mode: {}", onoff(prefs.bittorrent.anonymous_mode)));
        logger.info(format!(
            "Encryption support: {}",
            match prefs.bittorrent.encryption {
                crate::prefs::EncryptionMode::Allow => "ON",
                crate::prefs::EncryptionMode::Require => "FORCED",
                crate::prefs::EncryptionMode::Disable => "OFF",
            }
        ));

        session.start_engine().await;
        session.clone().spawn_background();
        Ok(session)
    }

    pub(super) fn arc(&self) -> Option<Arc<Session>> {
        self.me.get().and_then(Weak::upgrade)
    }

    pub(super) fn engine(&self) -> Option<Arc<Engine>> {
        self.engine.read().clone()
    }

    pub(super) fn limits(&self) -> bencode::Limits {
        let p = self.ctx.prefs.current();
        bencode::Limits {
            max_depth: p.advanced.bdecode_depth_limit.clamp(1, 1000) as usize,
            max_tokens: p.advanced.bdecode_token_limit.clamp(1, 100_000_000) as usize,
        }
    }

    pub(super) fn torrent_file_limit(&self) -> u64 {
        self.ctx.prefs.current().advanced.torrent_file_size_limit.clamp(1024, 1024 * 1024 * 1024) as u64
    }

    /// Wakes the session timer early (after actions that need engine work).
    pub(super) fn kick(&self) {
        self.wake.notify_one();
    }

    async fn restore_records(&self, records: Vec<resume::FastResume>, failed: Vec<(String, String)>, prefs: &Preferences) {
        let logger = &self.ctx.logger;
        for (file, reason) in failed {
            logger.critical(format!("Failed to resume torrent. Torrent: \"{file}\". Reason: \"{reason}\""));
        }
        let limits = self.limits();
        let file_limit = self.torrent_file_limit();
        let backup = self.backup.clone();
        let (loaded, migrated) = tokio::task::spawn_blocking(move || {
            let mut migrated = Vec::new();
            let loaded = records
                .into_iter()
                .map(|mut r| {
                    let bytes = backup.load_torrent_file(&r.info_hash.to_ascii_lowercase(), file_limit).ok();
                    let meta = bytes.as_ref().map(|b| metainfo::parse_torrent(b, limits).map(|p| Arc::new(p.meta)));
                    if let Some(new_id) = resume::migrate_id(&backup, &mut r, meta.as_ref().and_then(|m| m.as_ref().ok()), bytes.as_deref()) {
                        migrated.push(new_id);
                    }
                    (r, meta)
                })
                .collect::<Vec<_>>();
            (loaded, migrated)
        })
        .await
        .unwrap_or_default();
        if !migrated.is_empty() {
            logger.info(format!("Moved the resume data of {} hybrid torrent(s) to their v2-based IDs", migrated.len()));
        }

        let mut st = self.state.write();
        let mut next_queue = 0i64;
        for (r, meta) in loaded {
            let id = r.info_hash.to_ascii_lowercase();
            if metainfo::parse_hex20(&id).is_none() || st.torrents.contains_key(&id) {
                continue;
            }
            let meta = match meta {
                Some(Ok(m)) if m.id() == id => Some(m),
                Some(Ok(_)) => {
                    logger.critical(format!(
                        "Failed to resume torrent. Torrent: \"{id}\". Reason: \"Mismatching info-hash detected in resume data\""
                    ));
                    continue;
                }
                Some(Err(e)) => {
                    logger.critical(format!("Failed to resume torrent. Torrent: \"{id}\". Reason: \"{e}\""));
                    continue;
                }
                None => None,
            };
            let mut e = TorrentEntry::from_resume(r, meta);
            if !categories::is_valid_category_name(&e.category) {
                logger.warning(format!(
                    "Detected inconsistent data: invalid category. Torrent: \"{}\". Category: \"{}\"",
                    e.display_name(),
                    e.category
                ));
                e.category.clear();
            } else if !e.category.is_empty() && !st.categories.contains_key(&e.category) {
                logger.warning(format!(
                    "Detected inconsistent data: category is missing from the configuration file. Category will be recovered but its settings will be reset to default. Torrent: \"{}\". Category: \"{}\"",
                    e.display_name(),
                    e.category
                ));
                for c in categories::expand_category(&e.category) {
                    st.categories.entry(c.clone()).or_insert_with(|| categories::new_category(&c));
                }
            }
            e.tags.retain(|t| categories::is_valid_tag(t));
            for t in &e.tags {
                if !st.tags.contains(t) {
                    logger.warning(format!(
                        "Detected inconsistent data: tag is missing from the configuration file. Tag will be recovered. Torrent: \"{}\". Tag: \"{t}\"",
                        e.display_name()
                    ));
                    st.tags.insert(t.clone());
                }
            }
            // Queue positions are renumbered densely in stored order.
            e.queue_position = if e.has_metadata() && e.seed_status { -1 } else { next_queue };
            if e.queue_position >= 0 {
                next_queue += 1;
            }
            e.rt.restoring = e.has_metadata();
            logger.info(format!("Restored torrent. Torrent: \"{}\"", e.display_name()));
            st.torrents.insert(id, e);
        }
        // The engine names its fast-resume files after its own identity (the
        // v1 hash of a hybrid torrent).
        let keep: HashSet<String> = st
            .torrents
            .iter()
            .flat_map(|(id, e)| std::iter::once(id.clone()).chain(e.meta.as_ref().map(|m| m.engine_hash_hex())))
            .collect();
        drop(st);
        // Small directory scan at start-up; no need for a blocking task.
        Engine::prune_fastresume(&self.ctx.profile.engine_dir(), &keep);
        let _ = prefs;
        self.save_categories();
        self.save_tags();
    }

    pub async fn shutdown(&self) {
        self.shutdown_tx.send_replace(true);
        let records: Vec<resume::FastResume> = {
            let mut st = self.state.write();
            for e in st.torrents.values_mut() {
                if let Some(t) = e.rt.metadata_task.take() {
                    t.abort();
                }
            }
            st.torrents.values().map(|e| e.to_resume()).collect()
        };
        let backup = self.backup.clone();
        let stats = self.stats_file();
        let stats_path = self.ctx.profile.stats_file();
        let state_path = self.ctx.profile.data_dir.join("session_state.json");
        let alt = self.alt_speed.load(Ordering::Relaxed);
        let _ = tokio::task::spawn_blocking(move || {
            for r in &records {
                let _ = backup.save_resume(r);
            }
            let _ = resume::save_json(&stats_path, &stats);
            let _ = resume::save_json(&state_path, &SessionStateFile { use_alt_speed_limits: alt });
        })
        .await;
        self.save_categories();
        self.save_tags();
        let engine = self.engine.write().take();
        if let Some(engine) = engine {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), engine.stop()).await;
        }
        self.ctx.logger.info("BitTorrent session successfully finished.");
    }

    pub(super) fn stats_file(&self) -> StatsFile {
        let m = self.misc.lock();
        StatsFile {
            alltime_dl: m.stats_base.alltime_dl + m.session_total_download,
            alltime_ul: m.stats_base.alltime_ul + m.session_total_upload,
        }
    }

    pub(super) fn save_categories(&self) {
        let data = categories::to_json(&self.state.read().categories);
        let path = self.ctx.profile.categories_file();
        if let Err(e) = resume::write_private_atomic(&path, &data) {
            self.ctx.logger.warning(format!(
                "Failed to save Categories configuration. File: \"{}\". Error: \"{e}\"",
                path.display()
            ));
        }
    }

    pub(super) fn save_tags(&self) {
        let tags: Vec<String> = self.state.read().tags.iter().cloned().collect();
        let _ = resume::save_json(&self.ctx.profile.config_dir.join("tags.json"), &tags);
    }

    pub(super) fn emit(&self, event: CoreEvent) {
        self.ctx.events.emit(event);
    }

    /// Runs `f` on each existing torrent among `hashes` (lower-cased).
    pub(super) fn for_each<F>(&self, hashes: &[String], mut f: F) -> Vec<String>
    where
        F: FnMut(&mut TorrentEntry, &Env),
    {
        let prefs = self.ctx.prefs.current();
        let mut st = self.state.write();
        let State { torrents, categories, .. } = &mut *st;
        let env = Env::new(&prefs, categories);
        let mut done = Vec::new();
        for h in hashes {
            let id = h.trim().to_ascii_lowercase();
            if let Some(e) = torrents.get_mut(&id) {
                f(e, &env);
                e.rt.dirty = true;
                done.push(id);
            }
        }
        done
    }

    // ---- Snapshots ----------------------------------------------------------

    pub fn main_data_snapshot(&self) -> MainDataSnapshot {
        self.main_data_snapshot_since(None)
    }

    /// Like [`Self::main_data_snapshot`], but the tracker map is only built
    /// when the fingerprint of all (torrent, tracker) pairs differs from
    /// `known_trackers_fp` (what the caller last received).
    pub fn main_data_snapshot_since(&self, known_trackers_fp: Option<u64>) -> MainDataSnapshot {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let prefs = self.ctx.prefs.current();
        let st = self.state.read();
        let env = Env::new(&prefs, &st.categories);
        let mut torrents = Vec::with_capacity(st.torrents.len());
        let mut trackers_fp = 0u64;
        for e in st.torrents.values() {
            torrents.push(e.info(&env));
            for t in &e.trackers {
                let mut h = DefaultHasher::new();
                (&e.id, &t.url).hash(&mut h);
                trackers_fp = trackers_fp.wrapping_add(h.finish());
            }
        }
        let trackers_unchanged = known_trackers_fp == Some(trackers_fp);
        let mut trackers: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if !trackers_unchanged {
            for e in st.torrents.values() {
                for t in &e.trackers {
                    trackers.entry(t.url.clone()).or_default().push(e.id.clone());
                }
            }
            for v in trackers.values_mut() {
                v.sort();
            }
        }
        let categories = st.categories.values().cloned().collect();
        let tags = st.tags.iter().cloned().collect();
        drop(st);
        MainDataSnapshot { torrents, categories, tags, trackers, trackers_fp, trackers_unchanged, server_state: self.server_state() }
    }

    pub fn torrent_infos(&self) -> Vec<TorrentInfo> {
        let prefs = self.ctx.prefs.current();
        let st = self.state.read();
        let env = Env::new(&prefs, &st.categories);
        st.torrents.values().map(|e| e.info(&env)).collect()
    }

    pub fn torrent_infos_filtered(&self, query: &TorrentQuery) -> Vec<TorrentInfo> {
        let prefs = self.ctx.prefs.current();
        let st = self.state.read();
        let env = Env::new(&prefs, &st.categories);
        let hashes: HashSet<String> = query.hashes.iter().map(|h| h.trim().to_ascii_lowercase()).collect();
        st.torrents
            .values()
            .filter(|e| hashes.is_empty() || hashes.contains(&e.id))
            .filter(|e| match &query.category {
                None => true,
                Some(c) if c.is_empty() => e.category.is_empty(),
                Some(c) => e.belongs_to_category(c),
            })
            .filter(|e| match &query.tag {
                None => true,
                Some(t) if t.trim().is_empty() => e.tags.is_empty(),
                Some(t) => e.tags.iter().any(|x| x == t.trim()),
            })
            .filter(|e| query.private.is_none_or(|p| e.has_metadata() && e.is_private() == p))
            .filter(|e| state::matches_status(query.status, e.state(), e.stopped, e.dl_rate(), e.ul_rate()))
            .map(|e| e.info(&env))
            .collect()
    }

    pub fn torrent_info(&self, hash: &str) -> QbtResult<TorrentInfo> {
        self.with_torrent(hash, |e, env| e.info(env))
    }

    pub fn torrent_count(&self) -> usize {
        self.state.read().torrents.len()
    }

    pub(super) fn with_torrent<R>(&self, hash: &str, f: impl FnOnce(&TorrentEntry, &Env) -> R) -> QbtResult<R> {
        let prefs = self.ctx.prefs.current();
        let st = self.state.read();
        let env = Env::new(&prefs, &st.categories);
        let id = hash.trim().to_ascii_lowercase();
        let e = st.torrents.get(&id).ok_or_else(|| QbtError::not_found(format!("torrent {hash} not found")))?;
        Ok(f(e, &env))
    }

    pub(super) fn with_torrent_mut<R>(&self, hash: &str, f: impl FnOnce(&mut TorrentEntry, &Env) -> R) -> QbtResult<R> {
        let prefs = self.ctx.prefs.current();
        let mut st = self.state.write();
        let State { torrents, categories, .. } = &mut *st;
        let env = Env::new(&prefs, categories);
        let id = hash.trim().to_ascii_lowercase();
        let e = torrents.get_mut(&id).ok_or_else(|| QbtError::not_found(format!("torrent {hash} not found")))?;
        e.rt.dirty = true;
        Ok(f(e, &env))
    }

    // ---- Properties tabs ---------------------------------------------------

    pub fn properties(&self, hash: &str) -> QbtResult<TorrentProperties> {
        self.with_torrent(hash, |e, env| e.properties(env))
    }

    pub fn trackers(&self, hash: &str) -> QbtResult<Vec<TrackerEntry>> {
        let prefs = self.ctx.prefs.current();
        let (handle, private, list, flags) = self.with_torrent(hash, |e, _| {
            (
                e.rt.engine.as_ref().map(|(h, _)| h.clone()),
                e.is_private(),
                e.trackers.clone(),
                (e.disable_dht, e.disable_pex, e.disable_lsd),
            )
        })?;
        let engine = self.engine();
        // Seeds/leechers per peer source for the DHT/PeX/LSD rows.
        let mut counts = [(0i64, 0i64); 3];
        let mut statuses = Vec::new();
        if let (Some(h), Some(engine)) = (&handle, &engine) {
            for p in engine.peers(h) {
                for (i, src) in [librqbit::PeerSource::Dht, librqbit::PeerSource::Pex, librqbit::PeerSource::Lsd].iter().enumerate() {
                    if super::engine::has_source(&p, *src) {
                        if p.flags.seed {
                            counts[i].0 += 1;
                        } else {
                            counts[i].1 += 1;
                        }
                    }
                }
            }
            statuses = engine.trackers(h).statuses;
        }
        // Like qBittorrent, the DHT row is "working" only once the routing
        // table has nodes (PeX and LSD have no such state).
        let dht_nodes = self.misc.lock().dht_nodes;
        let pseudo = |url: &str, enabled: bool, disabled_for_torrent: bool, c: (i64, i64), working: bool| TrackerEntry {
            url: url.to_string(),
            tier: -1,
            status: if !enabled || private || disabled_for_torrent {
                TrackerStatus::Disabled
            } else if working {
                TrackerStatus::Working
            } else {
                TrackerStatus::NotWorking
            },
            message: if private { "This torrent is private".into() } else { String::new() },
            num_peers: 0,
            num_seeds: c.0,
            num_leeches: c.1,
            num_downloaded: 0,
            ..Default::default()
        };
        let mut out = vec![
            pseudo("** [DHT] **", prefs.bittorrent.dht, flags.0, counts[0], dht_nodes > 0),
            pseudo("** [PeX] **", prefs.bittorrent.pex, flags.1, counts[1], true),
            pseudo("** [LSD] **", prefs.bittorrent.lsd, flags.2, counts[2], true),
        ];
        let now = now_secs();
        let opt = |v: Option<u32>| v.map(|v| v as i64).unwrap_or(-1);
        let at = |d: Option<std::time::Duration>| d.map(|d| now + d.as_secs() as i64).unwrap_or(0);
        if handle.is_some() && !statuses.is_empty() {
            for t in statuses {
                out.push(TrackerEntry {
                    url: t.url.clone(),
                    tier: t.tier as i64,
                    status: super::engine::tracker_state(t.state),
                    updating: t.updating,
                    message: t.message.clone(),
                    num_peers: opt(t.peers),
                    num_seeds: opt(t.seeds),
                    num_leeches: opt(t.leechers),
                    num_downloaded: opt(t.downloaded),
                    next_announce: at(t.next_announce_in),
                    min_announce: at(t.min_announce_in),
                    endpoints: t
                        .endpoints
                        .iter()
                        .map(|ep| TrackerEndpoint {
                            name: format!("{:?}", ep.family),
                            updating: ep.updating,
                            status: super::engine::tracker_state(ep.state),
                            message: ep.message.clone(),
                            bt_version: 1,
                            num_peers: opt(ep.peers),
                            num_seeds: opt(ep.seeds),
                            num_leeches: opt(ep.leechers),
                            num_downloaded: opt(ep.downloaded),
                            next_announce: at(ep.next_announce_in),
                            min_announce: at(ep.min_announce_in),
                        })
                        .collect(),
                });
            }
        } else {
            for t in list {
                out.push(TrackerEntry {
                    url: t.url,
                    tier: t.tier,
                    status: TrackerStatus::NotContacted,
                    num_peers: -1,
                    num_seeds: -1,
                    num_leeches: -1,
                    num_downloaded: -1,
                    ..Default::default()
                });
            }
        }
        Ok(out)
    }

    pub fn web_seeds(&self, hash: &str) -> QbtResult<Vec<WebSeed>> {
        let urls = match self.web_seed_engine(hash) {
            Some((engine, h)) => engine.web_seed_urls(&h),
            None => self.with_torrent(hash, |e, _| e.url_seeds.clone())?,
        };
        Ok(urls.into_iter().map(|url| WebSeed { url }).collect())
    }

    /// The engine and handle of an attached torrent, to apply web seed
    /// changes to its live scheduler.
    fn web_seed_engine(&self, hash: &str) -> Option<(Arc<Engine>, EngineHandle)> {
        let id = hash.trim().to_ascii_lowercase();
        Some((self.engine()?, self.handle_of(&id)?))
    }

    pub fn files(&self, hash: &str, indexes: &[i64]) -> QbtResult<Vec<TorrentFile>> {
        let handle = self.with_torrent(hash, |e, _| e.rt.engine.as_ref().map(|(h, _)| h.clone()))?;
        let avail = match (handle, self.engine()) {
            (Some(h), Some(engine)) => engine.piece_availability(&h),
            _ => Vec::new(),
        };
        self.with_torrent(hash, |e, _| e.files(indexes, &avail))?.map_err(QbtError::conflict)
    }

    pub fn piece_states(&self, hash: &str) -> QbtResult<Vec<u8>> {
        let (handle, meta, progress) = self.with_torrent(hash, |e, _| {
            (e.rt.engine.as_ref().map(|(h, _)| h.clone()), e.meta.clone(), e.rt.file_progress.clone())
        })?;
        let Some(meta) = meta else { return Ok(Vec::new()) };
        let pieces = meta.pieces as usize;
        if let (Some(h), Some(engine)) = (handle, self.engine()) {
            let states = engine.piece_states(&h);
            if states.len() == pieces {
                return Ok(states);
            }
        }
        // While checking or detached: pieces fully inside completed files.
        let mut out = vec![0u8; pieces];
        for (f, have) in meta.files.iter().zip(progress.iter()) {
            if *have >= f.size && f.size > 0 {
                for p in f.first_piece..=f.last_piece {
                    if let Some(x) = out.get_mut(p as usize) {
                        *x = 2;
                    }
                }
            }
        }
        Ok(out)
    }

    pub fn piece_availability(&self, hash: &str) -> QbtResult<Vec<u32>> {
        let (handle, pieces) = self.with_torrent(hash, |e, _| {
            (e.rt.engine.as_ref().map(|(h, _)| h.clone()), e.meta.as_ref().map(|m| m.pieces as usize).unwrap_or(0))
        })?;
        if let (Some(h), Some(engine)) = (handle, self.engine()) {
            let a = engine.piece_availability(&h);
            if a.len() == pieces {
                return Ok(a);
            }
        }
        Ok(vec![0; pieces])
    }

    pub fn piece_hashes(&self, hash: &str) -> QbtResult<Vec<String>> {
        let (id, handle, has_meta) =
            self.with_torrent(hash, |e, _| (e.id.clone(), e.rt.engine.as_ref().map(|(h, _)| h.clone()), e.has_metadata()))?;
        if !has_meta {
            return Ok(Vec::new());
        }
        if let (Some(h), Some(engine)) = (handle, self.engine())
            && let Some(hashes) = engine.piece_hashes(&h)
        {
            return Ok(hashes);
        }
        let bytes = self.backup.load_torrent_file(&id, self.torrent_file_limit())?;
        let limits = self.limits();
        let (s, e) = bencode::find_top_level_span(&bytes, b"info", limits).ok_or_else(|| QbtError::other("invalid metadata"))?;
        let info = bencode::decode(&bytes[s..e], limits).map_err(|e| QbtError::other(e.to_string()))?;
        Ok(info.get("pieces").and_then(|p| p.as_bytes()).map(|p| p.chunks(20).map(metainfo::hex).collect()).unwrap_or_default())
    }

    pub fn peers(&self, hash: &str) -> QbtResult<Vec<PeerInfo>> {
        let prefs = self.ctx.prefs.current();
        let (handle, meta, paths) = self.with_torrent(hash, |e, _| {
            (e.rt.engine.as_ref().map(|(h, _)| h.clone()), e.meta.clone(), e.file_paths.clone())
        })?;
        let (Some(h), Some(engine)) = (handle, self.engine()) else { return Ok(Vec::new()) };
        let peers = engine.peers(&h);
        let resolve_hosts = prefs.advanced.resolve_peer_host_names;
        let resolve_countries = prefs.advanced.resolve_peer_countries;
        // Engine file index -> user path.
        let names: HashMap<usize, String> = meta
            .as_ref()
            .map(|m| m.files.iter().enumerate().map(|(i, f)| (f.engine_index as usize, paths.get(i).cloned().unwrap_or_default())).collect())
            .unwrap_or_default();
        let mut to_resolve = Vec::new();
        let mut out = Vec::with_capacity(peers.len());
        {
            let mut misc = self.misc.lock();
            if !resolve_hosts {
                misc.hosts.clear();
            }
            for p in &peers {
                let ip = p.addr.ip();
                let f = &p.flags;
                let (flags, flags_description) = determine_flags(&PeerFlags {
                    interesting: f.am_interested,
                    remote_choked: f.peer_choking,
                    remote_interested: f.peer_interested,
                    choked: f.am_choking,
                    optimistic_unchoke: f.optimistic_unchoke,
                    snubbed: f.snubbed,
                    incoming: f.incoming,
                    from_dht: super::engine::has_source(p, librqbit::PeerSource::Dht),
                    from_pex: super::engine::has_source(p, librqbit::PeerSource::Pex),
                    from_lsd: super::engine::has_source(p, librqbit::PeerSource::Lsd),
                    rc4_encrypted: f.encrypted,
                    plaintext_encrypted: f.encrypted_handshake,
                    utp: f.utp || super::engine::is_utp(p.connection),
                    holepunched: f.holepunched,
                });
                let i2p = super::engine::is_i2p(p.connection);
                let host_name = if resolve_hosts && !i2p {
                    match misc.hosts.get(&ip) {
                        Some(h) => h.to_string(),
                        None => {
                            if misc.hosts.should_resolve(ip) {
                                to_resolve.push(ip);
                            }
                            String::new()
                        }
                    }
                } else {
                    String::new()
                };
                let (country_code, country) = if resolve_countries && !i2p {
                    match self.geoip.country_code(ip) {
                        Some(c) => {
                            let name = geoip::country_name(&c);
                            (c.to_ascii_lowercase(), name)
                        }
                        None => (String::new(), String::new()),
                    }
                } else {
                    (String::new(), String::new())
                };
                out.push(PeerInfo {
                    ip: ip.to_string(),
                    port: p.addr.port(),
                    client: p.client.chars().filter(|c| !c.is_control()).collect(),
                    peer_id_client: super::peers::peer_id_client(&p.peer_id),
                    connection_type: if i2p {
                        "I2P".into()
                    } else if f.utp || super::engine::is_utp(p.connection) {
                        "μTP".into()
                    } else {
                        "BT".into()
                    },
                    country,
                    country_code,
                    dl_speed: p.download_rate as i64,
                    up_speed: p.upload_rate as i64,
                    downloaded: p.downloaded as i64,
                    uploaded: p.uploaded as i64,
                    progress: p.progress,
                    relevance: p.relevance,
                    flags,
                    flags_description,
                    files: p.downloading_files.iter().filter_map(|i| names.get(i).cloned()).collect::<Vec<_>>().join("\n"),
                    host_name,
                });
            }
        }
        if !to_resolve.is_empty()
            && let Some(me) = self.arc()
        {
            self.ctx.rt.spawn(async move {
                for ip in to_resolve {
                    let name = tokio::task::spawn_blocking(move || super::peers::reverse_lookup(ip)).await.ok().flatten();
                    me.misc.lock().hosts.insert(ip, name.unwrap_or_default());
                }
            });
        }
        Ok(out)
    }

    pub fn export_torrent(&self, hash: &str) -> QbtResult<Vec<u8>> {
        let (id, has_meta, trackers, seeds, comment, meta) = self.with_torrent(hash, |e, _| {
            (e.id.clone(), e.has_metadata(), e.trackers.clone(), e.url_seeds.clone(), e.comment(), e.meta.clone())
        })?;
        if !has_meta {
            return Err(QbtError::conflict("Unable to export torrent file. Error: Missing metadata"));
        }
        let bytes = self.backup.load_torrent_file(&id, self.torrent_file_limit())?;
        let (s, e) = bencode::find_top_level_span(&bytes, b"info", self.limits())
            .ok_or_else(|| QbtError::conflict("Unable to export torrent file. Error: invalid metadata"))?;
        let m = meta.as_deref();
        let layers = bencode::find_top_level_span(&bytes, b"piece layers", self.limits()).map(|(ls, le)| &bytes[ls..le]);
        Ok(metainfo::build_torrent_file(
            &bytes[s..e],
            layers,
            &trackers,
            &seeds,
            &comment,
            m.map(|m| m.created_by.as_str()).unwrap_or(""),
            m.map(|m| m.creation_date).unwrap_or(-1),
        ))
    }

    pub(super) fn cache_metadata(&self, id: &str, bytes: Arc<Vec<u8>>) {
        let mut m = self.misc.lock();
        m.metadata_cache.retain(|(i, _)| i != id);
        if m.metadata_cache.len() >= METADATA_CACHE_CAP {
            m.metadata_cache.pop_front();
        }
        m.metadata_cache.push_back((id.to_string(), bytes));
    }

    pub(super) fn cached_metadata(&self, id: &str) -> Option<Arc<Vec<u8>>> {
        self.misc.lock().metadata_cache.iter().find(|(i, _)| i == id).map(|(_, b)| b.clone())
    }

    pub fn fetched_metadata_torrent(&self, hash: &str) -> QbtResult<Vec<u8>> {
        let id = hash.trim().to_ascii_lowercase();
        if let Some(b) = self.cached_metadata(&id) {
            return Ok(b.as_ref().clone());
        }
        if self.state.read().torrents.get(&id).is_some_and(|e| e.has_metadata()) {
            return self.export_torrent(&id);
        }
        Err(QbtError::not_found("Metadata is not yet available"))
    }

    // ---- Simple per-torrent settings --------------------------------------

    pub fn rename(&self, hash: &str, name: &str) -> QbtResult<()> {
        let name = name.replace("\r\n", " ").replace(['\r', '\n'], " ").trim().to_string();
        if name.is_empty() {
            return Err(QbtError::conflict("Incorrect torrent name"));
        }
        let name: String = name.chars().filter(|c| !c.is_control()).collect();
        self.with_torrent_mut(hash, |e, _| e.name = name)
    }

    pub fn set_comment(&self, hashes: &[String], comment: &str) -> QbtResult<()> {
        let comment = comment.trim().to_string();
        self.for_each(hashes, |e, _| e.comment = Some(comment.clone()));
        Ok(())
    }

    pub fn set_file_priority(&self, hash: &str, indexes: &[i64], priority: FilePriority) -> QbtResult<()> {
        let r = self.with_torrent_mut(hash, |e, _| {
            let Some(meta) = e.meta.clone() else {
                return Err(QbtError::conflict("Torrent's metadata has not yet downloaded"));
            };
            let n = meta.files.len() as i64;
            if let Some(bad) = indexes.iter().find(|&&i| i < 0 || i >= n) {
                return Err(QbtError::conflict(format!("File ID is not valid: {bad}")));
            }
            let mut changed = false;
            for &i in indexes {
                let i = i as usize;
                let old = e.file_priorities[i];
                if old == priority {
                    continue;
                }
                changed = true;
                let size = meta.files[i].size;
                let complete = e.rt.file_progress.get(i).is_some_and(|&h| h >= size);
                if old == FilePriority::Ignored && !complete {
                    // An incomplete file is wanted again: not finished anymore.
                    e.seed_status = false;
                    e.rt.finished = false;
                }
                e.file_priorities[i] = priority;
            }
            if changed && !e.seed_status && e.queue_position < 0 {
                e.queue_position = i64::MAX;
            }
            Ok(changed)
        })??;
        if r {
            self.push_settings(&[hash.trim().to_ascii_lowercase()]);
            self.normalize_queue();
            self.kick();
        }
        Ok(())
    }

    /// Applies per-torrent engine settings (priorities, limits, piece
    /// picking switches) to attached torrents right away.
    pub(super) fn push_settings(&self, ids: &[String]) {
        let Some(engine) = self.engine() else { return };
        let prefs = self.ctx.prefs.current();
        let work: Vec<_> = {
            let st = self.state.read();
            ids.iter()
                .filter_map(|id| st.torrents.get(id))
                .filter_map(|e| {
                    let (h, g) = e.rt.engine.clone()?;
                    (g == engine.generation).then(|| (h, super::actions::torrent_settings(e, &prefs)))
                })
                .collect()
        };
        for (h, s) in work {
            engine.apply_torrent_settings(&h, &s);
        }
    }

    pub fn set_download_limit(&self, hashes: &[String], limit: i64) -> QbtResult<()> {
        let limit = if limit <= 0 || limit >= i32::MAX as i64 { 0 } else { limit };
        let mut changed = Vec::new();
        self.for_each(hashes, |e, _| {
            if e.dl_limit != limit {
                e.dl_limit = limit;
                changed.push(e.id.clone());
            }
        });
        self.push_settings(&changed);
        Ok(())
    }

    pub fn set_upload_limit(&self, hashes: &[String], limit: i64) -> QbtResult<()> {
        let limit = if limit <= 0 || limit >= i32::MAX as i64 { 0 } else { limit };
        let mut changed = Vec::new();
        self.for_each(hashes, |e, _| {
            if e.ul_limit != limit {
                e.ul_limit = limit;
                changed.push(e.id.clone());
            }
        });
        self.push_settings(&changed);
        Ok(())
    }

    pub fn set_share_limits(
        &self,
        hashes: &[String],
        ratio_limit: f64,
        seeding_time_limit: i64,
        inactive_seeding_time_limit: i64,
        action: ShareLimitAction,
    ) -> QbtResult<()> {
        let ratio = if ratio_limit < -2.0 || !ratio_limit.is_finite() { -1.0 } else { ratio_limit };
        let seed = seeding_time_limit.max(-2);
        let inactive = inactive_seeding_time_limit.max(-2);
        self.for_each(hashes, |e, _| {
            e.ratio_limit = ratio;
            e.seeding_time_limit = seed;
            e.inactive_seeding_time_limit = inactive;
            e.share_limit_action = action;
        });
        Ok(())
    }

    pub fn set_super_seeding(&self, hashes: &[String], value: bool) -> QbtResult<()> {
        let ids = self.for_each(hashes, |e, _| {
            if e.has_metadata() {
                e.super_seeding = value;
            }
        });
        self.push_settings(&ids);
        Ok(())
    }

    /// The per-torrent DHT/PeX/LSD switches of the given torrents (unknown
    /// hashes are skipped).
    pub fn discovery_options(&self, hashes: &[String]) -> Vec<TorrentDiscoveryOptions> {
        let st = self.state.read();
        hashes
            .iter()
            .filter_map(|h| st.torrents.get(&h.trim().to_ascii_lowercase()))
            .map(|e| TorrentDiscoveryOptions {
                hash: e.id.clone(),
                private: e.is_private(),
                disable_dht: e.disable_dht,
                disable_pex: e.disable_pex,
                disable_lsd: e.disable_lsd,
            })
            .collect()
    }

    /// Switch DHT, PeX or LSD off (or back on) for torrents, applied to the
    /// engine at once and persisted. Like qBittorrent's Torrent Options
    /// dialog, private torrents are left alone (they never use them).
    pub fn set_discovery_disabled(&self, hashes: &[String], source: DiscoverySource, value: bool) -> QbtResult<()> {
        let ids = self.for_each(hashes, |e, _| {
            if e.is_private() {
                return;
            }
            match source {
                DiscoverySource::Dht => e.disable_dht = value,
                DiscoverySource::Pex => e.disable_pex = value,
                DiscoverySource::Lsd => e.disable_lsd = value,
            }
        });
        self.push_settings(&ids);
        Ok(())
    }

    pub fn set_sequential_download(&self, hashes: &[String], value: bool) -> QbtResult<()> {
        let ids = self.for_each(hashes, |e, _| e.sequential = value);
        self.push_settings(&ids);
        Ok(())
    }

    pub fn set_first_last_piece_priority(&self, hashes: &[String], value: bool) -> QbtResult<()> {
        let logger = self.ctx.logger.clone();
        let ids = self.for_each(hashes, |e, _| {
            if e.first_last != value {
                e.first_last = value;
                logger.info(format!(
                    "Download first and last piece first: {}, torrent: '{}'",
                    if value { "On" } else { "Off" },
                    e.display_name()
                ));
            }
        });
        self.push_settings(&ids);
        Ok(())
    }

    /// Renumbers queue positions densely (0..n) keeping their order;
    /// finished torrents leave the queue.
    pub(super) fn normalize_queue(&self) {
        let mut st = self.state.write();
        let mut q: Vec<(i64, i64, String)> = st
            .torrents
            .values()
            .filter(|e| e.queue_position >= 0)
            .map(|e| (e.queue_position, e.added_on, e.id.clone()))
            .collect();
        q.sort();
        for (i, (old, _, id)) in q.into_iter().enumerate() {
            if let Some(e) = st.torrents.get_mut(&id)
                && old != i as i64
            {
                e.queue_position = i as i64;
                e.rt.dirty = true;
            }
        }
    }

    pub fn queue_move(&self, hashes: &[String], mv: QueueMove) -> QbtResult<()> {
        if !self.ctx.prefs.current().bittorrent.queueing_enabled {
            return Err(QbtError::conflict("Torrent queueing must be enabled"));
        }
        self.normalize_queue();
        let mut st = self.state.write();
        let mut order: Vec<(i64, String)> =
            st.torrents.values().filter(|e| e.queue_position >= 0).map(|e| (e.queue_position, e.id.clone())).collect();
        order.sort();
        let mut ids: Vec<String> = order.into_iter().map(|(_, id)| id).collect();
        let selected: HashSet<String> = hashes.iter().map(|h| h.trim().to_ascii_lowercase()).collect();
        super::queue::move_in_queue(&mut ids, &selected, mv);
        for (i, id) in ids.iter().enumerate() {
            if let Some(e) = st.torrents.get_mut(id)
                && e.queue_position != i as i64
            {
                e.queue_position = i as i64;
                e.rt.dirty = true;
            }
        }
        drop(st);
        self.kick();
        Ok(())
    }

    // ---- Trackers, peers, web seeds -----------------------------------------

    pub fn add_trackers(&self, hash: &str, list: Vec<TrackerTierEntry>) -> QbtResult<()> {
        let valid: Vec<TrackerTierEntry> = list.into_iter().filter(|t| metainfo::is_valid_tracker_url(&t.url)).collect();
        let logger = self.ctx.logger.clone();
        let hashes: Vec<String> = if hash.trim() == "all" {
            self.state.read().torrents.keys().cloned().collect()
        } else {
            hash.split('|').map(str::to_string).collect()
        };
        let ids = self.for_each(&hashes, |e, _| {
            let before: HashSet<String> = e.trackers.iter().map(|t| t.url.clone()).collect();
            let added = trackers::add_trackers(&mut e.trackers, &valid);
            e.trust_urls(valid.iter().map(|t| t.url.as_str()));
            if added {
                for t in e.trackers.iter().filter(|t| !before.contains(&t.url)) {
                    logger.info(format!("Added tracker to torrent. Torrent: \"{}\". Tracker: \"{}\"", e.display_name(), t.url));
                }
            }
        });
        self.push_trackers(&ids);
        Ok(())
    }

    pub fn edit_tracker(&self, hash: &str, orig: &str, new: &str) -> QbtResult<()> {
        let new = new.trim();
        if !metainfo::is_valid_tracker_url(new) {
            return Err(QbtError::invalid("New tracker URL is invalid"));
        }
        self.with_torrent_mut(hash, |e, _| {
            if e.trackers.iter().any(|t| t.url == new) {
                return if orig == new { Ok(()) } else { Err(QbtError::conflict("New tracker URL already exists")) };
            }
            match e.trackers.iter_mut().find(|t| t.url == orig) {
                Some(t) => {
                    t.url = new.to_string();
                    e.trust_urls([new]);
                    Ok(())
                }
                None => Err(QbtError::conflict("Tracker not found")),
            }
        })??;
        let id = hash.trim().to_ascii_lowercase();
        self.push_trackers(std::slice::from_ref(&id));
        if let (Some(h), Some(engine)) = (self.handle_of(&id), self.engine()) {
            engine.force_reannounce(&h, Some(new));
        }
        Ok(())
    }

    /// Hands the stored tracker lists to attached torrents (kept URLs keep
    /// their status in the engine).
    fn push_trackers(&self, ids: &[String]) {
        let Some(engine) = self.engine() else { return };
        let work: Vec<_> = {
            let st = self.state.read();
            ids.iter()
                .filter_map(|id| st.torrents.get(id))
                .filter_map(|e| {
                    let (h, g) = e.rt.engine.clone()?;
                    (g == engine.generation).then(|| {
                        let list = e.trackers.iter().map(|t| (t.url.clone(), t.tier.clamp(0, u32::MAX as i64) as u32)).collect::<Vec<_>>();
                        (h, list, e.trusted_urls.clone())
                    })
                })
                .collect()
        };
        for (h, list, trusted) in work {
            engine.replace_trackers(&h, list, &trusted);
        }
    }

    pub fn remove_trackers(&self, hash: &str, urls: &[String]) -> QbtResult<()> {
        let logger = self.ctx.logger.clone();
        let hashes: Vec<String> = if matches!(hash.trim(), "all" | "*") {
            self.state.read().torrents.keys().cloned().collect()
        } else {
            hash.split('|').map(str::to_string).collect()
        };
        let urls: HashSet<&str> = urls.iter().map(|u| u.as_str()).collect();
        let ids = self.for_each(&hashes, |e, _| {
            let name = e.display_name();
            e.trackers.retain(|t| {
                let remove = urls.contains(t.url.as_str());
                if remove {
                    logger.info(format!("Removed tracker from torrent. Torrent: \"{name}\". Tracker: \"{}\"", t.url));
                }
                !remove
            });
        });
        self.push_trackers(&ids);
        Ok(())
    }

    pub fn replace_trackers(&self, hash: &str, list: Vec<TrackerTierEntry>) -> QbtResult<()> {
        let list: Vec<TrackerTierEntry> = list.into_iter().filter(|t| metainfo::is_valid_tracker_url(&t.url)).collect();
        let list = trackers::normalized(&list);
        self.with_torrent_mut(hash, |e, _| {
            // Newly listed URLs are user input; kept ones keep their trust.
            let new: Vec<String> = list.iter().filter(|t| !e.trackers.iter().any(|x| x.url == t.url)).map(|t| t.url.clone()).collect();
            e.trackers = list;
            e.trust_urls(new.iter().map(String::as_str));
        })?;
        self.push_trackers(&[hash.trim().to_ascii_lowercase()]);
        Ok(())
    }

    pub fn add_peers(&self, hashes: &[String], peers: &[String]) -> QbtResult<()> {
        let parsed: Vec<std::net::SocketAddr> = peers.iter().filter_map(|p| metainfo::parse_peer_address(p)).collect();
        if parsed.is_empty() {
            return Err(QbtError::invalid("No valid peers were specified"));
        }
        let logger = self.ctx.logger.clone();
        let engine = self.engine();
        let mut live = Vec::new();
        let mut kick = Vec::new();
        self.for_each(hashes, |e, _| {
            if e.is_private() {
                logger.warning(format!("Failed to add peers to torrent \"{}\". Reason: private torrent", e.display_name()));
                return;
            }
            match (&e.rt.engine, &engine) {
                (Some((h, g)), Some(engine)) if *g == engine.generation => live.push(h.clone()),
                _ => {
                    // Not in the engine yet (e.g. magnet): connect when added.
                    for p in &parsed {
                        if !e.peers.contains(p) {
                            if e.peers.len() >= 200 {
                                e.peers.remove(0);
                            }
                            e.peers.push(*p);
                        }
                    }
                    // Fetching metadata: try the new peers now instead of
                    // at the next retry.
                    if let Some(t) = e.rt.metadata_task.take() {
                        t.abort();
                        kick.push(e.id.clone());
                    }
                }
            }
            for p in &parsed {
                logger.info(format!("Peer \"{p}\" is added to torrent \"{}\"", e.display_name()));
            }
        });
        if let Some(engine) = engine {
            for h in live {
                engine.add_peers(&h, parsed.clone());
            }
        }
        for id in kick {
            self.start_metadata_task(&id);
        }
        Ok(())
    }

    pub fn ban_peers(&self, peers: &[String]) -> QbtResult<()> {
        let ips: Vec<String> = peers.iter().filter_map(|p| crate::ipfilter::normalize_ip(p)).collect();
        if ips.is_empty() {
            return Ok(());
        }
        for ip in &ips {
            self.ctx.logger.info(format!("Peer \"{ip}\" is manually banned"));
        }
        // Banned and disconnected at once; the preference update below keeps
        // the engine's list in sync.
        if let Some(engine) = self.engine() {
            for ip in ips.iter().filter_map(|i| i.parse::<IpAddr>().ok()) {
                engine.ban_ip(ip);
            }
        }
        self.ctx.prefs.update(|p| {
            let mut set: BTreeSet<String> = p.connection.banned_ips.iter().cloned().collect();
            set.extend(ips.iter().cloned());
            p.connection.banned_ips = set.into_iter().collect();
        })
    }

    pub fn add_web_seeds(&self, hash: &str, urls: &[String]) -> QbtResult<()> {
        for u in urls {
            if !metainfo::is_web_url(u) {
                return Err(QbtError::invalid(format!("\"{u}\" is not a valid URL")));
            }
        }
        let logger = self.ctx.logger.clone();
        let live = self.web_seed_engine(hash);
        self.with_torrent_mut(hash, |e, _| {
            for u in urls {
                let u = u.trim().to_string();
                if e.url_seeds.len() < metainfo::MAX_WEB_SEEDS && !e.url_seeds.iter().any(|x| x.eq_ignore_ascii_case(&u)) {
                    logger.info(format!("Added URL seed to torrent. Torrent: \"{}\". URL: \"{u}\"", e.display_name()));
                    if let Some((engine, h)) = &live {
                        engine.add_web_seeds(h, vec![u.clone()]);
                    }
                    e.url_seeds.push(u.clone());
                    e.trust_urls([u.as_str()]);
                }
            }
        })
    }

    pub fn edit_web_seed(&self, hash: &str, orig: &str, new: &str) -> QbtResult<()> {
        if !metainfo::is_web_url(new) {
            return Err(QbtError::invalid(format!("\"{new}\" is not a valid URL")));
        }
        let live = self.web_seed_engine(hash);
        self.with_torrent_mut(hash, |e, _| {
            let Some(pos) = e.url_seeds.iter().position(|u| u == orig) else {
                return Err(QbtError::conflict(format!("\"{orig}\" is not an existing URL")));
            };
            if e.url_seeds.iter().any(|u| u.eq_ignore_ascii_case(new.trim())) {
                return Err(QbtError::conflict("This web seed is already in the list."));
            }
            if let Some((engine, h)) = &live {
                match engine.edit_web_seed(h, orig, new.trim()) {
                    Ok(()) | Err(librqbit::WebSeedEditError::NotFound) => {}
                    Err(librqbit::WebSeedEditError::InvalidUrl) => {
                        return Err(QbtError::invalid(format!("\"{new}\" is not a valid URL")));
                    }
                    Err(librqbit::WebSeedEditError::AlreadyExists) => {
                        return Err(QbtError::conflict("This web seed is already in the list."));
                    }
                }
            }
            e.url_seeds[pos] = new.trim().to_string();
            e.trust_urls([new.trim()]);
            Ok(())
        })?
    }

    pub fn remove_web_seeds(&self, hash: &str, urls: &[String]) -> QbtResult<()> {
        let logger = self.ctx.logger.clone();
        let live = self.web_seed_engine(hash);
        self.with_torrent_mut(hash, |e, _| {
            let name = e.display_name();
            let removed: Vec<String> = e.url_seeds.iter().filter(|u| urls.contains(u)).cloned().collect();
            for u in &removed {
                logger.info(format!("Removed URL seed from torrent. Torrent: \"{name}\". URL: \"{u}\""));
            }
            if let Some((engine, h)) = &live {
                engine.remove_web_seeds(h, &removed);
            }
            e.url_seeds.retain(|u| !removed.contains(u));
        })
    }

    // ---- Categories & tags --------------------------------------------------

    pub fn categories(&self) -> Vec<Category> {
        self.state.read().categories.values().cloned().collect()
    }

    pub fn create_category(&self, category: Category) -> QbtResult<()> {
        let name = category.name.trim().to_string();
        if name.is_empty() {
            return Err(QbtError::invalid("Category cannot be empty"));
        }
        if !categories::is_valid_category_name(&name) {
            return Err(QbtError::conflict("Incorrect category name"));
        }
        {
            let mut st = self.state.write();
            if st.categories.contains_key(&name) {
                return Err(QbtError::conflict("Unable to create category"));
            }
            for parent in categories::expand_category(&name) {
                if parent != name {
                    st.categories.entry(parent.clone()).or_insert_with(|| categories::new_category(&parent));
                }
            }
            st.categories.insert(name.clone(), Category { name, ..category });
        }
        self.save_categories();
        Ok(())
    }

    pub fn edit_category(&self, category: Category) -> QbtResult<()> {
        let name = category.name.trim().to_string();
        if name.is_empty() {
            return Err(QbtError::invalid("Category cannot be empty"));
        }
        let new = Category { name: name.clone(), ..category };
        let relocate: Vec<String> = {
            let prefs = self.ctx.prefs.current();
            let mut st = self.state.write();
            let Some(old) = st.categories.get(&name).cloned() else {
                return Err(QbtError::not_found("Category does not exist"));
            };
            if old == new {
                return Ok(());
            }
            let paths_changed = old.save_path != new.save_path || old.download_path != new.download_path;
            let State { torrents, categories, .. } = &mut *st;
            if paths_changed && !prefs.downloads.category_changed_tmm_enabled {
                // "Switch affected torrents to Manual Mode" before the change.
                let env = Env::new(&prefs, categories);
                for e in torrents.values_mut().filter(|e| e.auto_tmm && e.category == name) {
                    e.save_path = e.save_path(&env);
                    e.download_path = e.download_path(&env);
                    e.auto_tmm = false;
                    e.rt.dirty = true;
                }
            }
            categories.insert(name.clone(), new);
            torrents.values().filter(|e| e.auto_tmm && e.category == name).map(|e| e.id.clone()).collect()
        };
        self.save_categories();
        self.relocate(relocate);
        Ok(())
    }

    pub fn remove_categories(&self, names: &[String]) -> QbtResult<()> {
        let mut moved: Vec<(String, String)> = Vec::new();
        {
            let mut st = self.state.write();
            for name in names {
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                let parent = categories::parent_category_name(name).to_string();
                for e in st.torrents.values_mut() {
                    if e.belongs_to_category(name) {
                        moved.push((e.id.clone(), parent.clone()));
                    }
                }
                let prefix = format!("{name}/");
                st.categories.retain(|c, _| c != name && !c.starts_with(&prefix));
            }
        }
        for (id, parent) in moved {
            let _ = self.set_category_unchecked(&[id], &parent);
        }
        self.save_categories();
        Ok(())
    }

    pub fn set_category(&self, hashes: &[String], category: &str) -> QbtResult<()> {
        let category = category.trim();
        if !category.is_empty() && !self.state.read().categories.contains_key(category) {
            return Err(QbtError::conflict("Incorrect category name"));
        }
        self.set_category_unchecked(hashes, category)
    }

    fn set_category_unchecked(&self, hashes: &[String], category: &str) -> QbtResult<()> {
        let switch_to_manual = !self.ctx.prefs.current().downloads.torrent_changed_tmm_enabled;
        let mut relocate = Vec::new();
        self.for_each(hashes, |e, env| {
            if e.category == category {
                return;
            }
            if switch_to_manual && e.auto_tmm {
                e.save_path = e.save_path(env);
                e.download_path = e.download_path(env);
                e.auto_tmm = false;
            }
            e.category = category.to_string();
            if e.auto_tmm {
                relocate.push(e.id.clone());
            }
        });
        self.relocate(relocate);
        Ok(())
    }

    pub fn set_auto_management(&self, hashes: &[String], enabled: bool) -> QbtResult<()> {
        let mut relocate = Vec::new();
        self.for_each(hashes, |e, env| {
            if e.auto_tmm == enabled {
                return;
            }
            if !enabled {
                e.save_path = e.save_path(env);
                e.download_path = e.download_path(env);
            }
            e.auto_tmm = enabled;
            relocate.push(e.id.clone());
        });
        self.relocate(relocate);
        Ok(())
    }

    pub fn tags(&self) -> Vec<String> {
        self.state.read().tags.iter().cloned().collect()
    }

    fn valid_tags(tags: &[String]) -> QbtResult<Vec<String>> {
        let mut out = Vec::new();
        for t in tags {
            let t = t.trim();
            if t.is_empty() {
                continue;
            }
            if !categories::is_valid_tag(t) {
                return Err(QbtError::invalid(format!("Tag name '{t}' is invalid")));
            }
            if !out.iter().any(|x: &String| x == t) {
                out.push(t.to_string());
            }
        }
        Ok(out)
    }

    pub fn create_tags(&self, tags: &[String]) -> QbtResult<()> {
        let tags = Self::valid_tags(tags)?;
        self.state.write().tags.extend(tags);
        self.save_tags();
        Ok(())
    }

    pub fn delete_tags(&self, tags: &[String]) -> QbtResult<()> {
        let del: HashSet<String> = tags.iter().map(|t| t.trim().to_string()).collect();
        {
            let mut st = self.state.write();
            st.tags.retain(|t| !del.contains(t));
            for e in st.torrents.values_mut() {
                let before = e.tags.len();
                e.tags.retain(|t| !del.contains(t));
                if e.tags.len() != before {
                    e.rt.dirty = true;
                }
            }
        }
        self.save_tags();
        Ok(())
    }

    pub fn add_tags(&self, hashes: &[String], tags: &[String]) -> QbtResult<()> {
        let tags = Self::valid_tags(tags)?;
        self.state.write().tags.extend(tags.iter().cloned());
        self.for_each(hashes, |e, _| {
            for t in &tags {
                if !e.tags.contains(t) {
                    e.tags.push(t.clone());
                }
            }
            e.tags.sort();
        });
        self.save_tags();
        Ok(())
    }

    pub fn remove_tags(&self, hashes: &[String], tags: &[String]) -> QbtResult<()> {
        let del: HashSet<String> = tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        self.for_each(hashes, |e, _| {
            if del.is_empty() {
                e.tags.clear();
            } else {
                e.tags.retain(|t| !del.contains(t));
            }
        });
        Ok(())
    }

    pub fn set_tags(&self, hashes: &[String], tags: &[String]) -> QbtResult<()> {
        let mut tags = Self::valid_tags(tags)?;
        tags.sort();
        self.state.write().tags.extend(tags.iter().cloned());
        self.for_each(hashes, |e, _| e.tags = tags.clone());
        self.save_tags();
        Ok(())
    }

    // ---- Transfer ---------------------------------------------------------------

    pub fn server_state(&self) -> ServerState {
        let prefs = self.ctx.prefs.current();
        let engine = self.engine();
        let stats = self.stats_file();
        let m = self.misc.lock();
        let listening = engine.as_ref().is_some_and(|e| e.is_listening());
        let global_ratio = if stats.alltime_dl > 0 && stats.alltime_ul > 0 {
            stats.alltime_ul as f64 / stats.alltime_dl as f64
        } else {
            -1.0
        };
        ServerState {
            dl_speed: m.dl_rate,
            dl_session_data: m.session_download,
            up_speed: m.ul_rate,
            up_session_data: m.session_upload,
            dl_rate_limit: self.global_download_limit(),
            up_rate_limit: self.global_upload_limit(),
            dht_nodes: m.dht_nodes,
            connection_status: if !listening {
                ConnectionStatus::Disconnected
            } else if m.has_incoming {
                ConnectionStatus::Connected
            } else {
                ConnectionStatus::Firewalled
            },
            queueing: prefs.bittorrent.queueing_enabled,
            use_alt_speed_limits: self.alt_speed_limits_enabled(),
            use_subcategories: prefs.downloads.use_subcategories,
            refresh_interval: prefs.advanced.refresh_interval,
            free_space_on_disk: m.free_space,
            last_external_address_v4: m.external_v4.clone(),
            last_external_address_v6: m.external_v6.clone(),
            alltime_download: stats.alltime_dl,
            alltime_upload: stats.alltime_ul,
            total_wasted_session: m.session_wasted,
            global_ratio,
            total_peer_connections: m.peers_live,
            ..Default::default()
        }
    }

    pub fn alt_speed_limits_enabled(&self) -> bool {
        self.alt_speed.load(Ordering::Relaxed)
    }

    pub fn set_alt_speed_limits_enabled(&self, enabled: bool) {
        if self.alt_speed.swap(enabled, Ordering::Relaxed) != enabled {
            self.apply_rate_limits();
            self.save_session_state();
        }
    }

    pub fn global_download_limit(&self) -> i64 {
        let p = self.ctx.prefs.current();
        if self.alt_speed_limits_enabled() { p.speed.alt_dl_limit.max(0) } else { p.speed.dl_limit.max(0) }
    }

    pub fn global_upload_limit(&self) -> i64 {
        let p = self.ctx.prefs.current();
        if self.alt_speed_limits_enabled() { p.speed.alt_up_limit.max(0) } else { p.speed.up_limit.max(0) }
    }

    /// Normalises a B/s limit like qBittorrent (KiB/s storage).
    fn normalize_limit(limit: i64) -> i64 {
        if limit <= 0 {
            0
        } else if limit <= 1024 {
            1024
        } else {
            (limit / 1024) * 1024
        }
    }

    pub fn set_global_download_limit(&self, limit: i64) -> QbtResult<()> {
        let v = Self::normalize_limit(limit);
        let alt = self.alt_speed_limits_enabled();
        self.ctx.prefs.update(|p| if alt { p.speed.alt_dl_limit = v } else { p.speed.dl_limit = v })?;
        self.apply_rate_limits();
        Ok(())
    }

    pub fn set_global_upload_limit(&self, limit: i64) -> QbtResult<()> {
        let v = Self::normalize_limit(limit);
        let alt = self.alt_speed_limits_enabled();
        self.ctx.prefs.update(|p| if alt { p.speed.alt_up_limit = v } else { p.speed.up_limit = v })?;
        self.apply_rate_limits();
        Ok(())
    }

    pub(super) fn apply_rate_limits(&self) {
        if let Some(engine) = self.engine() {
            engine.set_rate_limits(self.global_download_limit() as u64, self.global_upload_limit() as u64);
        }
    }

    pub fn speed_history(&self, seconds: u32) -> Vec<SpeedSample> {
        self.misc.lock().speed.last(seconds)
    }

    pub fn speed_history_points(&self, seconds: u32, resolution: u32, after_ms: i64) -> Vec<SpeedSample> {
        self.misc.lock().speed.points(seconds, resolution, after_ms)
    }

    /// Peer country lookups for other subsystems (WebAPI torrentPeers).
    pub fn country_of(&self, ip: IpAddr) -> Option<String> {
        self.geoip.country_code(ip)
    }

    /// Info of torrent metadata for the add dialog.
    pub(super) fn metadata_info(&self, meta: &TorrentMeta) -> TorrentMetadataInfo {
        let id = meta.id();
        TorrentMetadataInfo {
            hash: id.clone(),
            infohash_v1: meta.info_hash_v1_hex(),
            infohash_v2: meta.info_hash_v2_hex(),
            name: meta.name.clone(),
            has_metadata: true,
            total_size: meta.total_size as i64,
            piece_size: meta.piece_length as i64,
            pieces_num: meta.pieces as i64,
            private: meta.private,
            comment: meta.comment.clone(),
            created_by: meta.created_by.clone(),
            creation_date: meta.creation_date,
            files: meta
                .files
                .iter()
                .enumerate()
                .map(|(i, f)| MetadataFile { index: i as i64, path: f.path.clone(), size: f.size as i64 })
                .collect(),
            trackers: meta.trackers.clone(),
            url_seeds: meta.url_seeds.clone(),
            is_duplicate: self.state.read().torrents.contains_key(&id),
        }
    }
}
