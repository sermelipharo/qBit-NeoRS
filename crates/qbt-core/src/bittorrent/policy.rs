//! The session timer (one task, every `refresh_interval`): engine status
//! refresh and accounting, queueing, state transitions (checked, finished,
//! errors), share limits, the alternative speed scheduler, persistence
//! cadence, free disk space, trackers from URL, watched folders — plus the
//! preferences watcher and engine (re)starts.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::engine::{Engine, EngineConfig, EnginePhase, ErrorKind, LiveSettings};
use super::queue::{self, QueueItem, QueueLimits};
use super::resume::{self, SessionStateFile};
use super::session::{Session, State, now_secs};
use super::share_limits::{self, ShareInputs};
use super::torrent::{Env, Phase};
use super::{scheduler, trackers};
use crate::events::CoreEvent;
use crate::model::*;
use crate::net::http::{self, HttpPurpose, HttpRequest};
use crate::prefs::{BtProtocol, Preferences};

/// Whether the engine's tracker list (URL, tier) equals the stored one,
/// ignoring order.
fn same_trackers<'a>(new: impl ExactSizeIterator<Item = (&'a str, i64)>, old: &[TrackerTierEntry]) -> bool {
    if new.len() != old.len() {
        return false;
    }
    let mut new: Vec<(&str, i64)> = new.collect();
    let mut old: Vec<(&str, i64)> = old.iter().map(|t| (t.url.as_str(), t.tier)).collect();
    new.sort_unstable();
    old.sort_unstable();
    new == old
}

pub fn client_name() -> String {
    format!("qBit NeoRS/{}", env!("CARGO_PKG_VERSION"))
}

/// Last-run instants of the periodic jobs.
struct Timers {
    last_tick: Instant,
    share_limits: Instant,
    scheduler: Instant,
    free_space: Option<Instant>,
    incoming: Instant,
    resume_all: Instant,
    stats: Instant,
    watched: Instant,
    trackers_url: Option<Instant>,
}

pub(super) enum TorrentEvent {
    Finished(String),
    Errored { id: String, name: String, message: String, disk_full: bool },
    Checked(String),
}

fn is_disk_full(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("no space left") || m.contains("os error 28") || m.contains("disk full")
}

impl Session {
    // ---- Engine lifecycle ---------------------------------------------------------

    pub(super) fn engine_config(&self, prefs: &Preferences) -> EngineConfig {
        let a = &prefs.advanced;
        EngineConfig {
            dht: prefs.bittorrent.dht,
            bootstrap_nodes: a.dht_bootstrap_nodes.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            engine_dir: self.ctx.profile.engine_dir(),
            client_name: client_name(),
        }
    }

    /// The IP filter file's rules (without manual bans); cached by source so
    /// unrelated preference changes do not re-parse it.
    async fn ip_filter_from_file(&self, prefs: &Preferences) -> Option<Arc<crate::ipfilter::IpFilter>> {
        let c = &prefs.connection;
        let path = PathBuf::from(super::paths::expand_home(c.ip_filter_path.trim()));
        let use_file = c.ip_filter_enabled && !c.ip_filter_path.trim().is_empty();
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let key = format!("{use_file}|{}|{mtime:?}", path.display());
        {
            let m = self.misc.lock();
            if let Some((k, v)) = &m.ip_filter_cache
                && *k == key
            {
                return v.clone();
            }
        }
        let logger = self.ctx.logger.clone();
        let (filter, rules) = if use_file {
            tokio::task::spawn_blocking(move || match crate::ipfilter::parse_file(&path) {
                Ok(r) => {
                    for e in &r.errors {
                        logger.critical(e.clone());
                    }
                    if r.error_count > r.errors.len() {
                        logger.critical(format!("{} extra IP filter parsing errors occurred.", r.error_count - r.errors.len()));
                    }
                    logger.info(format!("Successfully parsed the IP filter file. Number of rules applied: {}", r.rule_count));
                    (Some(Arc::new(r.filter)).filter(|f| !f.is_empty()), r.rule_count)
                }
                Err(e) => {
                    logger.critical(e);
                    logger.warning("Failed to parse the IP filter file");
                    (None, 0)
                }
            })
            .await
            .unwrap_or((None, 0))
        } else {
            (None, 0)
        };
        let mut m = self.misc.lock();
        m.ip_filter_rules = rules;
        m.ip_filter_cache = Some((key, filter.clone()));
        filter
    }

    /// Applies the IP filter and the manual bans to the running engine (no
    /// restart); with "Apply to trackers" both also block tracker addresses.
    pub(super) async fn apply_ip_filter(&self, prefs: &Preferences) {
        let Some(engine) = self.engine() else { return };
        let c = &prefs.connection;
        let file = self.ip_filter_from_file(prefs).await;
        let (bans, invalid) = crate::ipfilter::parse_banned(&c.banned_ips);
        for ip in invalid {
            self.ctx.logger.warning(format!("Rejected invalid IP address while applying the list of banned IP addresses. IP: \"{ip}\""));
        }
        engine.set_peer_ip_filter(file.clone());
        engine.set_banned_ips(c.banned_ips.iter().filter_map(|s| crate::ipfilter::normalize_ip(s)).filter_map(|s| s.parse().ok()).collect());
        if c.ip_filter_trackers {
            let mut filter = crate::ipfilter::IpFilter::default();
            if let Some(f) = &file {
                filter.merge_from(f);
            }
            filter.merge_from(&bans);
            engine.set_tracker_ip_filter((!filter.is_empty()).then(|| Arc::new(filter)));
        } else {
            engine.set_tracker_ip_filter(None);
        }
    }

