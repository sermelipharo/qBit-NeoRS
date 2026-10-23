//! Adding torrents (`SessionImpl::addTorrent_impl`), metadata download for
//! magnets, the add dialog's metadata preview, duplicates and the
//! "Copy .torrent files to" export.

use std::path::Path;
use std::sync::Arc;

use super::categories::{self, CategoryMap};
use super::metainfo::{self, MagnetLink, ParsedTorrent, TorrentMeta};
use super::session::{Session, now_secs};
use super::torrent::{self, Env, TorrentEntry};
use super::{autorun, paths, resume, trackers};
use crate::error::{QbtError, QbtResult};
use crate::events::CoreEvent;
use crate::model::*;
use crate::net::http::{self, HttpPurpose, HttpRequest};
use crate::prefs::{AutoDeleteMode, Preferences};

/// Total time a .torrent URL download may take.
const TORRENT_DOWNLOAD_TIMEOUT_SECS: u64 = 300;

/// A torrent to add: full metadata or a magnet link.
pub(super) enum Descriptor {
    Meta { bytes: Arc<Vec<u8>>, parsed: Box<ParsedTorrent> },
    Magnet(MagnetLink),
}

/// Adds the trackers and web seeds of a magnet link to metadata resolved for it.
fn merge_magnet_sources(meta: &mut TorrentMeta, link: &MagnetLink) {
    trackers::add_trackers(&mut meta.trackers, &link.trackers);
    for seed in &link.url_seeds {
        if !meta.url_seeds.contains(seed) && meta.url_seeds.len() < metainfo::MAX_WEB_SEEDS {
            meta.url_seeds.push(seed.clone());
        }
    }
}

impl Descriptor {
    fn id(&self) -> String {
        match self {
            Descriptor::Meta { parsed, .. } => parsed.meta.id(),
            Descriptor::Magnet(m) => metainfo::hex(&m.info_hash),
        }
    }

    /// The v1 hash of a hybrid torrent or of a magnet link with both hashes.
    fn hybrid_v1(&self) -> Option<String> {
        match self {
            Descriptor::Meta { parsed, .. } => parsed.meta.alt_id(),
            Descriptor::Magnet(m) => m.info_hash_v1.filter(|_| m.info_hash_v2.is_some()).map(|h| metainfo::hex(&h)),
        }
    }
}

