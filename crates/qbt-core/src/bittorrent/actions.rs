//! Engine-facing torrent actions: attaching torrents to the engine and
//! reconciling the desired run state, start/stop/force start, remove,
//! recheck, reannounce, storage moves, file renames.

use std::path::PathBuf;
use std::sync::Arc;

use super::engine::{AddRequest, EngineHandle, TorrentSettings};
use super::session::{Session, State};
use super::torrent::{Env, Phase, TorrentEntry};
use super::{bencode, metainfo, paths, trash};
use crate::error::{QbtError, QbtResult};
use crate::events::CoreEvent;
use crate::model::*;
use crate::prefs::{ContentRemoveOption, Preferences};

/// What a storage move updates afterwards (`MoveStorageContext`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MoveContext {
    AdjustCurrentLocation,
    ChangeSavePath,
    ChangeDownloadPath,
}

/// The torrent should be running in the engine.
pub(super) fn wants_running(e: &TorrentEntry) -> bool {
    e.has_metadata()
        && !e.stopped
        && !e.rt.moving
        && !e.rt.missing_files
        && e.rt.error.is_none()
        && (e.forced || !e.rt.queued)
}

/// A magnet torrent should be fetching its metadata. (A pure v2 torrent
/// missing piece layers runs in the engine, which fetches them.)
pub(super) fn wants_metadata(e: &TorrentEntry) -> bool {
    !e.has_metadata() && !e.stopped && (e.forced || !e.rt.queued)
}

/// Per-torrent engine settings from the entry and preferences.
pub(super) fn torrent_settings(e: &TorrentEntry, prefs: &Preferences) -> TorrentSettings {
    let c = &prefs.connection;
    TorrentSettings {
        priorities: e.engine_priorities(),
        sequential: e.sequential,
        first_last: e.first_last,
        super_seeding: e.super_seeding,
        download_limit: e.dl_limit.max(0) as u64,
        upload_limit: e.ul_limit.max(0) as u64,
        peer_limit: (c.max_connec_per_torrent > 0).then_some(c.max_connec_per_torrent as usize),
        max_uploads: (c.max_uploads_per_torrent > 0).then(|| c.max_uploads_per_torrent.min(u32::MAX as i64) as u32),
        disabled_discovery: librqbit::DisabledDiscovery { dht: e.disable_dht, pex: e.disable_pex, lsd: e.disable_lsd },
    }
}

enum Act {
    Attach(String),
    Pause(EngineHandle),
    Resume(String, EngineHandle),
    Settings(EngineHandle, TorrentSettings),
    StartMetadata(String),
}

impl Session {
    fn ids_of(hashes: &[String]) -> Vec<String> {
        hashes.iter().map(|h| h.trim().to_ascii_lowercase()).collect()
    }