    pub(super) async fn start_engine(&self) {
        let prefs = self.ctx.prefs.current();
        let cfg = self.engine_config(&prefs);
        let live = LiveSettings::from_prefs(&prefs);
        let generation = self.engine_gen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let port = live.net.listen_port;
        let desc = Self::listen_description(&live.net);
        match Engine::start(cfg, live, generation).await {
            Ok((engine, listen_error)) => {
                match listen_error {
                    Some(err) => self.ctx.logger.critical(format!("Failed to listen on IP. {desc}. Reason: \"{err}\"")),
                    None => self.ctx.logger.info(format!(
                        "Successfully listening on IP. {}",
                        Self::listen_description_port(&desc, engine.listen_port().unwrap_or(port))
                    )),
                }
                engine.set_blocked_peer_logger(self.ctx.logger.clone());
                let engine = Arc::new(engine);
                *self.engine.write() = Some(engine.clone());
                self.apply_rate_limits();
                self.apply_ip_filter(&prefs).await;
            }
            Err(e) => {
                self.ctx.logger.critical(format!("BitTorrent session encountered a serious error. Reason: \"{e:#}\""));
            }
        }
    }

    fn listen_description(n: &super::engine::NetPrefs) -> String {
        let proto = match n.protocol {
            BtProtocol::Both => "TCP/UTP",
            BtProtocol::Tcp => "TCP",
            BtProtocol::Utp => "UTP",
        };
        let ip = n.listen_ip.map(|i| i.to_string()).unwrap_or_else(|| "0.0.0.0, ::".to_string());
        format!("IP: \"{ip}\". Port: \"{}/{proto}\"", n.listen_port)
    }

    /// `desc` with the actually bound port (the preference may be 0 = random).
    fn listen_description_port(desc: &str, port: u16) -> String {
        match desc.rsplit_once(". Port: \"") {
            Some((head, tail)) => {
                let proto = tail.split_once('/').map(|(_, p)| p).unwrap_or("");
                format!("{head}. Port: \"{port}/{proto}")
            }
            None => desc.to_string(),
        }
    }

    /// Restarts the engine for settings it cannot change at runtime.
    pub(super) async fn restart_engine(&self, reason: &str) {
        let _guard = self.engine_lock.lock().await;
        self.ctx.logger.info(format!("Restarting the BitTorrent engine to apply new settings ({reason})."));
        let old = self.engine.write().take();
        {
            let mut st = self.state.write();
            for e in st.torrents.values_mut() {
                e.rt.engine = None;
                e.rt.adding = false;
                e.rt.phase = Phase::Detached;
                e.rt.restoring = e.has_metadata() && !e.rt.missing_files;
                e.rt.dl_rate = 0;
                e.rt.ul_rate = 0;
                e.rt.last_fetched = 0;
                e.rt.last_uploaded = 0;
                if let Some(t) = e.rt.metadata_task.take() {
                    t.abort();
                }
            }
        }
        if let Some(old) = old {
            let _ = tokio::time::timeout(Duration::from_secs(10), old.stop()).await;
        }
        {
            let mut m = self.misc.lock();
            m.engine_fetched = 0;
            m.engine_uploaded = 0;
            m.has_incoming = false;
        }
        self.start_engine().await;
        drop(_guard);
        self.kick();
    }

    // ---- Background tasks ---------------------------------------------------------------