impl Session {
    /// Resolves a source into a descriptor. Only http(s) URLs, magnet links
    /// and bare info hashes are accepted as URLs.
    pub(super) async fn load_source(&self, source: &TorrentSource, cookie: Option<String>) -> Result<Descriptor, String> {
        let limit = self.torrent_file_limit();
        match source {
            TorrentSource::Url { url } => {
                let url = url.trim();
                let lower = url.to_ascii_lowercase();
                if lower.starts_with("magnet:") || metainfo::parse_bare_hash(url).is_some() || metainfo::is_v2_hash(url) {
                    let link = metainfo::parse_magnet(url)?;
                    let id = metainfo::hex(&link.info_hash);
                    if let Some(bytes) = self.cached_metadata(&id) {
                        // Metadata already fetched for the add dialog: the link's own trackers and web seeds
                        // (`tr`, `ws`) still belong to the torrent being added.
                        let mut desc = self.parse_descriptor(bytes)?;
                        if let Descriptor::Meta { parsed, .. } = &mut desc {
                            merge_magnet_sources(&mut parsed.meta, &link);
                        }
                        return Ok(desc);
                    }
                    return Ok(Descriptor::Magnet(link));
                }
                if !(lower.starts_with("http://") || lower.starts_with("https://")) {
                    return Err("unsupported URL scheme (only http, https and magnet links are accepted)".into());
                }
                self.ctx.logger.info(format!("Downloading torrent... Source: \"{url}\""));
                // Bounded in total: a server dripping one byte per idle period
                // would otherwise keep the add pending forever.
                let req = HttpRequest { url: url.to_string(), cookie, max_size: limit, timeout_secs: TORRENT_DOWNLOAD_TIMEOUT_SECS };
                let resp = http::fetch(&self.ctx, req, HttpPurpose::General).await.map_err(|e| e.to_string())?;
                if !(200..300).contains(&resp.status) {
                    return Err(format!("HTTP error {}", resp.status));
                }
                if resp.body.len() as u64 > limit {
                    return Err(format!("The file size ({}) exceeds the download limit ({limit})", resp.body.len()));
                }
                if resp.body.starts_with(b"magnet:") {
                    let text = String::from_utf8_lossy(&resp.body).trim().to_string();
                    return metainfo::parse_magnet(&text).map(Descriptor::Magnet);
                }
                self.parse_descriptor(Arc::new(resp.body))
            }
            TorrentSource::File { path } => {
                let p = Path::new(path.trim());
                let bytes = tokio::task::spawn_blocking({
                    let p = p.to_path_buf();
                    move || resume::read_limited(&p, limit)
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
                self.parse_descriptor(Arc::new(bytes))
            }
            TorrentSource::Bytes { data } => {
                if data.len() as u64 > limit {
                    return Err(format!("The file size ({}) exceeds the limit ({limit})", data.len()));
                }
                self.parse_descriptor(Arc::new(data.clone()))
            }
        }
    }

    fn parse_descriptor(&self, bytes: Arc<Vec<u8>>) -> Result<Descriptor, String> {
        let parsed = metainfo::parse_torrent(&bytes, self.limits())?;
        Ok(Descriptor::Meta { bytes, parsed: Box::new(parsed) })
    }

    pub async fn add_torrent(&self, source: TorrentSource, params: AddTorrentParams) -> QbtResult<String> {
        self.add_torrent_from(source, params, true).await
    }

    /// Adds a torrent whose bytes were downloaded (search plugins, metadata
    /// fetched from a URL): never a trusted local source.
    pub(crate) async fn add_remote_torrent(&self, source: TorrentSource, params: AddTorrentParams) -> QbtResult<String> {
        self.add_torrent_from(source, params, false).await
    }

    async fn add_torrent_from(&self, source: TorrentSource, params: AddTorrentParams, trusted: bool) -> QbtResult<String> {
        let label = match &source {
            TorrentSource::Url { url } => url.trim().to_string(),
            TorrentSource::File { path } => path.clone(),
            TorrentSource::Bytes { .. } => "<data>".to_string(),
        };
        let delete_mode = self.ctx.prefs.current().downloads.auto_delete_mode;
        let file_source = match &source {
            TorrentSource::File { path } => Some(path.clone()),
            _ => None,
        };
        // Only .torrent files handed to us directly are a local source whose
        // trackers and web seeds are trusted (libtorrent's SSRF rule);
        // URLs, RSS, search plugins and magnets are not.
        let trusted = trusted && matches!(source, TorrentSource::File { .. } | TorrentSource::Bytes { .. });
        let result = match self.load_source(&source, params.cookie.clone()).await {
            Ok(desc) => self.add_descriptor(desc, params, false, trusted).await,
            Err(reason) => Err(QbtError::invalid(reason)),
        };
        if let Err(err) = &result
            && !matches!(err, QbtError::Conflict { .. })
        {
            let reason = match err {
                QbtError::InvalidArgument { message } | QbtError::Network { message } => message.clone(),
                e => e.to_string(),
            };
            self.ctx.logger.warning(format!("Failed to add torrent. Source: \"{label}\". Reason: \"{reason}\""));
            self.emit(CoreEvent::AddTorrentFailed { source: label.clone(), reason });
        }
        if let Some(path) = file_source {
            let delete = match delete_mode {
                AutoDeleteMode::Never => false,
                AutoDeleteMode::IfAdded => result.is_ok(),
                AutoDeleteMode::Always => true,
            };
            if delete {
                let _ = std::fs::remove_file(path);
            }
        }
        result
    }

    /// Core of `addTorrent_impl`. `from_watcher` skips the duplicate event
    /// noise for folders being re-scanned. `trusted_source`: the metadata
    /// came from a local .torrent file (its trackers and web seeds are
    /// trusted by SSRF mitigation; magnets never are).
    pub(super) async fn add_descriptor(&self, desc: Descriptor, params: AddTorrentParams, from_watcher: bool, trusted_source: bool) -> QbtResult<String> {
        let id = desc.id();
        if let Some(existing) = self.find_existing(&desc) {
            return self.handle_duplicate(&existing, desc, from_watcher).await;
        }
        let prefs = self.ctx.prefs.current();
        let now = now_secs();
        let mut e = TorrentEntry::new(id.clone());
        e.added_on = now;
        e.name = params.name.as_deref().map(|n| n.replace(['\r', '\n'], " ").trim().chars().filter(|c| !c.is_control()).collect()).unwrap_or_default();

        // Category (created on the fly; invalid names are dropped) and tags.
        let category = params.category.as_deref().map(str::trim).unwrap_or("").to_string();
        let mut created_category = false;
        let mut created_tags = false;
        {
            let mut st = self.state.write();
            if !category.is_empty() {
                if st.categories.contains_key(&category) {
                    e.category = category.clone();
                } else if categories::is_valid_category_name(&category) {
                    for c in categories::expand_category(&category) {
                        st.categories.entry(c.clone()).or_insert_with(|| categories::new_category(&c));
                    }
                    e.category = category.clone();
                    created_category = true;
                }
            }
            for t in &params.tags {
                let t = t.trim();
                if categories::is_valid_tag(t) && !e.tags.iter().any(|x| x == t) {
                    if st.tags.insert(t.to_string()) {
                        created_tags = true;
                    }
                    e.tags.push(t.to_string());
                }
            }
            e.tags.sort();
        }
        if created_category {
            self.save_categories();
        }
        if created_tags {
            self.save_tags();
        }

        e.stopped = params.stopped.unwrap_or(prefs.downloads.add_stopped_enabled);
        e.stop_condition = params.stop_condition.unwrap_or(prefs.downloads.torrent_stop_condition);
        let add_to_top = params.add_to_queue_top.unwrap_or(prefs.downloads.add_to_top_of_queue);
        e.content_layout = params.content_layout.unwrap_or(prefs.downloads.torrent_content_layout);
        e.forced = params.forced;
        e.sequential = params.sequential;
        e.first_last = params.first_last_piece_priority;
        e.ratio_limit = params.ratio_limit.map(|r| if r < -2.0 { -1.0 } else { r }).unwrap_or(-2.0);
        e.seeding_time_limit = params.seeding_time_limit.map(|v| v.max(-2)).unwrap_or(-2);
        e.inactive_seeding_time_limit = params.inactive_seeding_time_limit.map(|v| v.max(-2)).unwrap_or(-2);
        e.share_limit_action = params.share_limit_action.unwrap_or_default();
        e.dl_limit = params.download_limit.filter(|v| *v > 0).unwrap_or(0);
        e.ul_limit = params.upload_limit.filter(|v| *v > 0).unwrap_or(0);

        // Paths (initLoadTorrentParams).
        {
            let st = self.state.read();
            resolve_paths(&mut e, &params, &prefs, &st.categories);
        }

        let mut torrent_bytes = None;
        let private;
        match desc {
            Descriptor::Meta { bytes, parsed } => {
                let meta = Arc::new(parsed.meta);
                private = meta.private;
                if e.stop_condition == StopCondition::MetadataReceived {
                    e.stopped = true;
                    e.stop_condition = StopCondition::None;
                }
                e.trackers = meta.trackers.clone();
                e.url_seeds = meta.url_seeds.clone();
                if trusted_source {
                    let urls: Vec<String> = e.trackers.iter().map(|t| t.url.clone()).chain(e.url_seeds.iter().cloned()).collect();
                    e.trust_urls(urls.iter().map(String::as_str));
                }
                let n = meta.files.len();
                let explicit_paths: Vec<String> = params.file_paths.iter().map(|p| paths::normalize_user_path(p)).collect();
                e.file_paths = if explicit_paths.len() == n
                    && explicit_paths.iter().all(|p| paths::is_valid_relative_path(p))
                    && explicit_paths.iter().collect::<std::collections::HashSet<_>>().len() == n
                {
                    explicit_paths
                } else {
                    paths::apply_content_layout(&meta.file_paths(), e.content_layout)
                };
                if e.name.is_empty() {
                    let content_name = paths::find_root_folder(&e.file_paths)
                        .or_else(|| (n == 1).then(|| e.file_paths[0].rsplit('/').next().unwrap_or("").to_string()))
                        .unwrap_or_default();
                    if !content_name.is_empty() && content_name != meta.name {
                        e.name = content_name;
                    }
                }
                e.file_priorities = if params.file_priorities.len() == n {
                    params.file_priorities.clone()
                } else {
                    default_priorities(&meta, &e.file_paths, &prefs)
                };
                if params.skip_checking {
                    // ENGINE: hash checks cannot be skipped; the data is
                    // verified anyway, but no "finished" event is sent.
                    e.seed_status = true;
                }
                torrent_bytes = Some(bytes);
                e.meta = Some(meta);
            }
            Descriptor::Magnet(link) => {
                private = false;
                e.magnet_name = link.name.clone();
                e.magnet_v2 = link.info_hash_v2.map(|h| metainfo::hex(&h)).unwrap_or_default();
                if link.info_hash_v2.is_some() {
                    e.magnet_v1 = link.info_hash_v1.map(|h| metainfo::hex(&h)).unwrap_or_default();
                }
                e.trackers = link.trackers.clone();
                e.url_seeds = link.url_seeds.clone();
                e.peers = link.peers.clone();
            }
        }
        let extra: Vec<TrackerTierEntry> =
            params.trackers.iter().filter(|u| metainfo::is_valid_tracker_url(u)).map(|u| TrackerTierEntry { url: u.trim().to_string(), tier: 0 }).collect();
        trackers::append_after_last_tier(&mut e.trackers, &extra);
        for u in &params.url_seeds {
            if metainfo::is_web_url(u) && !e.url_seeds.contains(u) {
                e.url_seeds.push(u.trim().to_string());
            }
        }
        // Trackers and web seeds given with the add request are user input.
        e.trust_urls(extra.iter().map(|t| t.url.as_str()).chain(params.url_seeds.iter().map(String::as_str)));
        if !private {
            if prefs.bittorrent.add_trackers_enabled {
                let user = trackers::parse_tracker_entries(&prefs.bittorrent.add_trackers);
                trackers::append_after_last_tier(&mut e.trackers, &user);
                e.trust_urls(user.iter().map(|t| t.url.as_str()));
            }
            if prefs.bittorrent.add_trackers_from_url_enabled {
                let from_url = self.misc.lock().trackers_from_url.clone();
                trackers::append_after_last_tier(&mut e.trackers, &from_url);
            }
        }

        // Persist before registering so a crash cannot lose the torrent.
        let backup = self.backup.clone();
        let record = e.to_resume();
        let tb = torrent_bytes.clone();
        let save = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            if let Some(b) = tb {
                backup.save_torrent_file(&record.info_hash, &b)?;
            }
            backup.save_resume(&record)
        })
        .await
        .map_err(|e| QbtError::io(e.to_string()))?;
        if let Err(err) = save {
            self.ctx.logger.critical(format!("Couldn't save torrent resume data. Error: {err}."));
            return Err(QbtError::io(err.to_string()));
        }

        let name = e.display_name();
        {
            let mut st = self.state.write();
            if st.torrents.contains_key(&id) {
                return Err(QbtError::conflict("This torrent is already in the transfer list."));
            }
            if !e.seed_status {
                if add_to_top {
                    for other in st.torrents.values_mut() {
                        if other.queue_position >= 0 {
                            other.queue_position += 1;
                            other.rt.dirty = true;
                        }
                    }
                    e.queue_position = 0;
                } else {
                    e.queue_position = st.torrents.values().map(|t| t.queue_position).max().unwrap_or(-1).max(-1) + 1;
                }
            }
            st.torrents.insert(id.clone(), e);
        }
        self.ctx.logger.info(format!("Added new torrent. Torrent: \"{name}\""));
        self.emit(CoreEvent::TorrentAdded { hash: id.clone(), name: name.clone() });
        if let Some(bytes) = &torrent_bytes {
            self.cache_metadata(&id, bytes.clone());
            self.export_to(&prefs.downloads.export_dir, &id, &name).await;
        }
        if prefs.downloads.autorun_on_torrent_added_enabled {
            self.run_external_program(&prefs.downloads.autorun_on_torrent_added_program, &id);
        }
        self.update_queue();
        self.reconcile(Some(std::slice::from_ref(&id))).await;
        Ok(id)
    }