    /// Brings the engine in line with the desired state of the given
    /// torrents (all when `only` is None).
    pub(super) async fn reconcile(&self, only: Option<&[String]>) {
        let Some(engine) = self.engine() else { return };
        let generation = engine.generation;
        let prefs = self.ctx.prefs.current();
        let session_paused = self.is_paused();
        let mut acts = Vec::new();
        {
            let mut st = self.state.write();
            let ids: Vec<String> = match only {
                Some(ids) => ids.to_vec(),
                None => st.torrents.keys().cloned().collect(),
            };
            for id in ids {
                let Some(e) = st.torrents.get_mut(&id) else { continue };
                if !e.has_metadata() {
                    if wants_metadata(e) && !session_paused {
                        if e.rt.metadata_task.as_ref().is_none_or(|t| t.is_finished()) {
                            acts.push(Act::StartMetadata(id.clone()));
                        }
                    } else if let Some(t) = e.rt.metadata_task.take() {
                        t.abort();
                    }
                    continue;
                }
                if let Some((_, g)) = &e.rt.engine
                    && *g != generation
                {
                    e.rt.engine = None;
                    e.rt.phase = Phase::Detached;
                }
                match e.rt.engine.clone() {
                    None => {
                        if !e.rt.adding && !e.rt.moving && e.rt.error.is_none() {
                            acts.push(Act::Attach(id.clone()));
                        }
                    }
                    Some((h, _)) => {
                        if e.rt.settings_dirty {
                            e.rt.settings_dirty = false;
                            acts.push(Act::Settings(h.clone(), torrent_settings(e, &prefs)));
                        }
                        if e.rt.moving {
                            continue;
                        }
                        // A torrent with missing files is restarted once when
                        // the user starts it (the engine rechecks it).
                        let want = !session_paused && (wants_running(e) || (e.rt.missing_files && e.rt.retry_missing && !e.stopped));
                        let running = engine.is_running(&h);
                        if want && !running {
                            e.rt.retry_missing = false;
                            acts.push(Act::Resume(id.clone(), h.clone()));
                        } else if !want && running && !e.rt.missing_files {
                            acts.push(Act::Pause(h.clone()));
                        }
                    }
                }
            }
        }
        for act in acts {
            match act {
                Act::Attach(id) => self.attach(&id).await,
                Act::Pause(h) => engine.pause(&h).await,
                Act::Resume(id, h) => {
                    if let Err(e) = engine.resume(&h).await {
                        self.set_error(&id, format!("{e:#}"));
                    }
                }
                Act::Settings(h, s) => engine.apply_torrent_settings(&h, &s),
                Act::StartMetadata(id) => self.start_metadata_task(&id),
            }
        }
    }

    pub(super) fn set_error(&self, id: &str, message: String) {
        if let Some(e) = self.state.write().torrents.get_mut(id) {
            e.rt.error = Some(message);
            e.rt.error_reported = false;
        }
    }

    /// The torrent's engine handle if it is attached to the current engine.
    pub(super) fn handle_of(&self, id: &str) -> Option<EngineHandle> {
        let engine = self.engine()?;
        let st = self.state.read();
        let (h, g) = st.torrents.get(id)?.rt.engine.clone()?;
        (g == engine.generation).then_some(h)
    }