    pub(super) fn spawn_background(self: Arc<Self>) {
        let me = Arc::downgrade(&self);
        let mut shutdown = self.shutdown_tx.subscribe();
        self.ctx.rt.spawn(async move {
            let start = Instant::now();
            let mut timers = Timers {
                last_tick: start,
                share_limits: start,
                scheduler: start - Duration::from_secs(60),
                free_space: None,
                incoming: start,
                resume_all: start,
                stats: start,
                watched: start,
                trackers_url: None,
            };
            loop {
                let Some(s) = me.upgrade() else { return };
                let interval = s.ctx.prefs.current().advanced.refresh_interval.clamp(250, 30_000) as u64;
                let woken = async {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(interval)) => {}
                        _ = s.wake.notified() => {}
                    }
                };
                tokio::select! {
                    _ = woken => {}
                    _ = shutdown.changed() => return,
                }
                if *shutdown.borrow() {
                    return;
                }
                s.tick(&mut timers).await;
            }
        });

        let me = Arc::downgrade(&self);
        let mut shutdown = self.shutdown_tx.subscribe();
        let mut rx = self.ctx.prefs.subscribe();
        self.ctx.rt.spawn(async move {
            let mut prev = rx.borrow_and_update().clone();
            loop {
                tokio::select! {
                    r = rx.changed() => if r.is_err() { return },
                    _ = shutdown.changed() => return,
                }
                let new = rx.borrow_and_update().clone();
                let Some(s) = me.upgrade() else { return };
                s.apply_preferences(&prev, &new).await;
                prev = new;
            }
        });
        self.kick();
    }

    async fn apply_preferences(&self, old: &Preferences, new: &Preferences) {
        if old.speed.dl_limit != new.speed.dl_limit
            || old.speed.up_limit != new.speed.up_limit
            || old.speed.alt_dl_limit != new.speed.alt_dl_limit
            || old.speed.alt_up_limit != new.speed.alt_up_limit
        {
            self.apply_rate_limits();
        }
        let sched = |p: &Preferences| {
            (
                p.speed.scheduler_enabled,
                p.speed.schedule_from_hour,
                p.speed.schedule_from_min,
                p.speed.schedule_to_hour,
                p.speed.schedule_to_min,
                p.speed.scheduler_days,
            )
        };
        if sched(old) != sched(new) {
            self.misc.lock().scheduler_last = None;
        }
        if let Some(engine) = self.engine() {
            let live = LiveSettings::from_prefs(new);
            let report = engine.apply_live(&live).await;
            match report.listen {
                Some(Ok(addr)) => {
                    let desc = Self::listen_description(&live.net);
                    self.ctx.logger.info(format!("Successfully listening on IP. {}", Self::listen_description_port(&desc, addr.port())));
                }
                Some(Err(err)) => {
                    let desc = Self::listen_description(&live.net);
                    self.ctx.logger.critical(format!("Failed to listen on IP. {desc}. Reason: \"{err}\""));
                }
                None => {}
            }
            for err in report.errors {
                self.ctx.logger.critical(err);
            }
            if report.fail_closed {
                // Never keep running on the previous proxy or interface (a
                // direct connection): a new session either applies the
                // settings or does not start at all.
                self.misc.lock().restart_pending = Some((Instant::now(), "network settings could not be applied".to_string()));
            }
            // Only the DHT bootstrap configuration still needs a new session.
            let cfg = self.engine_config(new);
            if cfg.restart_required(&engine.cfg) {
                let reason = if cfg.dht && !engine.cfg.dht { "DHT enabled" } else { "DHT bootstrap nodes" };
                self.misc.lock().restart_pending = Some((Instant::now(), reason.to_string()));
            }
            let (oc, nc) = (&old.connection, &new.connection);
            if oc.ip_filter_trackers != nc.ip_filter_trackers
                || oc.ip_filter_path != nc.ip_filter_path
                || oc.ip_filter_enabled != nc.ip_filter_enabled
                || oc.banned_ips != nc.banned_ips
            {
                self.apply_ip_filter(new).await;
            }
            if old.connection.max_connec_per_torrent != new.connection.max_connec_per_torrent
                || old.connection.max_uploads_per_torrent != new.connection.max_uploads_per_torrent
            {
                for e in self.state.write().torrents.values_mut() {
                    e.rt.settings_dirty = true;
                }
            }
        }
        if old.downloads.save_path != new.downloads.save_path
            || old.downloads.temp_path != new.downloads.temp_path
            || old.downloads.temp_path_enabled != new.downloads.temp_path_enabled
        {
            self.default_paths_changed(old, new);
        }
        let b = (&old.bittorrent, &new.bittorrent);
        if b.0.add_trackers_from_url_enabled != b.1.add_trackers_from_url_enabled || b.0.add_trackers_url != b.1.add_trackers_url {
            if new.bittorrent.add_trackers_from_url_enabled {
                self.refresh_trackers_from_url().await;
            } else {
                self.misc.lock().trackers_from_url.clear();
                let _ = std::fs::remove_file(self.ctx.profile.data_dir.join("additional_trackers_from_url.txt"));
                if !new.bittorrent.add_trackers_url_list.is_empty() {
                    let _ = self.ctx.prefs.update(|p| p.bittorrent.add_trackers_url_list.clear());
                }
            }
        }
        self.kick();
    }

    /// Default save / download path changed (relocate or switch to manual).
    fn default_paths_changed(&self, old: &Preferences, new: &Preferences) {
        let mut relocate = Vec::new();
        {
            let mut st = self.state.write();
            let State { torrents, categories, .. } = &mut *st;
            let old_env = Env::new(old, categories);
            for e in torrents.values_mut().filter(|e| e.auto_tmm) {
                let affected = e.category.is_empty()
                    || categories.get(&e.category).is_none_or(|c| !c.save_path.trim().starts_with('/'));
                if !new.downloads.save_path_changed_tmm_enabled && affected {
                    e.save_path = e.save_path(&old_env);
                    e.download_path = e.download_path(&old_env);
                    e.auto_tmm = false;
                    e.rt.dirty = true;
                } else {
                    relocate.push(e.id.clone());
                }
            }
        }
        self.relocate(relocate);
    }

    // ---- The tick -------------------------------------------------------------------------

    async fn tick(&self, timers: &mut Timers) {
        let now = Instant::now();
        let dt_ms = now.duration_since(timers.last_tick).as_millis().min(10_000) as u64;
        timers.last_tick = now;
        let now_s = now_secs();
        let prefs = self.ctx.prefs.current();

        let pending = self.misc.lock().restart_pending.clone();
        if let Some((at, reason)) = pending
            && now.duration_since(at) >= Duration::from_secs(1)
        {
            self.misc.lock().restart_pending = None;
            self.restart_engine(&reason).await;
        }

        let engine = self.engine();
        if let Some(engine) = &engine {
            let s = engine.transfer_stats();
            let mut m = self.misc.lock();
            let delta = |cur: u64, last: u64| if cur >= last { cur - last } else { cur };
            let (pd, pu) = (delta(s.payload_downloaded, m.engine_fetched), delta(s.payload_uploaded, m.engine_uploaded));
            let (td, tu) = (delta(s.total_downloaded, m.engine_total_down), delta(s.total_uploaded, m.engine_total_up));
            let dw = delta(s.wasted, m.engine_wasted);
            m.engine_fetched = s.payload_downloaded;
            m.engine_uploaded = s.payload_uploaded;
            m.engine_total_down = s.total_downloaded;
            m.engine_total_up = s.total_uploaded;
            m.engine_wasted = s.wasted;
            m.session_download += pd as i64;
            m.session_upload += pu as i64;
            m.session_total_download += td as i64;
            m.session_total_upload += tu as i64;
            m.session_wasted += dw as i64;
            if td > 0 || tu > 0 {
                m.stats_dirty = true;
            }
            m.dl_rate = s.payload_dl_rate;
            m.ul_rate = s.payload_ul_rate;
            m.peers_live = s.peers as i64;
            m.dht_nodes = s.dht_nodes;
            let secs = (dt_ms.max(1) as f64) / 1000.0;
            let (total_up, total_down) = ((tu as f64 / secs) as i64, (td as f64 / secs) as i64);
            let (up, down) = (m.ul_rate, m.dl_rate);
            m.speed.record(now_s, total_up.max(up), total_down.max(down), up, down);
        } else {
            self.misc.lock().speed.record(now_s, 0, 0, 0, 0);
        }

        if let Some(engine) = &engine {
            self.poll_port_mapping(engine, &prefs);
        }

        let events = self.update_torrents(engine.as_deref(), &prefs, dt_ms, now, now_s);
        self.save_fetched_piece_layers().await;
        self.update_queue();
        self.reconcile(None).await;

        let mut finished = Vec::new();
        for ev in events {
            match ev {
                TorrentEvent::Finished(id) => finished.push(id),
                TorrentEvent::Errored { id, name, message, disk_full } => {
                    self.ctx.logger.warning(format!("Torrent errored. Torrent: \"{name}\". Error: \"{message}\""));
                    if disk_full || is_disk_full(&message) {
                        self.emit(CoreEvent::FullDiskError { hash: id, name, message });
                    } else {
                        self.emit(CoreEvent::TorrentError { hash: id, name, message });
                    }
                }
                TorrentEvent::Checked(id) => {
                    let _ = id;
                }
            }
        }
        if !finished.is_empty() {
            for id in &finished {
                self.handle_finished(id).await;
            }
            self.check_all_finished();
        }

        if now.duration_since(timers.share_limits) >= Duration::from_secs(10) {
            timers.share_limits = now;
            self.process_share_limits(None).await;
        }
        if now.duration_since(timers.scheduler) >= Duration::from_secs(30) || self.misc.lock().scheduler_last.is_none() {
            timers.scheduler = now;
            self.run_scheduler(&prefs);
        }
        if timers.free_space.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(30)) {
            timers.free_space = Some(now);
            let path = super::torrent::global_paths(&prefs).save_path;
            let free = tokio::task::spawn_blocking(move || crate::utils::fs::free_space(&path).unwrap_or(-1)).await.unwrap_or(-1);
            self.misc.lock().free_space = free;
        }
        if now.duration_since(timers.incoming) >= Duration::from_secs(30) {
            timers.incoming = now;
            self.check_incoming_connections();
        }
        if now.duration_since(timers.watched) >= Duration::from_secs(5) {
            timers.watched = now;
            if !prefs.downloads.watched_folders.is_empty() {
                self.scan_watched_folders().await;
            }
        }
        if prefs.bittorrent.add_trackers_from_url_enabled
            && timers.trackers_url.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(24 * 3600))
        {
            timers.trackers_url = Some(now);
            self.refresh_trackers_from_url().await;
        }

        let resume_interval = prefs.advanced.save_resume_data_interval;
        let save_all = resume_interval > 0 && now.duration_since(timers.resume_all) >= Duration::from_secs(resume_interval as u64 * 60);
        if save_all {
            timers.resume_all = now;
        }
        self.save_dirty(save_all).await;
        let stats_interval = prefs.advanced.save_statistics_interval;
        if stats_interval > 0 && now.duration_since(timers.stats) >= Duration::from_secs(stats_interval as u64 * 60) {
            timers.stats = now;
            let dirty = std::mem::take(&mut self.misc.lock().stats_dirty);
            if dirty {
                let stats = self.stats_file();
                let path = self.ctx.profile.stats_file();
                let _ = tokio::task::spawn_blocking(move || resume::save_json(&path, &stats)).await;
            }
        }
    }

    /// Refreshes every torrent from the engine and detects transitions.
    fn update_torrents(&self, engine: Option<&Engine>, prefs: &Preferences, dt_ms: u64, now: Instant, now_s: i64) -> Vec<TorrentEvent> {
        let mut events = Vec::new();
        let slow_dl = prefs.bittorrent.slow_torrent_dl_rate_threshold.max(0) * 1024;
        let slow_ul = prefs.bittorrent.slow_torrent_ul_rate_threshold.max(0) * 1024;
        let recheck_on_complete = prefs.advanced.recheck_completed_torrents;
        let mut st = self.state.write();
        for e in st.torrents.values_mut() {
            let status = match (&e.rt.engine, engine) {
                (Some((h, g)), Some(engine)) if *g == engine.generation => Some(engine.status(h)),
                _ => None,
            };
            let trackers = match (&e.rt.engine, engine) {
                (Some((h, g)), Some(engine)) if *g == engine.generation => Some(engine.trackers(h)),
                _ => None,
            };
            // A scan of every piece; `TorrentEntry::availability` shows it
            // only for live torrents, so paused ones skip it.
            let avail = match (&e.rt.engine, engine, &status) {
                (Some((h, g)), Some(engine), Some(st)) if *g == engine.generation && matches!(st.phase, EnginePhase::Live) => {
                    engine.distributed_copies(h)
                }
                _ => -1.0,
            };
            let Some(status) = status else {
                e.rt.dl_rate = 0;
                e.rt.ul_rate = 0;
                e.rt.peers_live = 0;
                e.rt.seeders_live = 0;
                e.rt.avg.push(0, 0);
                continue;
            };
            let was_checking = matches!(e.rt.phase, Phase::Checking { .. });
            e.rt.phase = match &status.phase {
                EnginePhase::Initializing { checked_bytes, .. } => Phase::Checking { checked_bytes: *checked_bytes },
                EnginePhase::Paused => Phase::Paused,
                EnginePhase::Live => Phase::Live,
                EnginePhase::Error(msg, ErrorKind::MissingFiles) => {
                    if !e.rt.missing_files && !e.rt.retry_missing {
                        e.rt.missing_files = true;
                        self.ctx.logger.warning(format!(
                            "Failed to restore torrent. Files were probably moved or storage isn't accessible. Torrent: \"{}\". Reason: \"{msg}\"",
                            e.display_name()
                        ));
                    }
                    Phase::Paused
                }
                EnginePhase::Error(msg, kind) => {
                    if e.rt.error.is_none() && !e.rt.retry_missing {
                        e.rt.error = Some(if msg.is_empty() { "Unknown error".into() } else { msg.clone() });
                        e.rt.error_reported = false;
                        e.rt.disk_full = *kind == ErrorKind::DiskFull;
                    }
                    Phase::Paused
                }
            };
            if !matches!(status.phase, EnginePhase::Error(..)) {
                e.rt.retry_missing = false;
            }
            if let Some(msg) = &e.rt.error
                && !e.rt.error_reported
            {
                e.rt.error_reported = true;
                events.push(TorrentEvent::Errored {
                    id: e.id.clone(),
                    name: e.display_name(),
                    message: msg.clone(),
                    disk_full: e.rt.disk_full,
                });
            }
            e.rt.wasted = status.wasted as i64;
            e.rt.connections = status.connections;
            e.rt.availability = avail;
            if let Some(t) = trackers {
                e.rt.swarm_seeds = t.swarm_seeds;
                e.rt.swarm_leechers = t.swarm_leechers;
                let s = &t.statuses;
                use librqbit::TrackerState as TS;
                e.rt.tracker_warning = s.iter().any(|x| x.state == TS::Working && !x.message.is_empty());
                e.rt.tracker_error = s.iter().any(|x| x.state == TS::TrackerError);
                e.rt.tracker_other_error = s.iter().any(|x| matches!(x.state, TS::NotWorking | TS::Unreachable));
                e.rt.working_tracker = s.iter().find(|x| x.state == TS::Working).map(|x| x.url.clone());
                e.rt.next_announce =
                    s.iter().filter_map(|x| x.next_announce_in).min().map(|d| d.as_secs() as i64).unwrap_or(0);
                // Runs for every torrent on every tick: compare by reference
                // and only build the owned list when something changed.
                if (!s.is_empty() || e.trackers.is_empty()) && !same_trackers(s.iter().map(|x| (x.url.as_str(), x.tier as i64)), &e.trackers) {
                    e.trackers = s.iter().map(|x| TrackerTierEntry { url: x.url.clone(), tier: x.tier as i64 }).collect();
                    e.rt.dirty = true;
                }
            }
            let checking = matches!(e.rt.phase, Phase::Checking { .. });
            if !status.file_progress.is_empty() && !checking {
                e.rt.file_progress = e.map_file_progress(&status.file_progress);
            }
            if was_checking && !checking {
                // Files checked.
                e.rt.unchecked = false;
                events.push(TorrentEvent::Checked(e.id.clone()));
                if e.stop_condition == StopCondition::FilesChecked {
                    e.stop_condition = StopCondition::None;
                    e.stopped = true;
                    self.ctx.logger.info(format!("Torrent stopped. Torrent: \"{}\"", e.display_name()));
                }
                e.rt.dirty = true;
            }
            e.rt.finished = !checking && (status.finished || e.wanted_complete());
            let live = matches!(e.rt.phase, Phase::Live);
            if live {
                e.rt.dl_rate = status.dl_rate;
                e.rt.ul_rate = status.ul_rate;
                let df = if status.fetched >= e.rt.last_fetched { status.fetched - e.rt.last_fetched } else { status.fetched };
                let du = if status.uploaded >= e.rt.last_uploaded { status.uploaded - e.rt.last_uploaded } else { status.uploaded };
                e.rt.last_fetched = status.fetched;
                e.rt.last_uploaded = status.uploaded;
                if df > 0 {
                    e.all_time_download += df as i64;
                    e.rt.session_download += df as i64;
                    e.last_download = now_s;
                    e.rt.unchecked = true;
                }
                if du > 0 {
                    e.all_time_upload += du as i64;
                    e.rt.session_upload += du as i64;
                    e.last_upload = now_s;
                }
                e.rt.active_ms += dt_ms;
                e.active_time += (e.rt.active_ms / 1000) as i64;
                e.rt.active_ms %= 1000;
                if e.rt.finished {
                    e.rt.seeding_ms += dt_ms;
                    e.seeding_time += (e.rt.seeding_ms / 1000) as i64;
                    e.rt.seeding_ms %= 1000;
                }
            } else {
                e.rt.dl_rate = 0;
                e.rt.ul_rate = 0;
                e.rt.last_fetched = 0;
                e.rt.last_uploaded = 0;
            }
            e.rt.avg.push(e.rt.dl_rate, e.rt.ul_rate);
            e.rt.peers_live = status.peers_live;
            e.rt.seeders_live = status.seeders_live;
            if e.rt.finished || e.rt.seeders_live > 0 {
                e.seen_complete = now_s;
            }
            // Slow torrent detection for "do not count slow torrents".
            if live && e.rt.dl_rate < slow_dl && e.rt.ul_rate < slow_ul {
                e.rt.slow_since.get_or_insert(now);
            } else {
                e.rt.slow_since = None;
            }
            // Finished transition (`handleTorrentFinished`).
            if e.rt.finished && !e.seed_status && !e.stopped && e.rt.error.is_none() {
                if recheck_on_complete && e.rt.unchecked && !e.rt.recheck_on_complete_pending {
                    e.rt.recheck_on_complete_pending = true;
                    e.rt.unchecked = false;
                    events.push(TorrentEvent::Checked(format!("recheck:{}", e.id)));
                } else {
                    e.rt.recheck_on_complete_pending = false;
                    e.seed_status = true;
                    e.completed_on = now_s;
                    e.queue_position = -1;
                    e.rt.dirty = true;
                    events.push(TorrentEvent::Finished(e.id.clone()));
                }
            }
        }
        drop(st);
        // Recheck-on-completion requests are carried as Checked("recheck:id").
        let mut out = Vec::with_capacity(events.len());
        for ev in events {
            match ev {
                TorrentEvent::Checked(id) if id.starts_with("recheck:") => {
                    let id = id["recheck:".len()..].to_string();
                    if let Some(me) = self.arc() {
                        self.ctx.rt.spawn(async move {
                            let _ = me.recheck(&[id]).await;
                        });
                    }
                }
                ev => out.push(ev),
            }
        }
        out
    }

    /// Recomputes which torrents are held in the queue (§4.5).
    pub(super) fn update_queue(&self) {
        let prefs = self.ctx.prefs.current();
        let b = &prefs.bittorrent;
        let limits = QueueLimits {
            enabled: b.queueing_enabled,
            max_downloads: b.max_active_downloads,
            max_uploads: b.max_active_uploads,
            max_active: b.max_active_torrents,
            dont_count_slow: b.dont_count_slow_torrents,
        };
        let timer = Duration::from_secs(b.slow_torrent_inactive_timer.max(1) as u64);
        let now = Instant::now();
        let mut needs_position = false;
        let mut st = self.state.write();
        let ids: Vec<String> = st.torrents.keys().cloned().collect();
        let items: Vec<QueueItem> = ids
            .iter()
            .map(|id| {
                let e = &st.torrents[id];
                let can_run = !e.stopped && e.rt.error.is_none() && !e.rt.missing_files && !e.rt.moving;
                if !e.is_finished() && !e.seed_status && e.queue_position < 0 {
                    needs_position = true;
                }
                QueueItem {
                    auto_managed: can_run && !e.forced,
                    forced_active: can_run && e.forced,
                    finished: e.has_metadata() && (e.is_finished() || e.seed_status),
                    position: e.queue_position,
                    seed_rank: e.seeding_time,
                    inactive: e.rt.slow_since.is_some_and(|t| now.duration_since(t) >= timer),
                }
            })
            .collect();
        let queued = queue::decide(&items, &limits);
        for (id, q) in ids.iter().zip(queued) {
            if let Some(e) = st.torrents.get_mut(id) {
                e.rt.queued = q;
            }
        }
        if needs_position {
            let mut next = st.torrents.values().map(|e| e.queue_position).max().unwrap_or(-1) + 1;
            for e in st.torrents.values_mut() {
                if !e.is_finished() && !e.seed_status && e.queue_position < 0 {
                    e.queue_position = next;
                    next += 1;
                    e.rt.dirty = true;
                }
            }
        }
        drop(st);
        self.normalize_queue();
    }

    // ---- Finished torrents ---------------------------------------------------------------

    async fn handle_finished(&self, id: &str) {
        // Move from the incomplete folder to the save path first; the
        // finished notification follows the move.
        let target = {
            let prefs = self.ctx.prefs.current();
            let st = self.state.read();
            let env = Env::new(&prefs, &st.categories);
            st.torrents.get(id).and_then(|e| {
                let target = e.save_path(&env);
                (e.actual_location(&env) != target).then_some(target)
            })
        };
        match (target, self.arc()) {
            (Some(target), Some(me)) => {
                let id = id.to_string();
                self.ctx.rt.spawn(async move {
                    me.move_storage(id, target, true, super::actions::MoveContext::AdjustCurrentLocation, true).await
                });
            }
            _ => self.announce_finished(id).await,
        }
    }

    /// Completion actions: log, export, external program, e-mail, event,
    /// share limits.
    pub(super) async fn announce_finished(&self, id: &str) {
        let prefs = self.ctx.prefs.current();
        let Ok((name, wanted, active, save_path, wanted_idx)) = self.with_torrent(id, |e, env| {
            let wanted_idx: Vec<usize> = e
                .meta
                .as_ref()
                .map(|m| {
                    m.files
                        .iter()
                        .zip(&e.file_priorities)
                        .filter(|(_, p)| **p != FilePriority::Ignored)
                        .map(|(f, _)| f.engine_index as usize)
                        .collect()
                })
                .unwrap_or_default();
            (e.display_name(), e.wanted_size(), e.active_time, e.save_path(env), wanted_idx)
        }) else {
            return;
        };
        self.ctx.logger.info(format!("Torrent download finished. Torrent: \"{name}\""));
        // Mark-of-the-Web on the downloaded files (macOS quarantine).
        if prefs.advanced.mark_of_the_web
            && let (Some(h), Some(engine)) = (self.handle_of(id), self.engine())
        {
            let root = engine.output_folder(&h);
            let actual = engine.actual_file_paths(&h);
            let files: Vec<std::path::PathBuf> = wanted_idx.iter().filter_map(|i| actual.get(*i)).map(|p| root.join(p)).collect();
            let ctx = self.ctx.clone();
            let _ = tokio::task::spawn_blocking(move || {
                for f in files {
                    let _ = crate::utils::os::apply_mark_of_the_web(&ctx, &f);
                }
            })
            .await;
        }
        self.export_to(&prefs.downloads.export_dir_fin, id, &name).await;
        if prefs.downloads.autorun_enabled {
            self.run_external_program(&prefs.downloads.autorun_program, id);
        }
        if prefs.downloads.mail_notification_enabled {
            let ctx = self.ctx.clone();
            let n = name.clone();
            self.ctx.rt.spawn(async move {
                if let Err(e) = crate::net::smtp::notify_torrent_finished(&ctx, &n, wanted, &save_path, active).await {
                    ctx.logger.warning(format!("Failed to send the notification e-mail: {e}"));
                }
            });
        }
        self.emit(CoreEvent::TorrentFinished { hash: id.to_string(), name });
        self.process_share_limits(Some(id)).await;
    }

    fn check_all_finished(&self) {
        let st = self.state.read();
        let pending = st
            .torrents
            .values()
            .any(|e| !e.is_finished() && !e.stopped && e.rt.error.is_none() && !e.rt.missing_files);
        drop(st);
        if !pending {
            self.emit(CoreEvent::AllTorrentsFinished);
        }
    }

    // ---- Share limits ----------------------------------------------------------------------

    pub(super) async fn process_share_limits(&self, only: Option<&str>) {
        let prefs = self.ctx.prefs.current();
        let decisions: Vec<(String, String, share_limits::ShareDecision)> = {
            let st = self.state.read();
            let env = Env::new(&prefs, &st.categories);
            st.torrents
                .values()
                .filter(|e| only.is_none_or(|id| id == e.id))
                .filter_map(|e| {
                    let inputs = ShareInputs {
                        finished: e.is_finished(),
                        forced: e.forced && !e.stopped,
                        stopped: e.stopped,
                        super_seeding: e.super_seeding,
                        ratio: e.real_ratio(),
                        seeding_secs: e.seeding_time,
                        time_since_activity: e.time_since_activity(env.now),
                        ratio_limit: e.effective_ratio_limit(&env),
                        seeding_time_limit: e.effective_seeding_time_limit(&env),
                        inactive_seeding_time_limit: e.effective_inactive_seeding_time_limit(&env),
                        action: e.effective_share_limit_action(&env),
                    };
                    share_limits::evaluate(&inputs).map(|d| (e.id.clone(), e.display_name(), d))
                })
                .collect()
        };
        for (id, name, d) in decisions {
            self.ctx.logger.info(format!("{} {} Torrent: \"{name}\".", d.description, d.action_text));
            let ids = [id];
            let _ = match d.action {
                ShareLimitAction::Remove => self.remove(&ids, false).await,
                ShareLimitAction::RemoveWithContent => self.remove(&ids, true).await,
                ShareLimitAction::EnableSuperSeeding => self.set_super_seeding(&ids, true),
                _ => self.stop(&ids).await,
            };
        }
    }

    // ---- Scheduler, stats, misc -------------------------------------------------------------

    fn run_scheduler(&self, prefs: &Preferences) {
        let s = &prefs.speed;
        if !s.scheduler_enabled {
            self.misc.lock().scheduler_last = Some(false);
            return;
        }
        let alt = scheduler::is_time_for_alternative_now(
            s.schedule_from_hour,
            s.schedule_from_min,
            s.schedule_to_hour,
            s.schedule_to_min,
            s.scheduler_days,
        );
        let changed = {
            let mut m = self.misc.lock();
            let changed = m.scheduler_last != Some(alt);
            m.scheduler_last = Some(alt);
            changed
        };
        // Only computed changes toggle the mode; manual toggles persist
        // until the next change.
        if changed {
            self.set_alt_speed_limits_enabled(alt);
        }
    }

    /// Reports the external address (learned through UPnP / NAT-PMP, else from
    /// trackers) and mapping failures.
    fn poll_port_mapping(&self, engine: &Engine, prefs: &Preferences) {
        let st = engine.port_mapping_status();
        let error = st.natpmp_error.clone().or_else(|| st.upnp_error.clone());
        let mut changed = None;
        let mut new_error = None;
        {
            let mut m = self.misc.lock();
            // The port mapping gateway knows the address best; otherwise use
            // what trackers saw (`external ip`, BEP 24).
            let (tracker_v4, tracker_v6) = engine.tracker_external_ips();
            let mapped = st.external_ip;
            let v4 = mapped.filter(|ip| ip.is_ipv4()).or(tracker_v4);
            let v6 = mapped.filter(|ip| ip.is_ipv6()).or(tracker_v6);
            for ip in [v4, v6].into_iter().flatten() {
                let text = ip.to_string();
                let slot = if ip.is_ipv4() { &mut m.external_v4 } else { &mut m.external_v6 };
                if *slot != text {
                    let had = !slot.is_empty();
                    slot.clone_from(&text);
                    changed = Some((text, had));
                }
            }
            if m.mapping_error != error {
                new_error = error.clone();
                m.mapping_error = error;
            }
        }
        if let Some(err) = new_error {
            self.ctx.logger.warning(format!("UPnP/NAT-PMP: Port mapping failure, message: \"{err}\""));
        }
        if let Some((address, had_previous)) = changed {
            self.ctx.logger.info(format!("Detected external IP. IP: \"{address}\""));
            if had_previous && prefs.advanced.reannounce_when_address_changed {
                engine.reannounce_all();
            }
            self.emit(CoreEvent::ExternalAddressChanged { address });
        }
    }

    fn check_incoming_connections(&self) {
        if self.misc.lock().has_incoming {
            return;
        }
        let Some(engine) = self.engine() else { return };
        // Incoming peers that connected and left between two polls count too.
        let handles: Vec<_> = self
            .state
            .read()
            .torrents
            .values()
            .filter(|e| matches!(e.rt.phase, Phase::Live))
            .filter_map(|e| e.rt.engine.as_ref().map(|(h, _)| h.clone()))
            .collect();
        let incoming = engine.incoming_connection_seen() || handles.iter().any(|h| engine.peers(h).iter().any(|p| p.flags.incoming));
        if incoming {
            self.misc.lock().has_incoming = true;
        }
    }

    async fn save_dirty(&self, all: bool) {
        let records: Vec<resume::FastResume> = {
            let mut st = self.state.write();
            st.torrents
                .values_mut()
                .filter(|e| all || e.rt.dirty)
                .map(|e| {
                    e.rt.dirty = false;
                    e.to_resume()
                })
                .collect()
        };
        if records.is_empty() {
            return;
        }
        let backup = self.backup.clone();
        let logger = self.ctx.logger.clone();
        let known: HashSet<String> = self.state.read().torrents.keys().cloned().collect();
        let _ = tokio::task::spawn_blocking(move || {
            for r in &records {
                // Skip torrents removed meanwhile.
                if !known.contains(&r.info_hash) {
                    continue;
                }
                if let Err(e) = backup.save_resume(r) {
                    logger.critical(format!(
                        "Couldn't save torrent resume data to '{}'. Error: {e}.",
                        backup.resume_path(&r.info_hash).display()
                    ));
                }
            }
        })
        .await;
    }

    /// Downloads the "trackers from URL" list (general-purpose proxy) and
    /// caches it in `data/additional_trackers_from_url.txt`.
    pub(super) async fn refresh_trackers_from_url(&self) {
        let prefs = self.ctx.prefs.current();
        let url = prefs.bittorrent.add_trackers_url.trim().to_string();
        let cache = self.ctx.profile.data_dir.join("additional_trackers_from_url.txt");
        if self.misc.lock().trackers_from_url.is_empty()
            && let Ok(data) = resume::read_limited(&cache, 4 * 1024 * 1024)
        {
            self.misc.lock().trackers_from_url = trackers::parse_tracker_entries(&String::from_utf8_lossy(&data));
        }
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return;
        }
        let req = HttpRequest { url, cookie: None, max_size: 4 * 1024 * 1024, timeout_secs: 60 };
        match http::fetch(&self.ctx, req, HttpPurpose::General).await {
            Ok(resp) if (200..300).contains(&resp.status) => {
                let text = String::from_utf8_lossy(&resp.body).trim().to_string();
                let list = trackers::parse_tracker_entries(&text);
                let _ = resume::write_private_atomic(&cache, text.as_bytes());
                self.misc.lock().trackers_from_url = list;
                if prefs.bittorrent.add_trackers_url_list != text {
                    let _ = self.ctx.prefs.update(|p| p.bittorrent.add_trackers_url_list = text.clone());
                }
                self.ctx.logger.info("Tracker list updated");
            }
            Ok(resp) => self.ctx.logger.warning(format!("Failed to update tracker list. Reason: \"HTTP error {}\"", resp.status)),
            Err(e) => self.ctx.logger.warning(format!("Failed to update tracker list. Reason: \"{e}\"")),
        }
    }

    /// Persists the alternative-speed mode (also used at shutdown).
    pub(super) fn save_session_state(&self) {
        let path = self.ctx.profile.data_dir.join("session_state.json");
        let _ = resume::save_json(&path, &SessionStateFile { use_alt_speed_limits: self.alt_speed_limits_enabled() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_lists_compare_ignoring_order() {
        let stored = vec![
            TrackerTierEntry { url: "udp://b".into(), tier: 1 },
            TrackerTierEntry { url: "udp://a".into(), tier: 0 },
        ];
        assert!(same_trackers([("udp://a", 0), ("udp://b", 1)].into_iter(), &stored));
        assert!(!same_trackers([("udp://a", 0), ("udp://b", 2)].into_iter(), &stored));
        assert!(!same_trackers([("udp://a", 0)].into_iter(), &stored));
        assert!(same_trackers(std::iter::empty(), &[]));
    }

    #[test]
    fn disk_full_detection() {
        assert!(is_disk_full("No space left on device (os error 28)"));
        assert!(!is_disk_full("permission denied"));
    }
}