    /// `SessionImpl::findTorrent`: the torrent with the descriptor's id or,
    /// for a hybrid torrent, the one known under its other hash: a hybrid
    /// added by its v1 hash (btih magnet still without metadata), or the
    /// hybrid whose v1 hash is the id of a v1-only descriptor.
    fn find_existing(&self, desc: &Descriptor) -> Option<String> {
        let id = desc.id();
        let st = self.state.read();
        if st.torrents.contains_key(&id) {
            return Some(id);
        }
        match desc.hybrid_v1() {
            Some(v1) => st.torrents.contains_key(&v1).then_some(v1),
            None => st.torrents.iter().find(|(_, e)| e.alt_id().as_deref() == Some(id.as_str())).map(|(k, _)| k.clone()),
        }
    }

    async fn handle_duplicate(&self, id: &str, desc: Descriptor, quiet: bool) -> QbtResult<String> {
        let prefs = self.ctx.prefs.current();
        let (new_trackers, new_seeds, new_private, new_meta) = match &desc {
            Descriptor::Meta { parsed, bytes } => (
                parsed.meta.trackers.clone(),
                parsed.meta.url_seeds.clone(),
                parsed.meta.private,
                // A .torrent with every piece layer completes a torrent
                // still fetching them.
                (!parsed.meta.missing_piece_layers).then(|| bytes.clone()),
            ),
            Descriptor::Magnet(m) => (m.trackers.clone(), m.url_seeds.clone(), false, None),
        };
        let needs_meta =
            self.state.read().torrents.get(id).is_some_and(|e| !e.has_metadata() || e.needs_piece_layers());
        let mut id = id.to_string();
        if let (true, Some(bytes)) = (needs_meta, new_meta) {
            let layers = librqbit::torrent_from_bytes(&bytes)
                .ok()
                .and_then(|t| t.piece_layers.map(|l| l.0.as_ref().to_vec()));
            // Metadata of a hybrid added by its v1 hash changes its id.
            if let Some(new_id) = self.on_metadata_received(&id, bytes.as_ref().clone(), Vec::new()).await {
                id = new_id;
            }
            // A pure v2 torrent running without some piece layers takes
            // them at once (each checked against its file's root).
            let handle = self.state.read().torrents.get(&id).and_then(|e| e.rt.engine.as_ref().map(|(h, _)| h.clone()));
            if let (Some(layers), Some(h)) = (layers, handle)
                && let Err(e) = h.add_piece_layers(&layers)
            {
                self.ctx.logger.warning(format!("Couldn't use the piece layers of the added torrent. Torrent: \"{id}\". Error: {e:#}"));
            }
        }
        let id = id.as_str();
        let (message, merged, name) = {
            let mut st = self.state.write();
            let Some(e) = st.torrents.get_mut(id) else { return Err(QbtError::not_found("torrent was removed")) };
            let name = e.display_name();
            if !prefs.downloads.merge_trackers {
                ("Merging of trackers is disabled", false, name)
            } else if e.is_private() || new_private {
                ("Trackers cannot be merged because it is a private torrent", false, name)
            } else {
                trackers::add_trackers(&mut e.trackers, &new_trackers);
                for s in new_seeds {
                    if !e.url_seeds.contains(&s) {
                        e.url_seeds.push(s);
                    }
                }
                e.rt.dirty = true;
                ("Trackers are merged from new source", true, name)
            }
        };
        if !quiet {
            self.ctx.logger.info(format!(
                "Detected an attempt to add a duplicate torrent. Existing torrent: \"{name}\". Torrent infohash: {id}. Result: {message}"
            ));
        }
        self.emit(CoreEvent::DuplicateTorrent { hash: id.to_string(), name, merged_trackers: merged });
        Err(QbtError::conflict(format!("This torrent is already in the transfer list. {message}")))
    }