    /// Hands a torrent with metadata to the engine.
    pub(super) async fn attach(&self, id: &str) {
        let Some(engine) = self.engine() else { return };
        let prefs = self.ctx.prefs.current();
        let queueing = prefs.bittorrent.queueing_enabled;
        let session_paused = self.is_paused();
        struct Prep {
            req: AddRequest,
            trackers: Vec<TrackerTierEntry>,
            seeds: Vec<String>,
            comment: String,
            created_by: String,
            creation_date: i64,
        }
        let prep = {
            let mut st = self.state.write();
            let State { torrents, categories, .. } = &mut *st;
            let env = Env::new(&prefs, categories);
            let Some(e) = torrents.get_mut(id) else { return };
            let Some(meta) = e.meta.clone() else { return };
            if e.rt.engine.is_some() || e.rt.adding || e.rt.moving {
                return;
            }
            e.rt.adding = true;
            let location = e.actual_location(&env);
            e.rt.location = Some(location.clone());
            let skip = std::mem::take(&mut e.rt.skip_hash_check);
            Prep {
                req: AddRequest {
                    torrent_bytes: Vec::new(),
                    paused: session_paused || !wants_running(e) || (queueing && !e.forced),
                    output_folder: PathBuf::from(location),
                    renamed_files: e.engine_paths(),
                    trackers: e.trackers.iter().map(|t| (t.url.clone(), t.tier.clamp(0, u32::MAX as i64) as u32)).collect(),
                    web_seeds: e.url_seeds.clone(),
                    trusted_urls: e.trusted_urls.clone(),
                    initial_peers: e.peers.clone(),
                    skip_hash_check: skip,
                    settings: torrent_settings(e, &prefs),
                },
                trackers: e.trackers.clone(),
                seeds: e.url_seeds.clone(),
                comment: e.comment(),
                created_by: meta.created_by.clone(),
                creation_date: meta.creation_date,
            }
        };
        let backup = self.backup.clone();
        let limit = self.torrent_file_limit();
        let limits = self.limits();
        let id_owned = id.to_string();
        let (trackers, seeds, comment, created_by, date) =
            (prep.trackers.clone(), prep.seeds.clone(), prep.comment.clone(), prep.created_by.clone(), prep.creation_date);
        let bytes = tokio::task::spawn_blocking(move || {
            let bytes = backup.load_torrent_file(&id_owned, limit).map_err(|e| format!("cannot read metadata: {e}"))?;
            match bencode::find_top_level_span(&bytes, b"info", limits) {
                Some((s, e)) => {
                    let layers = bencode::find_top_level_span(&bytes, b"piece layers", limits).map(|(ls, le)| &bytes[ls..le]);
                    Ok(metainfo::build_torrent_file(&bytes[s..e], layers, &trackers, &seeds, &comment, &created_by, date))
                }
                None => Err("invalid metadata".to_string()),
            }
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        let mut req = prep.req;
        let result = match bytes {
            Ok(b) => {
                req.torrent_bytes = b;
                engine.add(req).await.map_err(|e| format!("{e:#}"))
            }
            Err(e) => Err(e),
        };
        let generation = engine.generation;
        let stale = {
            let mut st = self.state.write();
            match st.torrents.get_mut(id) {
                None => result.ok(),
                Some(e) => {
                    e.rt.adding = false;
                    e.rt.restoring = false;
                    match result {
                        Ok(h) => {
                            e.rt.location = Some(engine.output_folder(&h).to_string_lossy().into_owned());
                            e.rt.engine = Some((h, generation));
                            e.rt.phase = Phase::Checking { checked_bytes: 0 };
                            None
                        }
                        Err(msg) => {
                            self.ctx.logger.warning(format!("Failed to load torrent. Reason: \"{msg}\""));
                            e.rt.error = Some(msg);
                            e.rt.error_reported = false;
                            None
                        }
                    }
                }
            }
        };
        if let Some(h) = stale {
            let _ = engine.remove(&h, false, None).await;
        }
        self.kick();
    }

    // ---- Session pause -----------------------------------------------------------

    /// `SessionImpl::isPaused`.
    pub fn is_paused(&self) -> bool {
        self.paused.load(std::sync::atomic::Ordering::Acquire)
    }

    /// `SessionImpl::pause`: no torrent connects, announces or transfers
    /// until [`Session::resume_session`]. The torrents' own stopped/started
    /// state is untouched (it is what resume data stores); the engine is only
    /// told to hold them. DHT keeps running, as in libtorrent.
    pub async fn pause_session(&self) {
        if self.paused.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        self.ctx.logger.info("BitTorrent session paused");
        self.reconcile(None).await;
        self.emit(CoreEvent::SessionPaused);
    }

    /// `SessionImpl::resume`.
    pub async fn resume_session(&self) {
        if !self.paused.swap(false, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        self.ctx.logger.info("BitTorrent session resumed");
        self.update_queue();
        self.reconcile(None).await;
        self.kick();
        self.emit(CoreEvent::SessionResumed);
    }

    // ---- Start / stop ----------------------------------------------------------

    pub async fn start(&self, hashes: &[String]) -> QbtResult<()> {
        self.start_mode(hashes, false).await
    }

    pub async fn set_force_start(&self, hashes: &[String], value: bool) -> QbtResult<()> {
        self.start_mode(hashes, value).await
    }

    async fn start_mode(&self, hashes: &[String], forced: bool) -> QbtResult<()> {
        let logger = self.ctx.logger.clone();
        let ids = self.for_each(hashes, |e, _| {
            e.rt.error = None;
            e.rt.error_reported = false;
            e.forced = forced;
            if e.rt.missing_files {
                // The engine re-checks the data when started again.
                e.rt.missing_files = false;
                e.rt.retry_missing = true;
            }
            if e.stopped {
                e.stopped = false;
                logger.info(format!("Torrent resumed. Torrent: \"{}\"", e.display_name()));
            }
        });
        self.update_queue();
        self.reconcile(Some(&ids)).await;
        Ok(())
    }

    pub async fn stop(&self, hashes: &[String]) -> QbtResult<()> {
        let logger = self.ctx.logger.clone();
        let ids = self.for_each(hashes, |e, _| {
            e.stop_condition = StopCondition::None;
            if !e.stopped {
                e.stopped = true;
                e.rt.dl_rate = 0;
                e.rt.ul_rate = 0;
                e.rt.avg.reset();
                logger.info(format!("Torrent stopped. Torrent: \"{}\"", e.display_name()));
            }
        });
        self.reconcile(Some(&ids)).await;
        Ok(())
    }

    // ---- Remove ------------------------------------------------------------------

    pub async fn remove(&self, hashes: &[String], delete_files: bool) -> QbtResult<()> {
        let prefs = self.ctx.prefs.current();
        let to_trash = prefs.advanced.torrent_content_remove_option == ContentRemoveOption::MoveToTrash;
        for id in Self::ids_of(hashes) {
            let removed = self.state.write().torrents.remove(&id);
            let Some(mut e) = removed else { continue };
            if let Some(t) = e.rt.metadata_task.take() {
                t.abort();
            }
            let name = e.display_name();
            let backup = self.backup.clone();
            let rid = id.clone();
            let _ = tokio::task::spawn_blocking(move || backup.delete(&rid)).await;
            self.ctx.logger.info(format!("Torrent removed. Torrent: \"{name}\""));
            self.emit(CoreEvent::TorrentRemoved { hash: id.clone(), name: name.clone() });
            let (Some((h, _)), Some(engine)) = (e.rt.engine.take(), self.engine()) else { continue };
            let trash: Option<Box<librqbit::storage::TrashFn>> = (delete_files && to_trash).then(|| {
                Box::new(|p: &std::path::Path| trash::move_to_trash(p).or_else(|_| std::fs::remove_file(p)))
                    as Box<librqbit::storage::TrashFn>
            });
            match engine.remove(&h, delete_files, trash).await {
                Ok(()) if delete_files => self.ctx.logger.info(format!("Torrent content removed. Torrent: \"{name}\"")),
                Ok(()) => {}
                Err(err) if delete_files => self
                    .ctx
                    .logger
                    .warning(format!("Failed to remove torrent content. Torrent: \"{name}\". Error: \"{err:#}\"")),
                Err(_) => {}
            }
        }
        self.normalize_queue();
        self.kick();
        Ok(())
    }

    // ---- Recheck / reannounce ------------------------------------------------------

    pub async fn recheck(&self, hashes: &[String]) -> QbtResult<()> {
        let ids = Self::ids_of(hashes);
        let mut targets = Vec::new();
        {
            let mut st = self.state.write();
            for id in &ids {
                let Some(e) = st.torrents.get_mut(id) else { continue };
                if !e.has_metadata() || e.rt.moving {
                    continue;
                }
                e.rt.missing_files = false;
                e.rt.error = None;
                e.rt.unchecked = false;
                e.rt.finished = false;
                if e.stopped {
                    // Checked, then stopped again (FilesChecked).
                    e.stopped = false;
                    e.stop_condition = StopCondition::FilesChecked;
                }
                e.rt.dirty = true;
                if let Some((h, _)) = &e.rt.engine {
                    e.rt.phase = Phase::Checking { checked_bytes: 0 };
                    targets.push((id.clone(), h.clone()));
                }
            }
        }
        if let Some(engine) = self.engine() {
            for (id, h) in targets {
                if let Err(err) = engine.force_recheck(&h).await {
                    self.set_error(&id, format!("{err:#}"));
                }
            }
        }
        self.reconcile(Some(&ids)).await;
        Ok(())
    }

    pub async fn reannounce(&self, hashes: &[String]) -> QbtResult<()> {
        self.reannounce_trackers(hashes, &[]).await
    }

    /// Forces an announce to all trackers (and DHT), or only to `urls`.
    pub async fn reannounce_trackers(&self, hashes: &[String], urls: &[String]) -> QbtResult<()> {
        let Some(engine) = self.engine() else { return Ok(()) };
        for id in Self::ids_of(hashes) {
            let Some(h) = self.handle_of(&id) else { continue };
            if urls.is_empty() {
                engine.force_reannounce(&h, None);
            } else {
                for u in urls {
                    engine.force_reannounce(&h, Some(u));
                }
            }
        }
        Ok(())
    }

    // ---- Storage location ----------------------------------------------------------

    fn prepare_dir(path: &str) -> QbtResult<String> {
        let path = paths::expand_home(path.trim());
        if path.is_empty() {
            return Err(QbtError::invalid("Save path cannot be empty"));
        }
        if !path.starts_with('/') {
            return Err(QbtError::invalid("Save path must be absolute"));
        }
        let path = paths::clean(&path);
        std::fs::create_dir_all(&path).map_err(|_| QbtError::conflict("Cannot make save path"))?;
        Ok(path)
    }

    pub async fn set_location(&self, hashes: &[String], location: &str) -> QbtResult<()> {
        let location = Self::prepare_dir(location)?;
        let mut targets = Vec::new();
        self.for_each(hashes, |e, env| {
            if e.auto_tmm {
                e.save_path = e.save_path(env);
                e.download_path = e.download_path(env);
                e.auto_tmm = false;
            }
            self.ctx.logger.info(format!(
                "Set location: moving \"{}\", from \"{}\" to \"{location}\"",
                e.display_name(),
                e.save_path
            ));
            targets.push(e.id.clone());
        });
        for id in targets {
            self.set_save_path_one(&id, &location);
        }
        Ok(())
    }

    pub async fn set_save_path(&self, hashes: &[String], path: &str) -> QbtResult<()> {
        let prefs = self.ctx.prefs.current();
        let expanded = paths::expand_home(path.trim());
        if expanded.is_empty() {
            return Err(QbtError::invalid("Save path cannot be empty"));
        }
        let ids = Self::ids_of(hashes);
        for id in ids {
            let resolved = {
                let st = self.state.read();
                let Some(e) = st.torrents.get(&id) else { continue };
                if e.auto_tmm {
                    continue;
                }
                let env = Env::new(&prefs, &st.categories);
                let base = if prefs.downloads.use_category_paths_in_manual_mode {
                    super::categories::category_save_path(&st.categories, &e.category, &env.paths)
                } else {
                    env.paths.save_path.clone()
                };
                paths::resolve(&base, &expanded)
            };
            std::fs::create_dir_all(&resolved).map_err(|_| QbtError::conflict("Cannot create target directory"))?;
            self.set_save_path_one(&id, &resolved);
        }
        Ok(())
    }

    /// `TorrentImpl::setSavePath` for a manual-mode torrent.
    fn set_save_path_one(&self, id: &str, resolved: &str) {
        let action = {
            let prefs = self.ctx.prefs.current();
            let mut st = self.state.write();
            let State { torrents, categories, .. } = &mut *st;
            let env = Env::new(&prefs, categories);
            let Some(e) = torrents.get_mut(id) else { return };
            if e.save_path(&env) == resolved && e.actual_location(&env) == resolved {
                return;
            }
            e.rt.dirty = true;
            if e.is_finished() || e.seed_status || e.download_path(&env).is_empty() {
                Some(resolved.to_string())
            } else {
                e.save_path = resolved.to_string();
                None
            }
        };
        if let (Some(target), Some(me)) = (action, self.arc()) {
            let id = id.to_string();
            self.ctx.rt.spawn(async move { me.move_storage(id, target, false, MoveContext::ChangeSavePath, false).await });
        }
    }

    pub async fn set_download_path(&self, hashes: &[String], path: &str) -> QbtResult<()> {
        let prefs = self.ctx.prefs.current();
        let expanded = paths::expand_home(path.trim());
        for id in Self::ids_of(hashes) {
            let (target, record) = {
                let mut st = self.state.write();
                let State { torrents, categories, .. } = &mut *st;
                let env = Env::new(&prefs, categories);
                let Some(e) = torrents.get_mut(&id) else { continue };
                if e.auto_tmm {
                    continue;
                }
                let resolved = if expanded.is_empty() {
                    String::new()
                } else {
                    let base = if prefs.downloads.use_category_paths_in_manual_mode {
                        let p = super::categories::category_download_path(categories, &e.category, &env.paths);
                        if p.is_empty() { env.paths.download_path.clone() } else { p }
                    } else {
                        env.paths.download_path.clone()
                    };
                    paths::resolve(&base, &expanded)
                };
                if resolved == e.download_path {
                    continue;
                }
                if !resolved.is_empty() {
                    std::fs::create_dir_all(&resolved).map_err(|_| QbtError::conflict("Cannot create target directory"))?;
                }
                e.rt.dirty = true;
                let incomplete = !(e.is_finished() || e.seed_status);
                if incomplete && e.has_metadata() {
                    let target = if resolved.is_empty() { e.save_path(&env) } else { resolved.clone() };
                    (Some(target), resolved)
                } else {
                    e.download_path = resolved.clone();
                    (None, resolved)
                }
            };
            if let (Some(target), Some(me)) = (target, self.arc()) {
                let _ = record;
                let id = id.clone();
                self.ctx.rt.spawn(async move { me.move_storage(id, target, false, MoveContext::ChangeDownloadPath, false).await });
            }
        }
        Ok(())
    }

    /// Relocates ATM torrents to their category paths when needed.
    pub(super) fn relocate(&self, ids: Vec<String>) {
        let prefs = self.ctx.prefs.current();
        let mut moves = Vec::new();
        {
            let st = self.state.read();
            let env = Env::new(&prefs, &st.categories);
            for id in ids {
                let Some(e) = st.torrents.get(&id) else { continue };
                if !e.auto_tmm || !e.has_metadata() {
                    continue;
                }
                let target = e.target_location(&env);
                if target != e.actual_location(&env) {
                    moves.push((id, target));
                }
            }
        }
        let Some(me) = self.arc() else { return };
        for (id, target) in moves {
            let me = me.clone();
            self.ctx.rt.spawn(async move { me.move_storage(id, target, true, MoveContext::AdjustCurrentLocation, false).await });
        }
    }

    /// Moves the torrent's content to `target` (queued, one move at a time).
    /// `overwrite` replaces existing files at the destination; otherwise
    /// they are kept and used (`dont_replace`).
    pub(super) async fn move_storage(self: Arc<Self>, id: String, target: String, overwrite: bool, ctx: MoveContext, finish_after: bool) {
        let target = paths::clean(&target);
        enum Plan {
            Done { finished: bool },
            Move(EngineHandle, String, String),
        }
        let plan = {
            let prefs = self.ctx.prefs.current();
            let mut st = self.state.write();
            let State { torrents, categories, .. } = &mut *st;
            let env = Env::new(&prefs, categories);
            let Some(e) = torrents.get_mut(&id) else { return };
            let save_path = e.save_path(&env);
            let apply = |e: &mut TorrentEntry| match ctx {
                MoveContext::ChangeSavePath => e.save_path = target.clone(),
                MoveContext::ChangeDownloadPath => {
                    if target != save_path {
                        e.download_path = target.clone();
                    } else {
                        e.download_path.clear();
                    }
                }
                MoveContext::AdjustCurrentLocation => {}
            };
            match e.rt.engine.clone() {
                None => {
                    // No data in the engine (no metadata or not attached):
                    // record the new location only.
                    apply(e);
                    if e.has_metadata() {
                        e.rt.location = Some(target.clone());
                    }
                    e.rt.dirty = true;
                    Plan::Done { finished: finish_after }
                }
                Some((h, _)) => {
                    let source = e.actual_location(&env);
                    if source == target {
                        apply(e);
                        e.rt.dirty = true;
                        Plan::Done { finished: finish_after }
                    } else {
                        e.rt.moving = true;
                        Plan::Move(h, e.display_name(), source)
                    }
                }
            }
        };
        let (handle, name, source) = match plan {
            Plan::Done { finished } => {
                if finished {
                    self.announce_finished(&id).await;
                }
                self.kick();
                return;
            }
            Plan::Move(h, n, s) => (h, n, s),
        };
        self.ctx.logger.info(format!(
            "Enqueued torrent move. Torrent: \"{name}\". Source: \"{source}\". Destination: \"{target}\""
        ));
        // qBittorrent moves one torrent at a time.
        let guard = self.move_lock.lock().await;
        self.ctx.logger.info(format!("Start moving torrent. Torrent: \"{name}\". Destination: \"{target}\""));
        let result = match self.engine() {
            Some(engine) => engine.move_storage(&handle, PathBuf::from(&target), overwrite).await,
            None => Err(anyhow::anyhow!("the BitTorrent engine is not running")),
        };
        {
            let prefs = self.ctx.prefs.current();
            let mut st = self.state.write();
            let State { torrents, categories, .. } = &mut *st;
            let env = Env::new(&prefs, categories);
            if let Some(e) = torrents.get_mut(&id) {
                e.rt.moving = false;
                e.rt.dirty = true;
                if result.is_ok() {
                    e.rt.location = Some(target.clone());
                    match ctx {
                        MoveContext::ChangeSavePath => e.save_path = target.clone(),
                        MoveContext::ChangeDownloadPath => {
                            if target != e.save_path(&env) {
                                e.download_path = target.clone();
                            } else {
                                e.download_path.clear();
                            }
                        }
                        MoveContext::AdjustCurrentLocation => {}
                    }
                    // Moving to where the files are recovers missing files.
                    if e.rt.missing_files {
                        e.rt.missing_files = false;
                        e.rt.error = None;
                    }
                }
            }
        }
        match &result {
            Ok(()) => self.ctx.logger.info(format!("Moved torrent successfully. Torrent: \"{name}\". Destination: \"{target}\"")),
            Err(err) => self.ctx.logger.warning(format!(
                "Failed to move torrent. Torrent: \"{name}\". Source: \"{source}\". Destination: \"{target}\". Reason: \"{err:#}\""
            )),
        }
        drop(guard);
        self.reconcile(Some(std::slice::from_ref(&id))).await;
        if finish_after {
            self.announce_finished(&id).await;
        }
    }

    // ---- Renaming ---------------------------------------------------------------------

    pub async fn rename_file(&self, hash: &str, old: &str, new: &str) -> QbtResult<()> {
        let old = paths::normalize_user_path(old);
        let new_raw = new.trim();
        let new = paths::normalize_user_path(new);
        if new_raw.starts_with('/') {
            return Err(QbtError::conflict(format!("Absolute path isn't allowed: '{new_raw}'.")));
        }
        if !paths::is_valid_relative_path(&old) {
            return Err(QbtError::conflict(format!("The old path is invalid: '{old}'.")));
        }
        if !paths::is_valid_relative_path(&new) {
            return Err(QbtError::conflict(format!("The new path is invalid: '{new}'.")));
        }
        let idx = self.with_torrent(hash, |e, _| {
            if !e.has_metadata() {
                return Err(QbtError::conflict("Torrent's metadata has not yet downloaded"));
            }
            let Some(idx) = e.file_paths.iter().position(|p| *p == old) else {
                return Err(QbtError::conflict(format!("No such file: '{old}'.")));
            };
            if old != new && (e.file_paths.contains(&new) || path_conflicts(&e.file_paths, &new, Some(idx))) {
                return Err(QbtError::conflict(format!("The file already exists: '{new}'.")));
            }
            Ok(idx)
        })??;
        if old == new {
            return Ok(());
        }
        self.rename_files(hash, vec![(idx, new)]).await
    }

    pub async fn rename_folder(&self, hash: &str, old: &str, new: &str) -> QbtResult<()> {
        let old = paths::normalize_user_path(old);
        let new_raw = new.trim();
        let new = paths::normalize_user_path(new);
        if new_raw.starts_with('/') {
            return Err(QbtError::conflict(format!("Absolute path isn't allowed: '{new_raw}'.")));
        }
        if !paths::is_valid_relative_path(&old) {
            return Err(QbtError::conflict(format!("The old path is invalid: '{old}'.")));
        }
        if !paths::is_valid_relative_path(&new) {
            return Err(QbtError::conflict(format!("The new path is invalid: '{new}'.")));
        }
        if old == new {
            return Ok(());
        }
        let changes = self.with_torrent(hash, |e, _| {
            if !e.has_metadata() {
                return Err(QbtError::conflict("Torrent's metadata has not yet downloaded"));
            }
            let old_prefix = format!("{old}/");
            let new_prefix = format!("{new}/");
            if e.file_paths.iter().any(|p| p.starts_with(&new_prefix)) {
                return Err(QbtError::conflict(format!("The folder already exists: '{new}'.")));
            }
            let changes: Vec<(usize, String)> = e
                .file_paths
                .iter()
                .enumerate()
                .filter(|(_, p)| p.starts_with(&old_prefix))
                .map(|(i, p)| (i, format!("{new_prefix}{}", &p[old_prefix.len()..])))
                .collect();
            if changes.is_empty() {
                return Err(QbtError::conflict(format!("No such folder: '{old}'.")));
            }
            let moved: std::collections::HashSet<usize> = changes.iter().map(|(i, _)| *i).collect();
            for (_, p) in &changes {
                let others: Vec<String> =
                    e.file_paths.iter().enumerate().filter(|(i, _)| !moved.contains(i)).map(|(_, p)| p.clone()).collect();
                if path_conflicts(&others, p, None) {
                    return Err(QbtError::conflict(format!("The folder already exists: '{new}'.")));
                }
            }
            Ok(changes)
        })??;
        self.rename_files(hash, changes).await
    }

    async fn rename_files(&self, hash: &str, changes: Vec<(usize, String)>) -> QbtResult<()> {
        let (handle, renames, name) = self.with_torrent(hash, |e, _| {
            let renames: Vec<(usize, PathBuf)> = changes
                .iter()
                .filter_map(|(idx, new)| Some((e.meta.as_ref()?.files[*idx].engine_index as usize, PathBuf::from(new))))
                .collect();
            (e.rt.engine.as_ref().map(|(h, _)| h.clone()), renames, e.display_name())
        })?;
        // The engine renames on disk (also while live) and validates the
        // batch; user paths are recorded only when that succeeded.
        if let (Some(h), Some(engine)) = (handle, self.engine())
            && let Err(err) = engine.rename_files(&h, renames).await
        {
            let reason = format!("{err:#}");
            self.ctx.logger.warning(format!("File rename failed. Torrent: \"{name}\", file: \"{}\", reason: \"{reason}\"", changes[0].1));
            return Err(QbtError::conflict(reason));
        }
        self.with_torrent_mut(hash, |e, _| {
            for (idx, new) in changes {
                if let Some(p) = e.file_paths.get_mut(idx) {
                    *p = new;
                }
            }
        })
    }
}

/// True when `new` would need an existing file as a folder, or an existing
/// file lives under `new` (ignoring file `skip`).
fn path_conflicts(files: &[String], new: &str, skip: Option<usize>) -> bool {
    let folder = format!("{new}/");
    files.iter().enumerate().filter(|(i, _)| Some(*i) != skip).any(|(_, p)| {
        p.starts_with(&folder) || new.starts_with(&format!("{p}/"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_conflict_rules() {
        let files = vec!["a/b".to_string(), "c".to_string()];
        assert!(path_conflicts(&files, "c/x", None));
        assert!(path_conflicts(&files, "a", None));
        assert!(!path_conflicts(&files, "a/c", None));
        assert!(!path_conflicts(&files, "c/x", Some(1)));
    }
}