    /// Copies the .torrent of a torrent to an export folder
    /// (`<name>.torrent`, then `<name> (N).torrent`).
    pub(super) async fn export_to(&self, dir: &str, id: &str, name: &str) {
        let dir = dir.trim();
        if dir.is_empty() {
            return;
        }
        let Ok(bytes) = self.export_torrent(id) else { return };
        let dir = paths::expand_home(dir);
        let base = paths::to_valid_file_name(name);
        let display = name.to_string();
        let logger = self.ctx.logger.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let result = (|| -> std::io::Result<()> {
                std::fs::create_dir_all(&dir)?;
                for n in 0..1000 {
                    let file = if n == 0 { format!("{base}.torrent") } else { format!("{base} ({n}).torrent") };
                    let path = Path::new(&dir).join(file);
                    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                        Ok(mut f) => {
                            use std::io::Write;
                            return f.write_all(&bytes);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                        Err(e) => return Err(e),
                    }
                }
                Err(std::io::Error::other("too many files with the same name"))
            })();
            if let Err(e) = result {
                logger.warning(format!(
                    "Failed to export torrent. Torrent: \"{display}\". Destination: \"{dir}\". Reason: \"{e}\""
                ));
            }
        })
        .await;
    }

    /// Runs the external program for a torrent (added/finished hooks).
    pub(super) fn run_external_program(&self, template: &str, id: &str) {
        let template = template.trim();
        if template.is_empty() {
            return;
        }
        let prefs = self.ctx.prefs.current();
        let Some((vars, name)) = self.with_torrent(id, |e, env| (autorun_vars(e, env), e.display_name())).ok() else {
            return;
        };
        let argv = autorun::build_argv(template, &vars);
        let shown = autorun::replace_vars(template, &vars);
        let _ = prefs;
        let _guard = self.ctx.rt.enter();
        if autorun::spawn(&argv) {
            self.ctx.logger.info(format!("Running external program. Torrent: \"{name}\". Command: `{shown}`"));
        } else {
            self.ctx.logger.warning(format!("Failed to run external program. Torrent: \"{name}\". Command: `{shown}`"));
        }
    }

    // ---- Magnet metadata -------------------------------------------------------

    pub(super) fn start_metadata_task(&self, id: &str) {
        // A paused session fetches nothing; `reconcile` restarts the task on resume.
        if self.is_paused() {
            return;
        }
        let Some(me) = self.arc() else { return };
        let id_owned = id.to_string();
        let handle = self.ctx.rt.spawn(async move { me.metadata_loop(id_owned).await });
        match self.state.write().torrents.get_mut(id) {
            Some(e) => e.rt.metadata_task = Some(handle.abort_handle()),
            None => handle.abort(),
        }
    }

    async fn metadata_loop(self: Arc<Self>, id: String) {
        let mut logged = false;
        loop {
            if self.is_paused() {
                if let Some(e) = self.state.write().torrents.get_mut(&id) {
                    e.rt.metadata_task = None;
                }
                return;
            }
            let Some((magnet, peers, disabled)) = self.state.read().torrents.get(&id).and_then(|e| {
                (!e.has_metadata()).then(|| {
                    (
                        {
                            let (v1, v2) = e.info_hashes();
                            metainfo::build_magnet(&v1, &v2, "", None, &e.trackers, &[])
                        },
                        e.peers.clone(),
                        librqbit::DisabledDiscovery { dht: e.disable_dht, pex: e.disable_pex, lsd: e.disable_lsd },
                    )
                })
            }) else {
                return;
            };
            if let Some(engine) = self.engine() {
                let trackers = super::engine::MetadataTrackers::default();
                if let Some(e) = self.state.write().torrents.get_mut(&id) {
                    e.rt.metadata_trackers = Some(trackers.clone());
                }
                match engine.resolve_metadata(&magnet, peers, disabled, trackers).await {
                    Ok(r) => {
                        self.on_metadata_received(&id, r.torrent_bytes, r.seen_peers).await;
                        return;
                    }
                    Err(err) => {
                        if !logged {
                            self.ctx.logger.warning(format!("Failed to download torrent metadata. Reason: \"{err:#}\""));
                            logged = true;
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    }

    /// Piece layers the engine fetched while torrents ran (hybrids added by
    /// a magnet link, all at once; pure v2 torrents whose .torrent lacked
    /// some, file by file): saves them into `BT_backup/<id>.torrent`
    /// (export and later sessions keep them, and serve hash requests).
    pub(super) async fn save_fetched_piece_layers(&self) {
        let fetched: Vec<(String, bytes::Bytes, bool)> = {
            let st = self.state.read();
            st.torrents
                .iter()
                .filter_map(|(id, e)| {
                    let h = &e.rt.engine.as_ref()?.0;
                    let layers = h.take_fetched_piece_layers()?;
                    Some((id.clone(), layers, h.missing_piece_layers() == 0))
                })
                .collect()
        };
        for (id, layers, complete) in fetched {
            let backup = self.backup.clone();
            let (bid, limit) = (id.clone(), self.torrent_file_limit());
            let saved = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
                let old = backup.load_torrent_file(&bid, limit).map_err(|e| e.to_string())?;
                let new = librqbit::torrent_with_piece_layers(&old, &layers).map_err(|e| e.to_string())?;
                backup.save_torrent_file(&bid, &new).map_err(|e| e.to_string())?;
                Ok(new)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            match saved {
                Ok(bytes) => {
                    if self.cached_metadata(&id).is_some() {
                        self.cache_metadata(&id, Arc::new(bytes));
                    }
                    if complete
                        && let Some(e) = self.state.write().torrents.get_mut(&id)
                        && let Some(m) = e.meta.as_mut().filter(|m| m.missing_piece_layers)
                    {
                        Arc::make_mut(m).missing_piece_layers = false;
                    }
                }
                Err(e) => self.ctx.logger.warning(format!("Couldn't save torrent metadata. Torrent: \"{id}\". Error: {e}")),
            }
        }
    }

    /// Metadata arrived for a magnet torrent (`MaintenanceJob::HandleMetadata`).
    /// A hybrid torrent added by its v1 hash takes its v2-based id now
    /// (`SessionImpl::handleTorrentInfoHashChanged`): its resume data moves
    /// to the new id. Returns the torrent's id when the metadata was taken.
    pub(super) async fn on_metadata_received(&self, id: &str, bytes: Vec<u8>, seen_peers: Vec<std::net::SocketAddr>) -> Option<String> {
        let parsed = match metainfo::parse_torrent(&bytes, self.limits()) {
            Ok(p) if p.meta.missing_piece_layers => {
                self.ctx.logger.warning(format!("Failed to download torrent metadata. Reason: \"missing piece layers\" ({id})"));
                return None;
            }
            Ok(p) if p.meta.id() == id || p.meta.alt_id().as_deref() == Some(id) => p,
            Ok(_) => {
                self.ctx.logger.warning(format!("Failed to download torrent metadata. Reason: \"info hash mismatch\" ({id})"));
                return None;
            }
            Err(e) => {
                self.ctx.logger.warning(format!("Failed to download torrent metadata. Reason: \"{e}\""));
                return None;
            }
        };
        let new_id = parsed.meta.id();
        if new_id != id && self.state.read().torrents.contains_key(&new_id) {
            self.ctx.logger.warning(format!(
                "Failed to download torrent metadata. Reason: \"the torrent is already in the transfer list as {new_id}\" ({id})"
            ));
            return None;
        }
        let bytes = Arc::new(bytes);
        let backup = self.backup.clone();
        let (bid, b) = (new_id.clone(), bytes.clone());
        let saved = tokio::task::spawn_blocking(move || backup.save_torrent_file(&bid, &b)).await;
        if !matches!(saved, Ok(Ok(()))) {
            self.ctx.logger.critical(format!("Couldn't save torrent metadata. Torrent: \"{new_id}\"."));
            return None;
        }
        self.cache_metadata(&new_id, bytes);
        let prefs = self.ctx.prefs.current();
        let meta = Arc::new(parsed.meta);
        let logger = self.ctx.logger.clone();
        let mut moved = None;
        let only_layers;
        let name = {
            let mut st = self.state.write();
            if st.torrents.get(id).is_none_or(|e| e.has_metadata() && !e.needs_piece_layers())
                || (new_id != id && st.torrents.contains_key(&new_id))
            {
                return None;
            }
            only_layers = st.torrents.get(id).is_some_and(|e| e.has_metadata());
            if new_id != id {
                let mut e = st.torrents.remove(id)?;
                e.id = new_id.clone();
                st.torrents.insert(new_id.clone(), e);
            }
            let e = st.torrents.get_mut(&new_id)?;
            if e.file_paths.len() != meta.files.len() {
                e.file_paths = paths::apply_content_layout(&meta.file_paths(), e.content_layout);
            }
            if e.file_priorities.len() != meta.files.len() {
                e.file_priorities = default_priorities(&meta, &e.file_paths, &prefs);
            }
            trackers::add_trackers(&mut e.trackers, &meta.trackers);
            for s in &meta.url_seeds {
                if !e.url_seeds.contains(s) {
                    e.url_seeds.push(s.clone());
                }
            }
            for p in seen_peers.into_iter().take(50) {
                if !e.peers.contains(&p) {
                    e.peers.push(p);
                }
            }
            e.meta = Some(meta);
            e.magnet_v1.clear();
            e.rt.metadata_task = None;
            e.rt.dirty = true;
            if new_id != id {
                moved = Some(e.to_resume());
            }
            if e.stop_condition == StopCondition::MetadataReceived {
                e.stop_condition = StopCondition::None;
                e.stopped = true;
                logger.info(format!("Torrent stopped. Torrent: \"{}\"", e.display_name()));
            }
            e.display_name()
        };
        if let Some(record) = moved {
            // The new resume file first, then the old files go: a crash in
            // between leaves both, and loading migrates the old one again.
            let backup = self.backup.clone();
            let old = id.to_string();
            let _ = tokio::task::spawn_blocking(move || {
                let _ = backup.save_resume(&record);
                backup.delete(&old);
            })
            .await;
        }
        let id = new_id.as_str();
        if !only_layers {
            self.emit(CoreEvent::TorrentMetadataReceived { hash: id.to_string(), name: name.clone() });
            self.export_to(&prefs.downloads.export_dir, id, &name).await;
        }
        self.reconcile(Some(&[id.to_string()])).await;
        Some(new_id)
    }

    /// Add dialog: parses a file/URL or fetches a magnet's metadata from
    /// the swarm without adding the torrent.
    pub async fn fetch_metadata(&self, source: TorrentSource) -> QbtResult<TorrentMetadataInfo> {
        let desc = self.load_source(&source, None).await.map_err(QbtError::invalid)?;
        match desc {
            Descriptor::Meta { bytes, parsed } => {
                let id = parsed.meta.id();
                self.cache_metadata(&id, bytes);
                Ok(self.metadata_info(&parsed.meta))
            }
            Descriptor::Magnet(link) => {
                let id = metainfo::hex(&link.info_hash);
                if let Ok(bytes) = self.fetched_metadata_torrent(&id)
                    && let Ok(Descriptor::Meta { parsed, .. }) = self.parse_descriptor(Arc::new(bytes))
                {
                    return Ok(self.metadata_info(&parsed.meta));
                }
                let engine = self.engine().ok_or_else(|| QbtError::engine("the BitTorrent engine is not running"))?;
                let prefs = self.ctx.prefs.current();
                let mut list = link.trackers.clone();
                if prefs.bittorrent.add_trackers_enabled {
                    trackers::append_after_last_tier(&mut list, &trackers::parse_tracker_entries(&prefs.bittorrent.add_trackers));
                }
                let v2 = link.info_hash_v2.map(|h| metainfo::hex(&h)).unwrap_or_default();
                let v1 = link.info_hash_v1.map(|h| metainfo::hex(&h)).unwrap_or_default();
                let magnet = metainfo::build_magnet(&v1, &v2, "", None, &list, &[]);
                let r = engine.resolve_metadata(&magnet, link.peers.clone(), Default::default(), Default::default()).await.map_err(|e| {
                    let msg = format!("{e:#}");
                    self.ctx.logger.warning(format!("Failed to download torrent metadata. Reason: \"{msg}\""));
                    QbtError::network(msg)
                })?;
                match self.parse_descriptor(Arc::new(r.torrent_bytes)).map_err(QbtError::invalid)? {
                    Descriptor::Meta { bytes, parsed } if parsed.meta.id() == id || parsed.meta.alt_id() == Some(id.clone()) => {
                        // A btih link of a hybrid torrent: known under both ids.
                        let real = parsed.meta.id();
                        if real != id {
                            self.cache_metadata(&real, bytes.clone());
                        }
                        self.cache_metadata(&id, bytes);
                        Ok(self.metadata_info(&parsed.meta))
                    }
                    _ => Err(QbtError::other("metadata does not match the info hash")),
                }
            }
        }
    }
}

/// Default file priorities: Normal, with excluded file names ignored.
pub(super) fn default_priorities(meta: &TorrentMeta, file_paths: &[String], prefs: &Preferences) -> Vec<FilePriority> {
    let d = &prefs.downloads;
    (0..meta.files.len())
        .map(|i| {
            let path = file_paths.get(i).map(String::as_str).unwrap_or(&meta.files[i].path);
            if d.excluded_file_names_enabled && paths::is_excluded(path, &d.excluded_file_names) {
                FilePriority::Ignored
            } else {
                FilePriority::Normal
            }
        })
        .collect()
}

/// Save path / download path / ATM resolution at add time
/// (`initLoadTorrentParams`).
pub(super) fn resolve_paths(e: &mut TorrentEntry, params: &AddTorrentParams, prefs: &Preferences, cats: &CategoryMap) {
    let g = torrent::global_paths(prefs);
    let save = params.save_path.as_deref().map(str::trim).unwrap_or("");
    let dl = params.download_path.as_deref().map(str::trim).unwrap_or("");
    let auto_default = prefs.downloads.auto_tmm_enabled;
    let manual_cat = prefs.downloads.use_category_paths_in_manual_mode;
    e.auto_tmm = params.auto_tmm.unwrap_or(save.is_empty() && dl.is_empty() && auto_default);
    if e.auto_tmm {
        e.save_path.clear();
        e.download_path.clear();
        return;
    }
    let default_save = categories::suggested_save_path(cats, &e.category, params.auto_tmm, auto_default, manual_cat, &g);
    let default_dl = categories::suggested_download_path(cats, &e.category, params.auto_tmm, auto_default, manual_cat, &g);
    e.save_path = paths::resolve(&default_save, save);
    let use_dl = params.use_download_path.unwrap_or(!dl.is_empty() || g.download_path_enabled);
    e.download_path = if use_dl {
        let base = if default_dl.is_empty() { g.download_path.clone() } else { default_dl };
        paths::resolve(&base, dl)
    } else {
        String::new()
    };
}

pub(super) fn autorun_vars(e: &TorrentEntry, env: &Env) -> autorun::AutorunVars {
    autorun::AutorunVars {
        name: e.display_name(),
        category: e.category.clone(),
        tags: e.tags.clone(),
        content_path: e.content_path(env),
        root_path: e.root_path(env),
        save_path: e.save_path(env),
        files_count: e.meta.as_ref().map(|m| m.files.len() as i64).unwrap_or(0),
        total_size: e.total_size(),
        current_tracker: e.current_tracker(),
        info_hash_v1: e.info_hashes().0,
        info_hash_v2: e.info_hashes().1,
        id: e.id.clone(),
        comment: e.comment(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Category, CategoryDownloadPath};

    fn prefs() -> Preferences {
        let mut p = Preferences::default();
        p.downloads.save_path = "/dl".into();
        p.downloads.temp_path = "/dl/temp".into();
        p
    }

    fn cats() -> CategoryMap {
        let mut c = CategoryMap::new();
        c.insert("Movies".into(), Category { name: "Movies".into(), save_path: "/media/movies".into(), ..Default::default() });
        c.insert(
            "TV".into(),
            Category { name: "TV".into(), download_path: CategoryDownloadPath::Path { path: "/inc/tv".into() }, ..Default::default() },
        );
        c
    }

    fn entry(cat: &str) -> TorrentEntry {
        let mut e = TorrentEntry::new("ab".repeat(20));
        e.category = cat.into();
        e
    }

    #[test]
    fn magnet_trackers_and_web_seeds_join_resolved_metadata() {
        let bytes = metainfo::tests::make_torrent("T", &[(&["a.bin"], 100)], 16, &[]);
        let mut meta = metainfo::parse_torrent(&bytes, Default::default()).unwrap().meta;
        let link = metainfo::parse_magnet(
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&tr=udp%3A%2F%2Ft%3A1&ws=https%3A%2F%2Fseed.example%2Ff%2F",
        )
        .unwrap();
        merge_magnet_sources(&mut meta, &link);
        merge_magnet_sources(&mut meta, &link);
        assert_eq!(meta.url_seeds, vec!["https://seed.example/f/".to_string()]);
        assert_eq!(meta.trackers.iter().filter(|t| t.url == "udp://t:1").count(), 1);
    }

    #[test]
    fn manual_mode_paths() {
        let p = prefs();
        let mut e = entry("Movies");
        resolve_paths(&mut e, &AddTorrentParams::default(), &p, &cats());
        assert!(!e.auto_tmm);
        assert_eq!(e.save_path, "/dl");
        assert_eq!(e.download_path, "");

        let mut e = entry("Movies");
        let params = AddTorrentParams { save_path: Some("sub".into()), ..Default::default() };
        resolve_paths(&mut e, &params, &p, &cats());
        assert_eq!(e.save_path, "/dl/sub");

        // Relative paths resolve against the category path in manual mode
        // when "Use Category paths in Manual Mode" is on.
        let mut p2 = prefs();
        p2.downloads.use_category_paths_in_manual_mode = true;
        let mut e = entry("Movies");
        resolve_paths(&mut e, &params, &p2, &cats());
        assert_eq!(e.save_path, "/media/movies/sub");

        let mut e = entry("TV");
        let params = AddTorrentParams { save_path: Some("/abs".into()), download_path: Some("x".into()), ..Default::default() };
        resolve_paths(&mut e, &params, &p2, &cats());
        assert_eq!(e.save_path, "/abs");
        assert_eq!(e.download_path, "/inc/tv/x");
    }

    #[test]
    fn automatic_mode_by_default() {
        let mut p = prefs();
        p.downloads.auto_tmm_enabled = true;
        let mut e = entry("Movies");
        resolve_paths(&mut e, &AddTorrentParams::default(), &p, &cats());
        assert!(e.auto_tmm);
        // An explicit path disables the automatic default.
        let mut e = entry("Movies");
        resolve_paths(&mut e, &AddTorrentParams { save_path: Some("/x".into()), ..Default::default() }, &p, &cats());
        assert!(!e.auto_tmm);
        // Global "keep incomplete in" applies to manual torrents.
        let mut p = prefs();
        p.downloads.temp_path_enabled = true;
        let mut e = entry("");
        resolve_paths(&mut e, &AddTorrentParams::default(), &p, &cats());
        assert_eq!(e.download_path, "/dl/temp");
        let mut e = entry("");
        resolve_paths(&mut e, &AddTorrentParams { use_download_path: Some(false), ..Default::default() }, &p, &cats());
        assert_eq!(e.download_path, "");
    }
}
